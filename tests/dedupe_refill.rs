//! #68: real SQLite/Tantivy retrieval, bounded refill and one rerank operation.
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::dense::DenseCache;
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::hybrid::{HybridStages, Ranking, Reranker};
use docsbase_memory::search::budget::PayloadBudget;
use docsbase_memory::search::metadata::MetadataSource;
use docsbase_memory::search::operation::SearchOperation;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{Db, repo};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    // Close SQLite before removing its files, including on Windows.
    db: Db,
    project: Project,
    _root: TempDir,
    cache: TempDir,
}
impl Fixture {
    fn new(chunks: usize, others: usize) -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let root = tempfile::tempdir().expect("root");
        fs::create_dir(root.path().join("guide")).expect("guide");
        let mut body = "# Widgets\n\nwidgets widgets widgets\n\n".repeat(chunks - 1);
        body.push_str("# Widgets\n\nwidgets widgets widgets winner\n\n");
        fs::write(root.path().join("guide/large.md"), body).expect("large");
        for i in 0..others {
            fs::write(
                root.path().join(format!("guide/small-{i:02}.md")),
                "# Answer\n\nwidgets are configured here.\n",
            )
            .expect("small");
        }
        fs::write(
            root.path().join("irrelevant.md"),
            "# Different\n\nunrelated text\n",
        )
        .expect("irrelevant");
        let mut db = Db::open(cache.path()).expect("db");
        let mut project = registry::ensure_project(&mut db, root.path()).expect("project");
        tools::run_project_index(&mut db, cache.path(), &project, &Config::default())
            .expect("index");
        project.status = ProjectStatus::Indexed;
        Self {
            db,
            project,
            _root: root,
            cache,
        }
    }
    fn search(&self, stages: &HybridStages, args: &Value) -> Value {
        tools::search_docs_with(
            stages,
            &DenseCache::new(),
            &self.db,
            self.cache.path(),
            &self.project,
            args,
            40,
        )
        .expect("search")
    }
}

fn policy() -> docsbase_memory::search::settings::SearchSettings {
    let mut policy = docsbase_memory::search::settings::standalone()
        .expect("policy")
        .into_settings();
    policy.max_retained_payload_bytes = policy.max_request_payload_bytes;
    policy
}

fn args() -> Value {
    json!({"query":"widgets", "limit":10, "dedupe":"path", "explain":true})
}
fn hits(result: &Value) -> &[Value] {
    result["hits"].as_array().expect("hits")
}
fn assert_cap(result: &Value, cap: usize) {
    let mut counts = std::collections::HashMap::new();
    for hit in hits(result) {
        let count = counts
            .entry(hit["path"].as_str().expect("path"))
            .or_insert(0);
        *count += 1;
        assert!(*count <= cap, "cap exceeded: {result}");
        assert!(hit["path"] != "irrelevant.md", "no lexical filler");
    }
}

#[test]
fn forty_dominant_chunks_refill_ten_hits_in_stable_score_order() {
    let f = Fixture::new(40, 10);
    let stages = HybridStages::default();
    let plain = f.search(&stages, &json!({"query":"widgets", "limit":50}));
    assert_eq!(plain.as_array().expect("plain").len(), 50);
    assert!(
        plain.as_array().expect("plain")[..40]
            .iter()
            .all(|hit| hit["path"] == "guide/large.md")
    );
    let result = f.search(&stages, &args());
    assert_eq!(
        hits(&result).len(),
        10,
        "refill beyond the old40 window: {result}"
    );
    assert_cap(&result, 2);
    let mut expected = Vec::new();
    let mut per_doc = std::collections::HashMap::new();
    for hit in plain.as_array().expect("plain") {
        let count = per_doc
            .entry(hit["path"].as_str().expect("path"))
            .or_insert(0);
        if *count < 2 {
            expected.push(hit.clone());
            *count += 1;
        }
    }
    expected.truncate(10);
    assert_eq!(hits(&result), expected);
    assert_eq!(result["meta"]["refill"]["stop_reason"], "satisfied");
    assert_eq!(result["meta"]["refill"]["candidates"], 50);
    assert_eq!(result["meta"]["refill"]["capacity"], 12);
    assert_eq!(result["meta"]["trimmed"]["deduped"], 38);
    assert_eq!(result["meta"]["trimmed"]["filtered_out"], 0);
}

#[test]
fn single_matching_document_is_exhaustion_without_filler() {
    let f = Fixture::new(40, 0);
    let result = f.search(&HybridStages::default(), &args());
    assert_eq!(hits(&result).len(), 2);
    assert_cap(&result, 2);
    assert_eq!(result["meta"]["refill"]["stop_reason"], "source-exhausted");
    assert_eq!(result["meta"]["refill"]["candidates"], 40);
}

