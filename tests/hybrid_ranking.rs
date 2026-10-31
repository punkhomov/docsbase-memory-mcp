//! Dense + RRF ranking end-to-end through the `search_docs_with` seam
//! (FR-15…FR-18, SC-2). Deterministic fake embedder and reranker: no network,
//! default offline build.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::search_docs_with;
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::dense::DenseCache;
use docsbase_memory::hybrid::jobs::{embed_project_chunks_with, embed_texts_with};
use docsbase_memory::hybrid::{HybridStages, Ranking, Reranker};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const DOCS: &[(&str, &str)] = &[
    ("a.md", "# Alpha\n\nalpha alpha widgets\n"),
    ("b.md", "# Beta\n\nsynonym text\n"),
    ("c.md", "# Gamma\n\nalpha gear\n"),
];

/// 2-d deterministic geometry: everything related to the `"alpha"` query
/// (`"alpha"` and the dense-only synonym) points at `[1, 0]`, the rest at
/// `[0, 1]`.
struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "fake/model@d2@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("alpha") || text.contains("synonym") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }
}

/// Reverses the candidate list and assigns descending scores by rank
/// (`docs.len()` down to 1), so a reorder is visible in both the order and
/// the exposed `score` field.
#[derive(Default)]
struct ReverseReranker {
    calls: AtomicUsize,
    top_ns: std::sync::Mutex<Vec<usize>>,
    documents: std::sync::Mutex<Vec<String>>,
}

impl Reranker for ReverseReranker {
    #[allow(clippy::cast_precision_loss)]
    fn rerank(&self, _query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.top_ns.lock().expect("top_ns").push(top_n);
        self.documents
            .lock()
            .expect("documents")
            .extend(docs.iter().cloned());
        let n = docs.len().min(top_n);
        Ok((0..n)
            .map(|rank| (docs.len() - 1 - rank, (n - rank) as f32))
            .collect())
    }
}

fn project_with_docs() -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for (rel, body) in DOCS {
        fs::write(root.path().join(rel), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
    docsbase_memory::daemon::tools::run_project_index(
        &mut db,
        cache.path(),
        &project,
        &Config::default(),
    )
    .expect("index");
    project.status = ProjectStatus::Indexed;
    (cache, db, project)
}

fn embed_all(db: &Db, project_id: i64) {
    let stored = embed_project_chunks_with(db, &FakeEmbedder, project_id).expect("embed");
    assert!(stored > 0, "the fixture stores vectors");
}

fn rrf_stages(reranker: Option<Arc<ReverseReranker>>) -> HybridStages {
    HybridStages {
        embedder: Some(Arc::new(FakeEmbedder)),
        reranker: reranker.map(|reranker| reranker as Arc<dyn Reranker>),
        ranking: Ranking::Rrf,
    }
}

fn paths(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["path"].as_str().expect("path").to_owned())
        .collect()
}

fn scores(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["score"].as_f64().expect("score"))
        .collect()
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

/// The default response shape is a contract: `search_docs` without `explain`
/// returns the bare citation array, and every element keeps this exact key set
/// and order. `explain: true` may add an envelope later; this test is the guard
/// that the default path never gains one (search-telemetry SC-1).
fn assert_bare_citation_array(value: &Value) {
    let rows = value
        .as_array()
        .unwrap_or_else(|| panic!("default response must stay an array, got {value}"));
    for row in rows {
        // `serde_json::Value` keeps object keys sorted, so the contract is the
        // key *set*: an extra field (an envelope, a snippet) fails here.
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("citation object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["chunk_id", "heading_path", "lines", "path", "score"],
            "citation key set is part of the contract"
        );
        assert!(row["chunk_id"].is_u64(), "chunk_id stays an integer: {row}");
        assert_eq!(
            row["lines"].as_array().map(Vec::len),
            Some(2),
            "lines stays [start, end]: {row}"
        );
        assert!(row["score"].is_number(), "score stays numeric: {row}");
    }
    let serialized = serde_json::to_string(value).expect("serialize");
    assert!(
        serialized.starts_with('['),
        "no envelope on the default path: {serialized}"
    );
}

#[test]
fn default_response_shape_is_the_bare_citation_array() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let dense = DenseCache::new();
    let reranker = Arc::new(ReverseReranker::default());
    let cases: Vec<(&str, HybridStages)> = vec![
        ("bm25", HybridStages::default()),
        (
            "bm25+rerank",
            HybridStages {
                embedder: None,
                reranker: Some(Arc::clone(&reranker) as Arc<dyn Reranker>),
                ranking: Ranking::Lexical,
            },
        ),
        ("rrf", rrf_stages(None)),
        ("rrf+rerank", rrf_stages(Some(Arc::clone(&reranker)))),
    ];
    for (label, stages) in cases {
        let value = search_docs_with(
            &stages,
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 3 }),
            50,
        )
        .expect("search");
        assert!(!value.as_array().expect("array").is_empty(), "{label}");
        assert_bare_citation_array(&value);
    }
}

