//! Manual BEIR evaluation, separate from the frozen regression gate.
#![allow(dead_code)] // Shared test support is compiled by several integration targets.
mod execution;
pub mod metrics;
mod replay;
use super::bench::{self, ConfigId, ExpectedHit, QueryCase};
use anyhow::{Context, ensure};
use docsbase_memory::index::QueryMode;
use metrics::ExternalCase;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const ADAPTER_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetMeta {
    pub dataset: String,
    pub dataset_version: String,
    pub source_url: String,
    pub downloaded_at: String,
    pub license_note: String,
}
#[derive(Debug, Deserialize)]
struct Document {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    title: String,
    text: String,
}
#[derive(Debug, Deserialize)]
struct Query {
    #[serde(rename = "_id")]
    id: String,
    text: String,
}

pub struct ExternalOptions {
    pub query_mode: QueryMode,
    pub split: String,
    pub k: usize,
    pub configs: Vec<ConfigId>,
    pub replay_dir: Option<PathBuf>,
    pub cost_cap_usd: f64,
}
impl Default for ExternalOptions {
    fn default() -> Self {
        Self {
            query_mode: QueryMode::default(),
            split: "test".into(),
            k: 10,
            configs: vec![ConfigId::Bm25],
            replay_dir: None,
            cost_cap_usd: 2.0,
        }
    }
}
impl ExternalOptions {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.split.is_empty()
                && self
                    .split
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "split must be a single alphanumeric name"
        );
        ensure!((1..=20).contains(&self.k), "external k must be in 1..=20");
        ensure!(
            !self.configs.is_empty(),
            "select at least one configuration"
        );
        ensure!(
            self.configs.contains(&ConfigId::Bm25),
            "external configurations must include the free BM25 baseline"
        );
        let unique: BTreeSet<_> = self.configs.iter().collect();
        ensure!(
            unique.len() == self.configs.len(),
            "duplicate configurations"
        );
        ensure!(
            self.cost_cap_usd.is_finite() && self.cost_cap_usd >= 0.0,
            "cost cap must be finite and nonnegative"
        );
        ensure!(
            self.configs.iter().all(|c| *c == ConfigId::Bm25) || self.replay_dir.is_some(),
            "non-BM25 modes require explicit offline replay_dir"
        );
        Ok(())
    }
}

