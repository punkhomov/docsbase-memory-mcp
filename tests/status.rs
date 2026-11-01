//! `status` telemetry: the ANN tier state and the index generation
//! (spec `search-telemetry`, FR-5/FR-6).

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::session::Stats;
use docsbase_memory::daemon::tools::{run_project_index, search_docs_with, status};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
use docsbase_memory::hybrid::jobs::embed_project_chunks_with;
use docsbase_memory::hybrid::meta::AnnState;
use docsbase_memory::hybrid::{HybridStages, Ranking};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const DOCS: &[(&str, &str)] = &[
    ("a.md", "# Alpha\n\nalpha alpha widgets\n"),
    ("b.md", "# Beta\n\nalpha gear\n"),
];

/// Two-dimensional geometry: everything with "alpha" points at `[1, 0]`.
struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "fake/model@d2@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("alpha") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }
}

fn project() -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for (rel, body) in DOCS {
        std::fs::write(root.path().join(rel), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
    run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
    project.status = ProjectStatus::Indexed;
    (cache, db, project)
}

/// Extracts the single project entry from a `status` value.
fn entry(value: &Value) -> &Value {
    value["projects"]
        .as_array()
        .expect("projects")
        .first()
        .expect("one project")
}

#[test]
fn status_without_a_daemon_reports_unknown_not_disabled() {
    let (_cache, db, project) = project();
    let value =
        status(&db, &Stats::default(), 0, None, &|_| false, &|_| None, None).expect("status");
    let ann = &entry(&value)["ann"];
    assert_eq!(
        ann["state"],
        AnnState::Unknown.as_str(),
        "a caller that cannot see the daemon must not claim the tier is off"
    );
    assert_eq!(ann["reason"], "daemon not running");
    assert!(ann["vectors"].is_null() && ann["estimated_bytes"].is_null());
    assert_eq!(
        entry(&value)["index_generation"],
        json!(
            docsbase_memory::store::repo::index_generation(db.connection(), project.id)
                .expect("generation")
        ),
        "status reports the generation the index is at"
    );
}

#[test]
fn status_reports_ready_after_an_ann_search() {
    let (cache, db, project) = project();
    embed_project_chunks_with(&db, &FakeEmbedder, project.id).expect("embed");
    let limits = DenseCacheLimits {
        min_vectors: 1,
        ..DenseCacheLimits::default()
    };
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let stages = HybridStages {
        embedder: Some(std::sync::Arc::new(FakeEmbedder)),
        reranker: None,
        ranking: Ranking::Rrf,
    };
    search_docs_with(
        &stages,
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 2 }),
        2,
    )
    .expect("search");
    // The first search starts the build; wait for it to publish, then search
    // again so the recorded outcome is `ready` rather than `building`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if matches!(
            dense
                .get(&db, project.id, FakeEmbedder.model_id())
                .expect("get"),
            DenseRows::Ann(_)
        ) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    search_docs_with(
        &stages,
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 2 }),
        2,
    )
    .expect("search after publish");

    let value = status(
        &db,
        &Stats::default(),
        0,
        None,
        &|_| false,
        &|_| {
            Some(docsbase_memory::hybrid::jobs::Coverage {
                cached: 2,
                total: 2,
            })
        },
        Some(&dense),
    )
    .expect("status");
    let ann = &entry(&value)["ann"];
    assert_eq!(
        ann["state"],
        AnnState::Ready.as_str(),
        "the published ANN served the second search: {ann}"
    );
    assert!(ann["reason"].is_null(), "a ready tier has no reason: {ann}");
    assert!(
        ann["vectors"].as_u64().unwrap_or_default() > 0,
        "a resident ANN reports its size: {ann}"
    );
}

#[test]
fn status_reports_disabled_when_hybrid_is_off() {
    let (cache, db, _project) = project();
    let dense = DenseCache::with_policy(AnnPolicy::BuildOrLoad, cache.path());
    let value = status(
        &db,
        &Stats::default(),
        0,
        None,
        &|_| false,
        &|_| None,
        Some(&dense),
    )
    .expect("status");
    let ann = &entry(&value)["ann"];
    assert_eq!(ann["state"], AnnState::Disabled.as_str());
    assert_eq!(ann["reason"], "hybrid off");
}

#[test]
fn status_reports_loading_when_hybrid_is_on_but_no_search_ran() {
    let (cache, db, _project) = project();
    let dense = DenseCache::with_policy(AnnPolicy::BuildOrLoad, cache.path());
    let value = status(
        &db,
        &Stats::default(),
        0,
        None,
        &|_| false,
        &|_| {
            Some(docsbase_memory::hybrid::jobs::Coverage {
                cached: 0,
                total: 2,
            })
        },
        Some(&dense),
    )
    .expect("status");
    let ann = &entry(&value)["ann"];
    assert_eq!(ann["state"], AnnState::Loading.as_str());
    assert_eq!(ann["reason"], "no search yet");
}

#[test]
fn a_recorded_fallback_names_its_reason() {
    let (cache, db, _project) = project();
    let dense = DenseCache::with_policy(AnnPolicy::BuildOrLoad, cache.path());
    dense.record_outcome(1, AnnState::Fallback, Some("ann search failed"));
    let value = status(
        &db,
        &Stats::default(),
        0,
        None,
        &|_| false,
        &|_| {
            Some(docsbase_memory::hybrid::jobs::Coverage {
                cached: 1,
                total: 1,
            })
        },
        Some(&dense),
    )
    .expect("status");
    let ann = &entry(&value)["ann"];
    assert_eq!(ann["state"], AnnState::Fallback.as_str());
    assert_eq!(ann["reason"], "ann search failed");
}