/// `explain: true` adds the envelope; the scale and stages come from what ran,
/// not from the config (search-telemetry FR-2).
#[test]
fn explain_reports_the_scale_and_the_stages_that_ran() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let dense = DenseCache::new();
    let reranker = Arc::new(ReverseReranker::default());
    // `window` is the fetch that actually happened: a lexical search fetches
    // exactly `limit`, a fused or reranked one fetches `max(limit, window)`.
    let cases: Vec<(&str, HybridStages, &str, Vec<&str>, u64)> = vec![
        ("lexical", HybridStages::default(), "bm25", vec!["bm25"], 3),
        (
            "rrf",
            rrf_stages(None),
            "rrf",
            vec!["bm25", "dense", "rrf"],
            50,
        ),
        (
            "rrf+rerank",
            rrf_stages(Some(Arc::clone(&reranker))),
            "rerank",
            vec!["bm25", "dense", "rrf", "rerank"],
            50,
        ),
    ];
    for (label, stages, scale, expected_stages, expected_window) in cases {
        let value = search_docs_with(
            &stages,
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 3, "explain": true }),
            50,
        )
        .expect("explain search");
        let meta = &value["meta"];
        assert_eq!(meta["score_scale"], scale, "{label}");
        assert_eq!(
            meta["stages"],
            serde_json::json!(expected_stages),
            "{label}"
        );
        assert_eq!(
            meta["window"], expected_window,
            "{label}: window is the fetch, not limit"
        );
        assert!(
            meta["index_generation"].as_i64().unwrap_or_default() >= 0,
            "{label}: generation is reported"
        );
        assert_eq!(meta["degraded"], serde_json::json!([]), "{label}");
        assert!(
            meta["stage_ms"]["total"].as_u64().is_some(),
            "{label}: total timing present"
        );
        assert!(
            value["hits"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "{label}: hits stay the same array"
        );
    }
}

/// A `rerank: false` request on a project with a stage answers in fused order,
/// so the scale must say `rrf`, not `rerank`.
#[test]
fn explain_follows_the_effective_path_not_the_config() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let dense = DenseCache::new();
    let reranker = Arc::new(ReverseReranker::default());
    let value = search_docs_with(
        &rrf_stages(Some(Arc::clone(&reranker))),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3, "rerank": false, "explain": true }),
        50,
    )
    .expect("explain search");
    assert_eq!(value["meta"]["score_scale"], "rrf");
    assert_eq!(reranker.calls.load(Ordering::SeqCst), 0, "no rerank call");
}

#[test]
fn explain_wraps_the_empty_result_and_rejects_a_bad_flag() {
    let (cache, db, project) = project_with_docs();
    let dense = DenseCache::new();
    let empty = search_docs_with(
        &HybridStages::default(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 0, "explain": true }),
        50,
    )
    .expect("limit 0 with explain");
    assert_eq!(empty["hits"], json!([]));
    assert_eq!(empty["meta"]["window"], 0);

    let err = search_docs_with(
        &HybridStages::default(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "explain": "yes" }),
        50,
    )
    .expect_err("a non-boolean explain is a protocol error");
    assert!(err.to_string().contains("explain"), "{err}");
}

#[test]
fn limit_zero_and_errors_carry_no_metadata() {
    let (cache, db, project) = project_with_docs();
    let dense = DenseCache::new();
    let empty = search_docs_with(
        &HybridStages::default(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 0 }),
        50,
    )
    .expect("limit 0");
    assert_eq!(empty, json!([]), "limit 0 stays an empty array");

    let err = search_docs_with(
        &HybridStages::default(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": "many" }),
        50,
    )
    .expect_err("a non-integer limit is a protocol error");
    assert!(
        !err.to_string().contains("meta"),
        "errors never carry telemetry: {err}"
    );
}

#[test]
fn lexical_search_stays_bm25_only() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let dense = DenseCache::new();
    let value = search_docs_with(
        &HybridStages::default(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        50,
    )
    .expect("lexical search");
    assert_eq!(paths(&value), vec!["a.md", "c.md"]);
    assert!(
        !paths(&value).contains(&"b.md".to_owned()),
        "no dense leakage"
    );
}

#[test]
fn rrf_fuses_bm25_and_dense_lists() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(None),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        50,
    )
    .expect("rrf search");
    // BM25 ranks a=1, c=2; dense (all scores equal) ranks a=1, b=2, c=3.
    assert_eq!(paths(&value), vec!["a.md", "c.md", "b.md"]);
    let scores = scores(&value);
    close(scores[0], 2.0 / 61.0);
    close(scores[1], 1.0 / 62.0 + 1.0 / 63.0);
    close(scores[2], 1.0 / 62.0);
}