/// Owns derived Markdown; no source dataset file is modified.
pub struct ExternalDataset {
    pub meta: DatasetMeta,
    pub root: tempfile::TempDir,
    pub split: String,
    pub cases: Vec<QueryCase>,
    pub path_to_id: BTreeMap<String, String>,
    pub qrels: BTreeMap<String, BTreeMap<String, i64>>,
    pub corpus_hash: String,
    pub queries_hash: String,
    pub qrels_hash: String,
    pub derived_corpus_hash: String,
    pub query_ids: BTreeSet<String>,
    pub split_query_ids: BTreeSet<String>,
    pub zero_only_query_ids: Vec<String>,
}
impl ExternalDataset {
    pub fn load(dir: &Path, split: &str, k: usize) -> anyhow::Result<Self> {
        ExternalOptions {
            split: split.into(),
            k,
            ..ExternalOptions::default()
        }
        .validate()?;
        let meta: DatasetMeta = read_json(&dir.join("dataset.json"))?;
        for value in [
            &meta.dataset,
            &meta.dataset_version,
            &meta.source_url,
            &meta.downloaded_at,
            &meta.license_note,
        ] {
            ensure!(
                !value.trim().is_empty(),
                "dataset metadata fields must be nonempty"
            );
        }
        let corpus_path = dir.join("corpus.jsonl");
        let query_path = dir.join("queries.jsonl");
        let qrels_path = dir.join("qrels").join(format!("{split}.tsv"));
        // Hash exactly the bytes parsed below, never a later re-read of mutable input.
        let corpus_bytes = std::fs::read(&corpus_path).context("read corpus.jsonl")?;
        let query_bytes = std::fs::read(&query_path).context("read queries.jsonl")?;
        let qrels_bytes = std::fs::read(&qrels_path).context("read selected qrels")?;
        let docs = load_documents(&corpus_bytes)?;
        let queries = load_queries(&query_bytes)?;
        ensure!(
            !docs.is_empty() && !queries.is_empty(),
            "empty corpus or queries"
        );
        let qrels = parse_qrels(&qrels_bytes, &docs, &queries)?;
        let root = tempfile::tempdir().context("create derived corpus")?;
        let mut path_to_id = BTreeMap::new();
        let mut id_to_path = BTreeMap::new();
        for (id, doc) in &docs {
            let path = format!("{}.md", hash(id.as_bytes()));
            ensure!(
                path_to_id.insert(path.clone(), id.clone()).is_none(),
                "document filename hash collision"
            );
            id_to_path.insert(id.as_str(), path.clone());
            let markdown = if doc.title.trim().is_empty() {
                format!("{}\n", doc.text)
            } else {
                format!(
                    "# {}\n\n{}\n",
                    doc.title.replace(['\n', '\r'], " "),
                    doc.text
                )
            };
            std::fs::write(root.path().join(path), markdown).context("write derived Markdown")?;
        }
        let mut cases = Vec::new();
        let mut zero_only_query_ids = Vec::new();
        for (id, judgements) in &qrels {
            let expected: Vec<_> = judgements
                .iter()
                .filter(|(_, score)| **score > 0)
                .map(|(doc, _)| ExpectedHit {
                    path: id_to_path[doc.as_str()].clone(),
                    lines: None,
                    grade: 2,
                })
                .collect();
            if expected.is_empty() {
                zero_only_query_ids.push(id.clone());
                continue;
            }
            cases.push(QueryCase {
                id: id.clone(),
                query: queries[id].clone(),
                class: meta.dataset.clone(),
                k,
                expected,
                absent: vec![],
                source: "external".into(),
                notes: "binary positive BEIR qrels; document relevance in citation slots".into(),
                reviewed_by: vec![],
            });
        }
        ensure!(!cases.is_empty(), "selected split has no positive queries");
        let derived_corpus_hash = bench::corpus_hash(root.path())?;
        Ok(Self {
            meta,
            root,
            cases,
            split: split.into(),
            path_to_id,
            corpus_hash: hash(&corpus_bytes),
            queries_hash: hash(&query_bytes),
            qrels_hash: hash(&qrels_bytes),
            derived_corpus_hash,
            query_ids: queries.into_keys().collect(),
            split_query_ids: qrels.keys().cloned().collect(),
            qrels,
            zero_only_query_ids,
        })
    }
}

