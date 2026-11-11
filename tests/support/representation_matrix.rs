//! Sequential research kernel. All callers use synthetic stages; no live entrypoint.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use docsbase_memory::daemon::tools::search_docs_with;
use docsbase_memory::hybrid::{Reranker, blocking, dense::DenseCache};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};

use super::{
    CONTEXT, RAW,
    bench::{self, BenchEnv, ConfigId, QueryCase, VectorFixture},
    digest, input_key, prepare_with_pins,
};

pub struct MatrixStages {
    pub embedder: Arc<dyn Embedder>,
    pub reranker: Arc<dyn Reranker>,
    pub rerank_pin: String,
}

#[derive(Clone, serde::Deserialize)]
struct ChunkRecipe {
    raw_input: String,
    context_input: String,
    raw_input_hash: String,
}
impl ChunkRecipe {
    fn text(&self, recipe: &str) -> &str {
        if recipe == RAW {
            &self.raw_input
        } else {
            &self.context_input
        }
    }
}

type Scores = Vec<(usize, f32)>;
#[derive(Clone)]
struct Recording {
    pool: Value,
    scores: Scores,
}
#[derive(Default)]
struct Memo {
    records: Mutex<BTreeMap<String, Recording>>,
}
struct Transform {
    inner: Arc<dyn Reranker>,
    pin: String,
    recipe: &'static str,
    inputs: Arc<BTreeMap<String, ChunkRecipe>>,
    memo: Arc<Memo>,
    seen: Mutex<Option<(Value, bool)>>,
}
impl Transform {
    fn score(&self, query: &str, docs: &[String], top_n: usize) -> anyhow::Result<Scores> {
        anyhow::ensure!(
            !docs.is_empty() && top_n == docs.len(),
            "invalid matrix pool size"
        );
        let inputs = docs
            .iter()
            .map(|body| {
                self.inputs
                    .get(body)
                    .map(|row| row.text(self.recipe).to_owned())
                    .context("unmapped matrix candidate")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let keys = inputs
            .iter()
            .map(|text| input_key("rerank-document", &self.pin, self.recipe, text))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let key = digest(&(query, &self.pin, self.recipe, &keys, top_n))?;
        // The guard must be dropped before invoking the injected stage.
        let cached = self
            .memo
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("matrix memo poisoned"))?
            .get(&key)
            .cloned();
        let hit = cached.is_some();
        let recording = if let Some(cached) = cached {
            cached
        } else {
            let scores = self.inner.rerank(query, &inputs, top_n)?;
            let indices: BTreeSet<_> = scores.iter().map(|(i, _)| *i).collect();
            anyhow::ensure!(
                scores.len() == docs.len()
                    && indices.len() == docs.len()
                    && scores.iter().all(|(i, s)| *i < docs.len() && s.is_finite()),
                "incomplete or invalid matrix rerank results"
            );
            let recording = Recording {
                pool: json!({"key":key,"query":query,"model_pin":self.pin,
                "recipe":self.recipe,"top_n":top_n,"keys":keys,"inputs":inputs,
                "input_bytes":inputs.iter().map(String::len).sum::<usize>(),"scores":scores}),
                scores,
            };
            self.memo
                .records
                .lock()
                .map_err(|_| anyhow::anyhow!("matrix memo poisoned"))?
                .insert(key, recording.clone());
            recording
        };
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| anyhow::anyhow!("matrix capture poisoned"))?;
        anyhow::ensure!(seen.is_none(), "unexpected repeated matrix capture");
        *seen = Some((recording.pool, hit));
        Ok(recording.scores)
    }
    fn take(&self) -> anyhow::Result<Option<(Value, bool)>> {
        Ok(self
            .seen
            .lock()
            .map_err(|_| anyhow::anyhow!("matrix capture poisoned"))?
            .take())
    }
}
impl Reranker for Transform {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Scores> {
        self.score(query, docs, top_n)
            .map_err(|e| docsbase_memory::error::Error::internal(format!("matrix rerank: {e:#}")))
    }
}

fn validate_vector(vector: &[f32], dim: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        vector.len() == dim && vector.iter().all(|v| v.is_finite()),
        "invalid matrix vector dimension/finiteness"
    );
    Ok(())
}