#[test]
fn more_than_a_thousand_dominant_chunks_report_budget_not_exhaustion() {
    let f = Fixture::new(1010, 10);
    let result = f.search(&HybridStages::default(), &args());
    assert_eq!(hits(&result).len(), 2);
    assert_eq!(result["meta"]["window"], 1000);
    assert_eq!(result["meta"]["refill"]["candidate_limit"], 1000);
    assert_eq!(result["meta"]["refill"]["candidates"], 1000);
    assert_eq!(result["meta"]["refill"]["stop_reason"], "candidate-budget");
    assert_eq!(result["meta"]["trimmed"]["deduped"], 998);
}

#[derive(Default)]
struct Geometry {
    queries: AtomicUsize,
}
impl Embedder for Geometry {
    fn model_id(&self) -> &'static str {
        "refill-fixture@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("widgets widgets") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.6, 0.8]
                }
            })
            .collect())
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1.0, 0.0])
    }
}
#[derive(Default)]
struct Winner {
    inputs: Mutex<Vec<Vec<String>>>,
}
impl Reranker for Winner {
    fn rerank(&self, _: &str, documents: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        assert_eq!(top_n, documents.len());
        self.inputs.lock().expect("inputs").push(documents.to_vec());
        let mut ranked: Vec<_> = documents
            .iter()
            .enumerate()
            .map(|(i, text)| (i, if text.contains("winner") { 1.0 } else { 0.0 }))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).expect("finite"));
        Ok(ranked)
    }
}

#[test]
fn rrf_and_rerank_refill_once_and_keep_the_late_best_chunk() {
    let f = Fixture::new(40, 10);
    let geometry = Arc::new(Geometry::default());
    docsbase_memory::hybrid::jobs::embed_project_chunks_with(
        &f.db,
        geometry.as_ref(),
        f.project.id,
    )
    .expect("vectors");
    for (rrf, rerank) in [(false, true), (true, false), (true, true)] {
        let winner = Arc::new(Winner::default());
        let stages = HybridStages {
            embedder: rrf.then(|| geometry.clone() as Arc<dyn Embedder>),
            reranker: rerank.then(|| winner.clone() as Arc<dyn Reranker>),
            ranking: if rrf { Ranking::Rrf } else { Ranking::Lexical },
        };
        let mut query = args();
        query["path_prefix"] = json!("guide/");
        query["snippet"] = json!(true);
        let result = f.search(&stages, &query);
        assert_eq!(
            hits(&result).len(),
            10,
            "rrf={rrf} rerank={rerank}: {result}"
        );
        assert_cap(&result, 2);
        assert_eq!(result["meta"]["refill"]["stop_reason"], "satisfied");
        let inputs = winner.inputs.lock().expect("inputs");
        if rerank {
            assert_eq!(inputs.len(), 1, "one comparable rerank score scale");
            assert_eq!(
                inputs[0].len(),
                50,
                "keep every base duplicate and all refill chunks"
            );
            assert!(
                hits(&result)[0]["snippet"]
                    .as_str()
                    .expect("snippet")
                    .contains("winner")
            );
        } else {
            assert_eq!(inputs.len(), 0);
            let first = hits(&result)[0]["score"].as_f64().expect("score");
            assert!((first - 2.0 / 61.0).abs() < 1e-6, "scoped RRF source ranks");
        }
    }
    assert_eq!(geometry.queries.load(Ordering::SeqCst), 2);
}

#[test]
fn generation_change_during_refill_refuses_mixed_state() {
    let f = Fixture::new(40, 10);
    let policy = policy();
    let budget = PayloadBudget::new(policy).expect("budget");
    let stages = HybridStages::default();
    let dense = DenseCache::new();
    let context = tools::SearchContext {
        stages: &stages,
        dense: &dense,
        cache: f.cache.path(),
        project: &f.project,
        window: 40,
        payload_budget: &budget,
        operation: SearchOperation::new(budget.settings(), Instant::now(), Cancellation::new())
            .expect("operation"),
    };
    let phases = AtomicUsize::new(0);
    let result =
        tools::search_docs_from(&context, MetadataSource::Borrowed(&f.db), &args(), || {
            if phases.fetch_add(1, Ordering::SeqCst) == 1 {
                repo::bump_index_generation(f.db.connection(), f.project.id)?;
            }
            Ok(())
        });
    assert!(
        matches!(result, Err(Error::Project { .. })),
        "must refuse stale candidates: {result:?}"
    );
    drop(
        budget
            .reserve(budget.settings().max_request_payload_bytes)
            .expect("scope/payload released"),
    );
}