pub fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}
fn json_lines<T: serde::de::DeserializeOwned>(bytes: &[u8], label: &str) -> anyhow::Result<Vec<T>> {
    std::str::from_utf8(bytes)
        .with_context(|| format!("{label}: UTF-8"))?
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).with_context(|| format!("{label}: JSON line {}", i + 1))
        })
        .collect()
}
fn parse_qrels(
    bytes: &[u8],
    docs: &BTreeMap<String, Document>,
    queries: &BTreeMap<String, String>,
) -> anyhow::Result<BTreeMap<String, BTreeMap<String, i64>>> {
    let text = std::str::from_utf8(bytes).context("qrels: UTF-8")?;
    let mut lines = text.lines();
    ensure!(
        lines.next() == Some("query-id\tcorpus-id\tscore"),
        "qrels header must be query-id/corpus-id/score TSV"
    );
    let mut out: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (i, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<_> = line.split('\t').collect();
        ensure!(
            cols.len() == 3,
            "qrels row {} must have three columns",
            i + 2
        );
        ensure!(
            queries.contains_key(cols[0]),
            "unknown qrels query {}",
            cols[0]
        );
        ensure!(
            docs.contains_key(cols[1]),
            "unknown qrels document {}",
            cols[1]
        );
        let score: i64 = cols[2].parse().context("qrels score must be an integer")?;
        ensure!(score >= 0, "negative qrels score is unsupported");
        ensure!(
            out.entry(cols[0].into())
                .or_default()
                .insert(cols[1].into(), score)
                .is_none(),
            "duplicate qrels query/document pair"
        );
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct Provenance {
    #[serde(flatten)]
    pub dataset: DatasetMeta,
    pub split: String,
    pub query_mode: QueryMode,
    pub k: usize,
    pub document_count: usize,
    pub query_count: usize,
    pub split_query_ids: Vec<String>,
    pub evaluated_query_ids: Vec<String>,
    pub excluded_query_ids: Vec<String>,
    pub zero_only_query_ids: Vec<String>,
    pub corpus_hash: String,
    pub queries_hash: String,
    pub qrels_hash: String,
    pub derived_corpus_hash: String,
    pub adapter_version: u32,
    pub bench_version: u32,
    pub harness_source_hash: String,
    pub engine_revision: String,
    pub engine_source_hash: String,
    pub tokenizer_version: u32,
    pub model_pins: Vec<String>,
    pub embedding_vector_format: Option<&'static str>,
    pub replay_hashes: BTreeMap<String, String>,
    pub metric_contract: String,
}
#[derive(Debug, Serialize)]
pub struct ExternalConfig {
    pub cases: Vec<ExternalCase>,
    pub summary: metrics::Summary,
}
#[derive(Debug, Serialize)]
pub struct Cost {
    pub cap_usd: f64,
    pub spent_usd: f64,
    pub provider_requests: usize,
    pub mode: String,
}
#[derive(Debug, Serialize)]
pub struct Performance {
    pub index_setup_ms: f64,
    pub build_profile: String,
    pub platform: String,
    pub latency: BTreeMap<ConfigId, bench::LatencyReport>,
}
#[derive(Debug, Serialize)]
pub struct ExternalReport {
    pub track: String,
    pub advisory: bool,
    pub provenance: Provenance,
    pub quality: BTreeMap<ConfigId, ExternalConfig>,
    pub performance: Performance,
    pub cost: Cost,
}

pub fn evaluate_directory(dir: &Path, options: &ExternalOptions) -> anyhow::Result<ExternalReport> {
    options.validate()?;
    let dataset = ExternalDataset::load(dir, &options.split, options.k)?;
    run_external(&dataset, options)
}
pub fn run_external(
    dataset: &ExternalDataset,
    options: &ExternalOptions,
) -> anyhow::Result<ExternalReport> {
    options.validate()?;
    ensure!(
        dataset.split == options.split,
        "dataset/options split mismatch"
    );
    ensure!(
        dataset.cases.iter().all(|c| c.k == options.k),
        "dataset/options k mismatch"
    );
    let started = std::time::Instant::now();
    let count = dataset.path_to_id.len();
    let env = bench::build_env(dataset.root.path(), "external", count..=count)?;
    let index_setup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let replay = replay::load(dataset, options)?;
    let mut quality = BTreeMap::new();
    let mut latency = BTreeMap::new();
    for cfg in &options.configs {
        if matches!(cfg, ConfigId::Rrf | ConfigId::RrfRerank) {
            bench::preload_vectors(
                &env,
                replay
                    .stages
                    .vectors
                    .as_ref()
                    .context("dense replay vectors required")?,
            )?;
        }
        let stages = bench::stages_for(
            *cfg,
            replay.stages.embedder.clone(),
            replay.stages.reranker.clone(),
        );
        let (cases, times) = execution::run(&env, dataset, &stages, options.query_mode)?;
        let summary = metrics::summarize(&cases);
        quality.insert(*cfg, ExternalConfig { cases, summary });
        latency.insert(
            *cfg,
            bench::LatencyReport {
                p50_ms: bench::percentile(&times, 0.5),
                p95_ms: bench::percentile(&times, 0.95),
            },
        );
    }
    Ok(ExternalReport {
        track: "external".into(),
        advisory: true,
        provenance: provenance(dataset, options, &replay)?,
        quality,
        performance: Performance {
            index_setup_ms,
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            latency,
        },
        cost: Cost {
            cap_usd: options.cost_cap_usd,
            spent_usd: 0.0,
            provider_requests: 0,
            mode: "offline; no provider client".into(),
        },
    })
}

fn provenance(
    dataset: &ExternalDataset,
    options: &ExternalOptions,
    replay: &replay::LoadedReplay,
) -> anyhow::Result<Provenance> {
    let evaluated: BTreeSet<_> = dataset.cases.iter().map(|c| c.id.clone()).collect();
    let engine_revision = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .context("read engine revision")?;
    ensure!(
        engine_revision.status.success(),
        "engine revision unavailable"
    );
    let source = [
        include_bytes!("external.rs").as_slice(),
        include_bytes!("external/metrics.rs").as_slice(),
        include_bytes!("external/execution.rs").as_slice(),
        include_bytes!("external/replay.rs").as_slice(),
        include_bytes!("bench.rs").as_slice(),
    ];
    let mut hasher = blake3::Hasher::new();
    for bytes in source {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(Provenance {
        dataset: dataset.meta.clone(),
        split: options.split.clone(),
        query_mode: options.query_mode,
        k: options.k,
        document_count: dataset.path_to_id.len(),
        query_count: dataset.query_ids.len(),
        split_query_ids: dataset.split_query_ids.iter().cloned().collect(),
        evaluated_query_ids: evaluated.iter().cloned().collect(),
        excluded_query_ids: dataset.query_ids.difference(&evaluated).cloned().collect(),
        zero_only_query_ids: dataset.zero_only_query_ids.clone(),
        corpus_hash: dataset.corpus_hash.clone(),
        queries_hash: dataset.queries_hash.clone(),
        qrels_hash: dataset.qrels_hash.clone(),
        derived_corpus_hash: dataset.derived_corpus_hash.clone(),
        adapter_version: ADAPTER_VERSION,
        bench_version: bench::BENCH_VERSION,
        harness_source_hash: hasher.finalize().to_hex().to_string(),
        engine_revision: String::from_utf8(engine_revision.stdout)?.trim().into(),
        engine_source_hash: engine_source_hash()?,
        tokenizer_version: docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
        model_pins: replay.pins.clone(),
        embedding_vector_format: replay.stages.vectors.as_ref().map(|fixture| {
            if fixture.query_vectors.is_some() { "v2-role-aware-raw" }
            else { "v1-legacy-role-unknown" }
        }),
        replay_hashes: replay.hashes.clone(),
        metric_contract: "binary positive document qrels at top-k citation slots; duplicate document gain once; IDCG truncated at k; unjudged is not explicit negative".into(),
    })
}

pub fn measurement_from_env() -> anyhow::Result<()> {
    let dir = PathBuf::from(
        std::env::var("DOCSBASE_EXTERNAL_DIR").context("DOCSBASE_EXTERNAL_DIR is required")?,
    );
    let output = PathBuf::from(
        std::env::var("DOCSBASE_EXTERNAL_REPORT")
            .context("DOCSBASE_EXTERNAL_REPORT is required")?,
    );
    let mut options = ExternalOptions::default();
    if let Some(raw) = std::env::var_os("DOCSBASE_EXTERNAL_QUERY_MODE") {
        let raw = raw.into_string().map_err(|_| {
            anyhow::anyhow!("invalid external query mode: expected Unicode syntax or literal")
        })?;
        options.query_mode = raw
            .parse()
            .map_err(anyhow::Error::msg)
            .context("invalid external query mode")?;
    }
    if let Ok(raw) = std::env::var("DOCSBASE_EXTERNAL_SPLIT") {
        options.split = raw;
    }
    if let Ok(raw) = std::env::var("DOCSBASE_EXTERNAL_K") {
        options.k = raw.parse().context("invalid external k")?;
    }
    if let Ok(raw) = std::env::var("DOCSBASE_EXTERNAL_CONFIGS") {
        options.configs = parse_configs(&raw)?;
    }
    options.replay_dir = std::env::var_os("DOCSBASE_EXTERNAL_REPLAY_DIR").map(PathBuf::from);
    if let Ok(raw) = std::env::var("DOCSBASE_BENCH_COST_CAP_USD") {
        options.cost_cap_usd = raw.parse().context("invalid cost cap")?;
    }
    let report = evaluate_directory(&dir, &options)?;
    write_report(&report, &output, &dir)?;
    println!("external advisory report: {}", output.display());
    Ok(())
}
pub fn parse_configs(raw: &str) -> anyhow::Result<Vec<ConfigId>> {
    raw.split(',')
        .map(|s| match s.trim() {
            "Bm25" => Ok(ConfigId::Bm25),
            "Bm25Rerank" => Ok(ConfigId::Bm25Rerank),
            "Rrf" => Ok(ConfigId::Rrf),
            "RrfRerank" => Ok(ConfigId::RrfRerank),
            _ => anyhow::bail!("unknown external configuration {s:?}"),
        })
        .collect()
}
pub fn write_report(report: &ExternalReport, output: &Path, dataset: &Path) -> anyhow::Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Existing parent required: canonicalize before any write, including symlink aliases.
    let parent = parent
        .canonicalize()
        .context("report parent must already exist")?;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let allowed_target = repo.join("target/search-bench");
    let allowed_reports = repo.join("docs/specs/search-benchmark/reports");
    ensure!(
        !parent.starts_with(&repo)
            || parent.starts_with(&allowed_target)
            || parent.starts_with(&allowed_reports),
        "report inside repository must use target/search-bench or benchmark reports directory"
    );
    ensure!(
        !parent.starts_with(dataset.canonicalize()?),
        "report may not overwrite source dataset"
    );
    ensure!(
        !output.exists(),
        "report already exists; choose a new output path"
    );
    let bytes = serde_json::to_vec_pretty(report)?;
    let mut file = tempfile::NamedTempFile::new_in(&parent).context("create report temp file")?;
    file.write_all(&bytes).context("write report")?;
    file.persist_noclobber(output)
        .context("publish complete report without overwriting")?;
    Ok(())
}

fn load_documents(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, Document>> {
    let mut docs = BTreeMap::new();
    for doc in json_lines::<Document>(bytes, "corpus")? {
        ensure!(!doc.id.trim().is_empty(), "empty document ID");
        ensure!(
            !doc.title.trim().is_empty() || !doc.text.trim().is_empty(),
            "empty document text for {}",
            doc.id
        );
        let id = doc.id.clone();
        ensure!(
            docs.insert(id.clone(), doc).is_none(),
            "duplicate document ID {id}"
        );
    }
    Ok(docs)
}
fn load_queries(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut queries = BTreeMap::new();
    for q in json_lines::<Query>(bytes, "queries")? {
        ensure!(
            !q.id.trim().is_empty() && !q.text.trim().is_empty(),
            "empty query ID or text"
        );
        ensure!(
            queries.insert(q.id.clone(), q.text).is_none(),
            "duplicate query ID {}",
            q.id
        );
    }
    Ok(queries)
}
fn engine_source_hash() -> anyhow::Result<String> {
    fn collect(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                collect(root, &entry.path(), files)?;
            } else if entry.file_type()?.is_file() {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    bench::blake3_file(&entry.path())?,
                );
            }
        }
        Ok(())
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = BTreeMap::new();
    collect(root, &root.join("src"), &mut files)?;
    for name in ["Cargo.toml", "Cargo.lock"] {
        files.insert(name.into(), bench::blake3_file(&root.join(name))?);
    }
    Ok(hash(&serde_json::to_vec(&files)?))
}