#[test]
fn rrf_rerank_applies_on_top_of_the_fused_window() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let reranker = Arc::new(ReverseReranker::default());
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(Some(Arc::clone(&reranker))),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        50,
    )
    .expect("rrf + rerank");
    assert_eq!(paths(&value), vec!["b.md", "c.md", "a.md"]);
    assert_eq!(
        scores(&value),
        vec![3.0, 2.0, 1.0],
        "score = relevance_score"
    );
    assert_eq!(reranker.calls.load(Ordering::SeqCst), 1);
    let documents = reranker.documents.lock().expect("documents");
    assert!(
        documents.last().expect("fused window").contains("synonym"),
        "the provider sees the fused order: {documents:?}"
    );
}

#[test]
fn rerank_false_keeps_the_fused_order() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let reranker = Arc::new(ReverseReranker::default());
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(Some(Arc::clone(&reranker))),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3, "rerank": false }),
        50,
    )
    .expect("rrf without rerank");
    assert_eq!(
        paths(&value),
        vec!["a.md", "c.md", "b.md"],
        "FR-18: ranking order"
    );
    assert_eq!(reranker.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn limit_one_still_widens_the_window_for_rrf() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let reranker = Arc::new(ReverseReranker::default());
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(Some(Arc::clone(&reranker))),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 1 }),
        50,
    )
    .expect("rrf + rerank, limit 1");
    assert_eq!(
        paths(&value),
        vec!["b.md"],
        "the window is max(limit, window)"
    );
    assert_eq!(reranker.documents.lock().expect("documents").len(), 3);
}

#[test]
fn zero_coverage_degrades_to_bm25_order_with_rrf_scores() {
    let (cache, db, project) = project_with_docs();
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(None),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        50,
    )
    .expect("rrf without vectors");
    assert_eq!(paths(&value), vec!["a.md", "c.md"], "no silent dense hits");
    let scores = scores(&value);
    close(scores[0], 1.0 / 61.0);
    close(scores[1], 1.0 / 62.0);
}

#[test]
fn partial_coverage_skips_chunks_without_vectors() {
    let (cache, db, project) = project_with_docs();
    let texts =
        docsbase_memory::store::repo::chunk_texts(db.connection(), project.id).expect("texts");
    let covered: Vec<String> = texts
        .iter()
        .filter(|text| text.contains("alpha"))
        .cloned()
        .collect();
    embed_texts_with(cache.path(), &FakeEmbedder, &covered).expect("partial embed");

    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(None),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        50,
    )
    .expect("rrf with partial coverage");
    assert_eq!(
        paths(&value),
        vec!["a.md", "c.md"],
        "the uncovered chunk has no dense candidate and no lexical match"
    );
}

fn project_with_many_docs() -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for index in 0..15u32 {
        let body = if index % 2 == 0 {
            format!("# Doc {index}\n\nalpha alpha topic {index}\n")
        } else {
            format!("# Doc {index}\n\nsynonym topic {index}\n")
        };
        fs::write(root.path().join(format!("doc-{index:02}.md")), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
    docsbase_memory::daemon::tools::run_project_index(
        &mut db,
        cache.path(),
        &project,
        &Config::default(),
    )
    .expect("index");
    project.status = ProjectStatus::Indexed;
    (cache, db, project)
}

#[test]
fn rerank_sees_at_most_fetch_candidates_and_ranks_the_window() {
    let (cache, db, project) = project_with_many_docs();
    embed_all(&db, project.id);
    let reranker = Arc::new(ReverseReranker::default());
    let dense = DenseCache::new();
    let value = search_docs_with(
        &rrf_stages(Some(Arc::clone(&reranker))),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 3 }),
        10,
    )
    .expect("rrf + rerank, window 10");
    assert_eq!(value.as_array().expect("array").len(), 3);
    let documents = reranker.documents.lock().expect("documents");
    assert_eq!(
        documents.len(),
        10,
        "the fused union is truncated to fetch before rerank"
    );
    assert_eq!(
        *reranker.top_ns.lock().expect("top_ns"),
        vec![10],
        "top_n = the fetched candidate window"
    );
}

#[test]
fn cap_degradation_streams_the_same_fused_results() {
    let (cache, db, project) = project_with_docs();
    embed_all(&db, project.id);
    let args = json!({ "query": "alpha", "limit": 3 });
    let cached = search_docs_with(
        &rrf_stages(None),
        &DenseCache::new(),
        &db,
        cache.path(),
        &project,
        &args,
        50,
    )
    .expect("cached scan");
    let streamed = search_docs_with(
        &rrf_stages(None),
        &DenseCache::with_cap(1),
        &db,
        cache.path(),
        &project,
        &args,
        50,
    )
    .expect("streamed scan");
    assert_eq!(
        cached, streamed,
        "cap degradation changes memory, not results"
    );
}

#[test]
fn rrf_without_a_stage_is_an_internal_error() {
    let (cache, db, project) = project_with_docs();
    let stages = HybridStages {
        embedder: None,
        reranker: None,
        ranking: Ranking::Rrf,
    };
    let err = search_docs_with(
        &stages,
        &DenseCache::new(),
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha" }),
        50,
    )
    .expect_err("invariant violation");
    assert!(err.to_string().contains("rrf"), "{err}");
}
