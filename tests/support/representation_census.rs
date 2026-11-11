//! Offline census of actual pre-provider candidate windows, never model scores.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use docsbase_memory::daemon::tools::search_docs_with;
use docsbase_memory::hybrid::{Reranker, config::RerankerCfg, dense::DenseCache};
use docsbase_memory::index::tantivy_index::chunk_id_parts;
use serde_json::{Value, json};

use super::{
    bench::{self, BenchEnv, ConfigId, QueryCase, VectorFixture},
    digest, prepare_with_pins,
};

struct Captured {
    query: String,
    docs: Vec<String>,
    top_n: usize,
}
#[derive(Default)]
struct Capture(Mutex<Option<Captured>>);
impl Reranker for Capture {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| docsbase_memory::error::Error::internal("census capture poisoned"))?;
        if slot.is_some() {
            return Err(docsbase_memory::error::Error::internal(
                "census reranker called twice",
            ));
        }
        *slot = Some(Captured {
            query: query.to_owned(),
            docs: docs.to_vec(),
            top_n,
        });
        Ok((0..docs.len()).map(|index| (index, 1.0)).collect())
    }
}

/// Uses only supplied replay vectors and identity capture; no live stage can be injected.
pub fn collect_candidates(
    env: &BenchEnv,
    cases: &[QueryCase],
    vectors: &VectorFixture,
    corpus_hash: &str,
    revision: &str,
) -> anyhow::Result<Value> {
    validate_vectors(cases, vectors)?;
    let rerank = RerankerCfg::default();
    let rerank_pin = format!("{}/{}", rerank.provider, rerank.model);
    let mut plan = prepare_with_pins(
        env,
        cases,
        corpus_hash,
        revision,
        &vectors.model_pin,
        &rerank_pin,
    )?;
    let chunks: BTreeMap<_, _> = plan["chunks"]
        .as_array()
        .context("preflight chunks")?
        .iter()
        .map(|row| {
            Ok((
                (
                    row["path"].as_str().context("chunk path")?.to_owned(),
                    row["seq"].as_u64().context("chunk seq")?,
                ),
                row.clone(),
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        chunks.len() == plan["chunks"].as_array().context("preflight chunks")?.len(),
        "ambiguous normalized census occurrence"
    );
    let paths = {
        let mut statement = env
            .db
            .connection()
            .prepare("SELECT id,rel_path FROM docs WHERE project_id=?1")?;
        let rows = statement.query_map([env.project.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?.replace('\\', "/"),
            ))
        })?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()?
    };
    bench::preload_vectors(env, vectors)?;
    let embedder = Arc::new(bench::ReplayEmbedder {
        pin: vectors.model_pin.clone(),
        fixture: Arc::new(vectors.clone()),
    });
    let dense = DenseCache::new();
    let mut pools = Vec::new();
    for case in cases {
        for cfg in [ConfigId::Bm25Rerank, ConfigId::RrfRerank] {
            let capture = Arc::new(Capture::default());
            let stages = bench::stages_for(cfg, Some(embedder.clone()), Some(capture.clone()));
            let response = search_docs_with(
                &stages,
                &dense,
                &env.db,
                env.cache.path(),
                &env.project,
                &json!({"query":case.query,"limit":bench::RERANK_WINDOW}),
                bench::RERANK_WINDOW,
            )
            .with_context(|| format!("census {cfg:?}/{}", case.id))?;
            let observed = capture
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("census capture poisoned"))?
                .take();
            pools.push(resolve_pool(
                case,
                cfg,
                &response,
                observed,
                &paths,
                &chunks,
                &rerank_pin,
            )?);
        }
    }
    plan["phase"] = "offline-candidate-census".into();
    plan["candidate_source"] = vector_provenance(vectors)?;
    plan["census_source_hash"] = digest(&include_str!("representation_census.rs"))?.into();
    plan["candidate_stats"] = pool_stats(&pools);
    plan["candidate_pools"] = pools.into();
    plan["limits"]["candidate_pools"] = "observed raw-vector source pools only".into();
    Ok(plan)
}

fn vector_provenance(vectors: &VectorFixture) -> anyhow::Result<Value> {
    let docs: BTreeMap<_, _> = vectors.vectors.iter().collect();
    let queries = vectors
        .query_vectors
        .as_ref()
        .map(|rows| rows.iter().collect::<BTreeMap<_, _>>());
    Ok(json!({
        "embedding_pin":vectors.model_pin,"vector_format":if vectors.query_vectors.is_some(){"v2-role-aware-raw"}else{"v1-legacy-role-unknown"},
        "vector_content_hash":digest(&(&vectors.model_pin,vectors.dim,docs,queries))?,
        "recorded_corpus_hash":vectors.corpus_hash,"recorded_queries_hash":vectors.queries_hash,
        "provenance":if vectors.query_vectors.is_some(){"explicit supplied raw replay vectors"}else{"historical vectors; query/document role provenance unknown; not a fresh raw control"},
        "context_embedding_pools":"not collected","rerank_scores":null,
        "rerank_mode":"identity capture; no model invoked",
    }))
}