#[test]
fn configured_refill_ceiling_is_explicit_and_never_shrinks_the_base_window() {
    let f = Fixture::new(40, 10);
    let config_dir = tempfile::tempdir().expect("config");
    fs::write(
        config_dir.path().join("config.toml"),
        "[search]\ndedupe_max_candidates = 40\n",
    )
    .expect("config");
    let mut config = Config::global_from(config_dir.path()).expect("refill policy");
    config.search.max_retained_payload_bytes = config.search.max_request_payload_bytes;
    let budget = PayloadBudget::new(config.search).expect("budget");
    let stages = HybridStages::default();
    let dense = DenseCache::new();
    for (limit, want, window, reason) in [
        (10, 2, 40, "candidate-budget"),
        (50, 12, 200, "source-exhausted"),
    ] {
        let context = tools::SearchContext {
            stages: &stages,
            dense: &dense,
            cache: f.cache.path(),
            project: &f.project,
            window: 40,
            payload_budget: &budget,
            operation: SearchOperation::new(budget.settings(), Instant::now(), Cancellation::new())
                .expect("operation"),
        };
        let mut query = args();
        query["limit"] = json!(limit);
        let result = tools::search_docs_in(&context, &f.db, &query).expect("configured search");
        assert_eq!(hits(&result).len(), want);
        assert_eq!(result["meta"]["window"], window);
        assert_eq!(result["meta"]["refill"]["candidate_limit"], window);
        assert_eq!(result["meta"]["refill"]["stop_reason"], reason);
    }
    drop(
        budget
            .reserve(budget.settings().max_request_payload_bytes)
            .expect("released"),
    );
}

#[test]
fn rrf_refill_keeps_fused_scores_and_order_of_surviving_chunks() {
    let f = Fixture::new(40, 10);
    let geometry = Arc::new(Geometry::default());
    docsbase_memory::hybrid::jobs::embed_project_chunks_with(
        &f.db,
        geometry.as_ref(),
        f.project.id,
    )
    .expect("vectors");
    let stages = HybridStages {
        embedder: Some(geometry),
        ranking: Ranking::Rrf,
        reranker: None,
    };
    let full = f.search(
        &stages,
        &json!({"query":"widgets", "limit":1000,
        "path_prefix":"guide/"}),
    );
    let mut query = args();
    query["path_prefix"] = json!("guide/");
    let result = f.search(&stages, &query);
    let mut counts = std::collections::HashMap::new();
    let expected: Vec<_> = full
        .as_array()
        .expect("full")
        .iter()
        .filter(|hit| {
            let count = counts
                .entry(hit["path"].as_str().expect("path"))
                .or_insert(0);
            *count += 1;
            *count <= 2
        })
        .take(10)
        .cloned()
        .collect();
    assert_eq!(hits(&result), expected);
    assert_eq!(result["meta"]["refill"]["dense_window"], 1000);
    assert_eq!(result["meta"]["refill"]["rounds"], 2);
}

#[test]
fn no_cap_and_zero_limit_keep_their_original_response_contract() {
    let f = Fixture::new(40, 10);
    for query in [
        json!({"query":"widgets", "limit":10, "explain":true}),
        json!({"query":"widgets", "limit":0, "dedupe":"path", "explain":true}),
    ] {
        let result = f.search(&HybridStages::default(), &query);
        assert!(result["meta"].get("refill").is_none());
        assert_eq!(result["meta"]["window"], query["limit"]);
    }
    let result = f.search(
        &HybridStages::default(),
        &json!({"query":"widgets", "limit":10, "dedupe":"path"}),
    );
    assert!(result.is_array());
    assert_eq!(result.as_array().expect("array").len(), 10);
}

