//! Exact offline input inventory; no provider is constructed by this module.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use serde::Serialize;
use serde_json::{Value, json};

use super::support::bench::{self, BenchEnv, QueryCase};
use docsbase_memory::hybrid::config::{EmbeddingCfg, RerankerCfg, embedding_model_id};
use docsbase_memory::index::tokenizer::TOKENIZER_VERSION;

const RAW: &str = "raw-body-v1";
const CONTEXT: &str = "path-heading-v1";
const QUERY: &str = "raw-query-v1";

fn digest(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn input_key(role: &str, model: &str, recipe: &str, input: &str) -> anyhow::Result<String> {
    digest(&(role, model, recipe, input))
}

/// Builds research identities, without changing production cache keys.
/// Location-dependent chunk membership is distinct from reusable vector blobs.
#[allow(clippy::too_many_lines)] // one serializable preflight, no execution path
pub fn prepare(
    env: &BenchEnv,
    cases: &[QueryCase],
    expected_corpus_hash: &str,
    revision: &str,
) -> anyhow::Result<Value> {
    let actual_hash = bench::corpus_hash(env.root.path())?;
    anyhow::ensure!(
        actual_hash == expected_corpus_hash,
        "indexed corpus hash mismatch"
    );
    anyhow::ensure!(!revision.trim().is_empty(), "source revision is required");
    anyhow::ensure!(!cases.is_empty(), "query cases are required");
    let mut case_ids = BTreeSet::new();
    for case in cases {
        anyhow::ensure!(case_ids.insert(&case.id), "duplicate case ID {}", case.id);
        anyhow::ensure!(!case.query.is_empty(), "empty query {}", case.id);
    }
    let embedding_pin = embedding_model_id(&EmbeddingCfg::default());
    let rerank_cfg = RerankerCfg::default();
    let rerank_pin = format!("{}/{}", rerank_cfg.provider, rerank_cfg.model);
    let mut statement = env.db.connection().prepare(
        "SELECT d.rel_path,c.seq,c.heading_path,c.kind,c.line_start,c.line_end,c.text \
         FROM chunks c JOIN docs d ON d.id=c.doc_id WHERE d.project_id=?1 \
         ORDER BY d.rel_path,c.seq",
    )?;
    let rows = statement.query_map([env.project.id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u32>(4)?,
            row.get::<_, u32>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut chunks = Vec::new();
    let mut documents = BTreeSet::new();
    let mut unique_raw = BTreeSet::new();
    let mut unique_context = BTreeSet::new();
    let mut raw_bytes = 0;
    let mut context_bytes = 0;
    let mut locations = Vec::new();
    for row in rows {
        let (path, seq, heading, kind, line_start, line_end, body) = row?;
        let path = path.replace('\\', "/");
        documents.insert(path.clone());
        let context = format!("Path: {path}\nHeading: {heading}\n\n{body}");
        let raw_key = input_key("document", &embedding_pin, RAW, &body)?;
        let context_key = input_key("document", &embedding_pin, CONTEXT, &context)?;
        raw_bytes += body.len();
        context_bytes += context.len();
        unique_raw.insert(raw_key.clone());
        unique_context.insert(context_key.clone());
        locations.push(json!([
            path,
            seq,
            heading,
            kind,
            line_start,
            line_end,
            raw_key,
            context_key
        ]));
        chunks.push(json!({
            "path":path, "seq":seq, "heading_path":heading, "kind":kind,
            "lines":[line_start,line_end], "raw_input":body, "context_input":context,
            "raw_input_hash":blake3::hash(body.as_bytes()).to_hex().to_string(),
            "context_input_hash":blake3::hash(context.as_bytes()).to_hex().to_string(),
            "raw_key":raw_key, "context_key":context_key,
            "raw_rerank_key":input_key("rerank-document", &rerank_pin, RAW, &body)?,
            "context_rerank_key":input_key("rerank-document", &rerank_pin, CONTEXT, &context)?,
        }));
    }
    anyhow::ensure!(!chunks.is_empty(), "indexed corpus has no chunks");
    let queries = cases
        .iter()
        .map(|case| {
            Ok(json!({
                "case":case, "input":case.query,
                "input_hash":blake3::hash(case.query.as_bytes()).to_hex().to_string(),
                "key":input_key("query", &embedding_pin, QUERY, &case.query)?,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let matrix: Vec<_> = [RAW, CONTEXT]
        .into_iter()
        .flat_map(|document_recipe| {
            [RAW, CONTEXT].into_iter().map(move |rerank_recipe| {
                json!({
                    "document_recipe":document_recipe, "rerank_recipe":rerank_recipe,
                    "query_recipe":QUERY,
                })
            })
        })
        .collect();
    Ok(json!({
        "schema_version":1, "phase":"offline-preflight", "advisory":true,
        "source_revision":revision, "tokenizer_version":TOKENIZER_VERSION,
        "harness_source_hash":digest(&[
            include_str!("representation.rs"), include_str!("../search_representation.rs"),
        ])?,
        "corpus_hash":actual_hash, "queries_hash":digest(&cases)?,
        "embedding_pin":embedding_pin, "rerank_pin":rerank_pin,
        "recipes":{"raw":RAW, "context":CONTEXT, "query":QUERY,
            "context_template":"Path: <rel_path>\nHeading: <heading_path>\n\n<body>"},
        "raw_document_set_key":digest(&unique_raw)?,
        "context_document_set_key":digest(&unique_context)?,
        "chunk_set_key":digest(&locations)?,
        "stats":{"documents":documents.len(), "chunks":chunks.len(), "queries":queries.len(),
            "unique_raw_items":unique_raw.len(), "unique_context_items":unique_context.len(),
            "raw_bytes":raw_bytes, "context_bytes":context_bytes},
        "matrix":matrix, "chunks":chunks, "queries":queries,
        "provider_requests":0, "quality":null,
        "limits":{"rerank_window":bench::RERANK_WINDOW,
            "per_document_limit":null, "candidate_pools":"not collected",
            "token_count":null, "live_cost_estimate_usd":null},
    }))
}

/// Atomic output publication with no overwrite of an earlier artifact.
pub fn write_new(path: &Path, value: &Value) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.persist_noclobber(path)
        .context("publish new preflight artifact")?;
    Ok(())
}

/// Manual artifacts live outside the checkout and cannot mutate frozen data.
pub fn outside_source_output(path: &Path) -> anyhow::Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().context("resolve output parent")?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    anyhow::ensure!(
        !parent.starts_with(source),
        "output must be outside the source checkout"
    );
    Ok(parent.join(path.file_name().context("output needs a file name")?))
}

/// The manual entrypoint refuses to label a modified source tree as a commit.
pub fn clean_revision() -> anyhow::Result<String> {
    let git = |args: &[&str]| -> anyhow::Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()?;
        anyhow::ensure!(output.status.success(), "git source identity failed");
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    anyhow::ensure!(
        git(&["status", "--porcelain"])?.is_empty(),
        "commit preflight source changes first"
    );
    git(&["rev-parse", "HEAD"])
}