fn validate_vectors(cases: &[QueryCase], vectors: &VectorFixture) -> anyhow::Result<()> {
    anyhow::ensure!(!cases.is_empty(), "census requires cases");
    let bench::ModelPin::Embedding { dim, .. } = bench::parse_model_pin(&vectors.model_pin)? else {
        anyhow::bail!("census needs an embedding pin");
    };
    anyhow::ensure!(
        vectors.dim > 0 && vectors.dim == usize::try_from(dim)?,
        "census vector dimension/pin mismatch"
    );
    for vector in vectors
        .vectors
        .values()
        .chain(vectors.query_vectors.iter().flat_map(|rows| rows.values()))
    {
        anyhow::ensure!(
            vector.len() == vectors.dim && vector.iter().all(|v| v.is_finite()),
            "invalid census vector"
        );
    }
    for case in cases {
        anyhow::ensure!(
            case.k > 0 && case.k <= bench::RERANK_WINDOW as usize,
            "census original k exceeds candidate window"
        );
        vectors.get_query(&case.query)?;
    }
    Ok(())
}

fn resolve_pool(
    case: &QueryCase,
    cfg: ConfigId,
    response: &Value,
    captured: Option<Captured>,
    paths: &BTreeMap<i64, String>,
    chunks: &BTreeMap<(String, u64), Value>,
    rerank_pin: &str,
) -> anyhow::Result<Value> {
    let rows = response
        .as_array()
        .context("census response must be citations")?;
    anyhow::ensure!(
        rows.len() <= bench::RERANK_WINDOW as usize,
        "census window exceeded"
    );
    let captured = match captured {
        None if rows.is_empty() => Captured {
            query: case.query.clone(),
            docs: Vec::new(),
            top_n: 0,
        },
        Some(_) if rows.is_empty() => {
            anyhow::bail!("empty census unexpectedly invoked reranker");
        }
        Some(captured) => captured,
        None => anyhow::bail!("nonempty census without rerank capture"),
    };
    anyhow::ensure!(
        captured.query == case.query
            && captured.top_n == rows.len()
            && captured.docs.len() == rows.len(),
        "census capture/citation count mismatch"
    );
    let mut candidates = Vec::new();
    for (row, body) in rows.iter().zip(&captured.docs) {
        let (doc, seq) = chunk_id_parts(row["chunk_id"].as_u64().context("census chunk_id")?);
        let path = paths.get(&doc).context("census chunk outside project")?;
        let input = chunks
            .get(&(path.clone(), u64::from(seq)))
            .context("missing census occurrence")?;
        anyhow::ensure!(
            row["path"] == input["path"]
                && row["heading_path"] == input["heading_path"]
                && row["lines"] == input["lines"]
                && input["raw_input"].as_str() == Some(body),
            "census citation/input order mismatch"
        );
        let mut candidate = input.clone();
        candidate["occurrence_key"] = digest(&(
            input["path"].clone(),
            input["seq"].clone(),
            input["heading_path"].clone(),
            input["lines"].clone(),
            input["raw_key"].clone(),
            input["context_key"].clone(),
        ))?
        .into();
        candidates.push(candidate);
    }
    let raw: Vec<_> = candidates.iter().map(|c| &c["raw_rerank_key"]).collect();
    let context: Vec<_> = candidates
        .iter()
        .map(|c| &c["context_rerank_key"])
        .collect();
    let bytes = |field: &str| {
        candidates
            .iter()
            .map(|c| c[field].as_str().map_or(0, str::len))
            .sum::<usize>()
    };
    Ok(
        json!({"case_id":case.id,"query":case.query,"source":format!("{cfg:?}"),"requested_k":case.k,
        "candidate_window":bench::RERANK_WINDOW,"rerank_call":!rows.is_empty(),"top_n":rows.len(),
        "raw_pool_key":digest(&(&case.query,rerank_pin,super::RAW,raw,rows.len()))?,
        "context_pool_key":digest(&(&case.query,rerank_pin,super::CONTEXT,context,rows.len()))?,
        "raw_bytes":bytes("raw_input"),"context_bytes":bytes("context_input"),"candidates":candidates}),
    )
}

fn pool_stats(pools: &[Value]) -> Value {
    let nonempty: Vec<_> = pools.iter().filter(|p| p["rerank_call"] == true).collect();
    let unique = |field: &str| {
        nonempty
            .iter()
            .map(|p| p[field].as_str())
            .collect::<BTreeSet<_>>()
            .len()
    };
    json!({"observations":pools.len(),"nonempty":nonempty.len(),"empty":pools.len()-nonempty.len(),
        "unique_raw_rerank_pools":unique("raw_pool_key"),"unique_context_rerank_pools":unique("context_pool_key"),
        "max_raw_bytes":pools.iter().filter_map(|p|p["raw_bytes"].as_u64()).max().unwrap_or(0),
        "max_context_bytes":pools.iter().filter_map(|p|p["context_bytes"].as_u64()).max().unwrap_or(0)})
}