#[test]
fn cancellation_at_refill_admission_releases_scope_without_calling_rerank() {
    let f = Fixture::new(40, 10);
    let budget = PayloadBudget::new(policy()).expect("budget");
    let winner = Arc::new(Winner::default());
    let stages = HybridStages {
        reranker: Some(winner.clone()),
        ..HybridStages::default()
    };
    let dense = DenseCache::new();
    let token = Cancellation::new();
    let context = tools::SearchContext {
        stages: &stages,
        dense: &dense,
        cache: f.cache.path(),
        project: &f.project,
        window: 40,
        payload_budget: &budget,
        operation: SearchOperation::new(budget.settings(), Instant::now(), token.clone())
            .expect("operation"),
    };
    let phases = AtomicUsize::new(0);
    let mut query = args();
    query["path_prefix"] = json!("guide/");
    let result = tools::search_docs_from(&context, MetadataSource::Borrowed(&f.db), &query, || {
        if phases.fetch_add(1, Ordering::SeqCst) == 1 {
            token.cancel();
        }
        Ok(())
    });
    assert!(matches!(result, Err(Error::Admission { .. })), "{result:?}");
    assert_eq!(winner.inputs.lock().expect("inputs").len(), 0);
    drop(
        budget
            .reserve(budget.settings().max_request_payload_bytes)
            .expect("released"),
    );
}

#[test]
fn expanded_payload_refusal_is_an_error_before_any_rerank_and_releases_admission() {
    let f = Fixture::new(40, 10);
    let mut policy = policy();
    policy.max_request_payload_bytes = 1;
    policy.max_retained_payload_bytes = 1;
    let budget = PayloadBudget::new(policy).expect("budget");
    let winner = Arc::new(Winner::default());
    let stages = HybridStages {
        reranker: Some(winner.clone()),
        ..HybridStages::default()
    };
    let dense = DenseCache::new();
    let context = tools::SearchContext {
        stages: &stages,
        dense: &dense,
        cache: f.cache.path(),
        project: &f.project,
        window: 40,
        payload_budget: &budget,
        operation: SearchOperation::new(budget.settings(), Instant::now(), Cancellation::new())
            .expect("operation"),
    };
    let result = tools::search_docs_in(&context, &f.db, &args());
    assert!(matches!(result, Err(Error::Admission { .. })), "{result:?}");
    assert_eq!(winner.inputs.lock().expect("inputs").len(), 0);
    drop(budget.reserve(1).expect("released"));
}

#[test]
fn unfiltered_rrf_uses_one_dense_source_and_scoped_single_doc_proves_exhaustion() {
    for others in [0, 10] {
        let f = Fixture::new(40, others);
        let geometry = Arc::new(Geometry::default());
        docsbase_memory::hybrid::jobs::embed_project_chunks_with(
            &f.db,
            geometry.as_ref(),
            f.project.id,
        )
        .expect("vectors");
        let stages = HybridStages {
            embedder: Some(geometry.clone()),
            ranking: Ranking::Rrf,
            reranker: None,
        };
        let mut query = args();
        if others == 0 {
            query["path_prefix"] = json!("guide/");
        }
        let result = f.search(&stages, &query);
        assert_eq!(hits(&result).len(), if others == 0 { 2 } else { 10 });
        assert_eq!(
            result["meta"]["refill"]["stop_reason"],
            if others == 0 {
                "source-exhausted"
            } else {
                "satisfied"
            }
        );
        assert_cap(&result, 2);
        assert_eq!(geometry.queries.load(Ordering::SeqCst), 1);
    }
}

#[test]
#[ignore = "manual release-only local refill diagnostic; no network or thresholds"]
// This ignored diagnostic must compile in debug, but reject a debug execution.
#[allow(clippy::assertions_on_constants)]
fn release_refill_diagnostics() {
    assert!(!cfg!(debug_assertions), "run with --release");
    for (label, chunks, others) in [
        ("no-refill", 1, 10),
        ("refill", 40, 10),
        ("budget", 1010, 10),
    ] {
        let f = Fixture::new(chunks, others);
        let geometry = Arc::new(Geometry::default());
        docsbase_memory::hybrid::jobs::embed_project_chunks_with(
            &f.db,
            geometry.as_ref(),
            f.project.id,
        )
        .expect("vectors");
        for rrf in [false, true] {
            let stages = HybridStages {
                embedder: rrf.then(|| geometry.clone() as Arc<dyn Embedder>),
                ranking: if rrf { Ranking::Rrf } else { Ranking::Lexical },
                reranker: None,
            };
            let dense = DenseCache::new();
            for phase in ["first", "warm"] {
                let started = Instant::now();
                let mut query = args();
                query["path_prefix"] = json!("guide/");
                let result = tools::search_docs_with(
                    &stages,
                    &dense,
                    &f.db,
                    f.cache.path(),
                    &f.project,
                    &query,
                    40,
                )
                .expect("diagnostic");
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                println!(
                    "case={label} chunks={chunks} docs={} rrf={rrf} phase={phase} ms={ms:.3} hits={} window={} refill={}",
                    others + 1,
                    hits(&result).len(),
                    result["meta"]["window"],
                    result["meta"]["refill"]
                );
            }
        }
    }
}