/// No client/key lookup or network entrypoint. Trait calls are not wire-request counts.
#[allow(clippy::too_many_lines)] // sequential factorial orchestration, not a second search implementation
pub fn run_matrix(
    env: &BenchEnv,
    cases: &[QueryCase],
    stages: &MatrixStages,
    corpus_hash: &str,
    revision: &str,
) -> anyhow::Result<Value> {
    let base_pin = stages.embedder.model_id();
    let bench::ModelPin::Embedding {
        dim,
        provider,
        model,
        ..
    } = bench::parse_model_pin(base_pin)?
    else {
        anyhow::bail!("matrix embedding pin required");
    };
    let dim = usize::try_from(dim)?;
    anyhow::ensure!(
        dim > 0 && !provider.trim().is_empty() && !model.trim().is_empty(),
        "valid matrix embedding pin required"
    );
    let bench::ModelPin::Rerank { provider, model } = bench::parse_model_pin(&stages.rerank_pin)?
    else {
        anyhow::bail!("matrix reranker pin required");
    };
    anyhow::ensure!(
        !provider.trim().is_empty() && !model.trim().is_empty(),
        "valid matrix reranker pin required"
    );
    anyhow::ensure!(
        cases
            .iter()
            .all(|c| c.k > 0 && c.k <= bench::RERANK_WINDOW as usize),
        "matrix k outside candidate window"
    );
    let mut report = prepare_with_pins(
        env,
        cases,
        corpus_hash,
        revision,
        base_pin,
        &stages.rerank_pin,
    )?;
    let mut inputs = BTreeMap::<String, ChunkRecipe>::new();
    for value in report["chunks"].as_array().context("matrix chunks")? {
        let row: ChunkRecipe = serde_json::from_value(value.clone())?;
        if let Some(previous) = inputs.insert(row.raw_input.clone(), row.clone()) {
            anyhow::ensure!(
                previous.context_input == row.context_input,
                "ambiguous body/context mapping; occurrence-qualified production representation required"
            );
        }
    }
    let inputs = Arc::new(inputs);
    // All metadata guards above run before the first injected method call.
    let mut banks = Vec::new();
    for recipe in [RAW, CONTEXT] {
        let texts: Vec<_> = inputs
            .values()
            .map(|row| row.text(recipe).to_owned())
            .collect();
        let vectors = blocking::enter(|| stages.embedder.embed(&texts))?;
        anyhow::ensure!(
            vectors.len() == texts.len(),
            "matrix document vector cardinality mismatch"
        );
        for vector in &vectors {
            validate_vector(vector, dim)?;
        }
        let namespace = format!(
            "{base_pin}@research-{}",
            digest(&(recipe, &report["chunk_set_key"]))?
        );
        let vectors = inputs
            .values()
            .zip(vectors)
            .map(|(row, vector)| (row.raw_input_hash.clone(), vector))
            .collect();
        banks.push((recipe, namespace, vectors));
    }
    let queries: BTreeSet<_> = cases.iter().map(|c| c.query.as_str()).collect();
    let mut query_vectors = std::collections::HashMap::new();
    for query in &queries {
        let vector = blocking::enter(|| stages.embedder.embed_query(query))?;
        validate_vector(&vector, dim)?;
        query_vectors.insert(blake3::hash(query.as_bytes()).to_hex().to_string(), vector);
    }
    let memo = Arc::new(Memo::default());
    let mut runs = Vec::new();
    let mut bank_reports = Vec::new();
    for (document_recipe, namespace, vectors) in banks {
        let bank = Arc::new(VectorFixture {
            model_pin: namespace.clone(),
            dim,
            corpus_hash: corpus_hash.into(),
            queries_hash: report["queries_hash"]
                .as_str()
                .context("matrix query hash")?
                .into(),
            vectors,
            query_vectors: Some(query_vectors.clone()),
        });
        // No bank is published until both document banks and every query validate.
        bench::preload_vectors(env, &bank)?;
        bank_reports.push(json!({"recipe":document_recipe,"cache_namespace":namespace,
            "vector_hash":digest(&bank.vectors.iter().collect::<BTreeMap<_,_>>())?}));
        let embedder = Arc::new(bench::ReplayEmbedder {
            pin: namespace.clone(),
            fixture: bank,
        });
        let dense = DenseCache::new();
        for rerank_recipe in [RAW, CONTEXT] {
            for case in cases {
                for config in [ConfigId::Bm25Rerank, ConfigId::RrfRerank] {
                    let reranker = Arc::new(Transform {
                        inner: stages.reranker.clone(),
                        pin: stages.rerank_pin.clone(),
                        recipe: rerank_recipe,
                        inputs: inputs.clone(),
                        memo: memo.clone(),
                        seen: Mutex::new(None),
                    });
                    let hybrid =
                        bench::stages_for(config, Some(embedder.clone()), Some(reranker.clone()));
                    let search = || {
                        search_docs_with(
                            &hybrid,
                            &dense,
                            &env.db,
                            env.cache.path(),
                            &env.project,
                            &json!({"query":case.query,"limit":case.k}),
                            bench::RERANK_WINDOW,
                        )
                    };
                    let response = search()?;
                    let pool = reranker.take()?;
                    let warm = search()?;
                    let warm_pool = reranker.take()?;
                    anyhow::ensure!(response == warm, "matrix warm result mismatch");
                    match (&pool, &warm_pool) {
                        (Some((p, _)), Some((w, true))) if p == w => {}
                        (None, None) if response.as_array().is_some_and(Vec::is_empty) => {}
                        _ => anyhow::bail!("matrix warm pool/reuse mismatch"),
                    }
                    runs.push(json!({"case_id":case.id,"source":format!("{config:?}"),"requested_k":case.k,
                        "document_recipe":document_recipe,"rerank_recipe":rerank_recipe,"cache_namespace":namespace,
                        "pool":pool.map(|(value,_)|value),"citations":response,"warm_equal":true}));
                }
            }
        }
    }
    let rerank_calls = memo
        .records
        .lock()
        .map_err(|_| anyhow::anyhow!("matrix memo poisoned"))?
        .len();
    report
        .as_object_mut()
        .context("matrix report")?
        .remove("provider_requests");
    report["phase"] = "research-matrix-kernel".into();
    report["matrix_source_hash"] = digest(&include_str!("representation_matrix.rs"))?.into();
    report["method_calls"] = json!({"document":2,"query":queries.len(),"rerank":rerank_calls});
    report["wire_requests"] = Value::Null;
    report["quality"] = Value::Null;
    report["banks"] = bank_reports.into();
    report["query_bank_hash"] = digest(&query_vectors.iter().collect::<BTreeMap<_, _>>())?.into();
    report["runs"] = runs.into();
    report["limits"]["candidate_pools"] =
        "observed injected-stage pools; no quality inference".into();
    Ok(report)
}
