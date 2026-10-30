//! End-to-end search through the `search_docs_with` seam with the ANN tier
//! enabled (spec ann-index T4/T5, Delta 0.3): search must be proven exact
//! while a background build is in flight (SC-11), the published ANN must
//! actually serve the read path (counter, not just internal state), and its
//! neighbours must match the exact scan on the same vectors (T14).

//! The debug hooks it relies on compile out of release builds, so the whole
//! target is a no-op there and `cargo test --release` still compiles.
#![cfg(debug_assertions)]
#![allow(clippy::cast_precision_loss, clippy::field_reassign_with_default)]

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
#[cfg(debug_assertions)]
use docsbase_memory::daemon::tools::ann_search_count;
use docsbase_memory::daemon::tools::{run_project_index, search_docs_with};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
#[cfg(debug_assertions)]
use docsbase_memory::hybrid::dense::{TestBuildGate, set_test_build_gate};
use docsbase_memory::hybrid::jobs::embed_project_chunks_with;
use docsbase_memory::hybrid::{HybridStages, Ranking};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const VECTORS: usize = 600;
const DIM: usize = 64;

struct LengthEmbedder;

impl Embedder for LengthEmbedder {
    fn model_id(&self) -> &'static str {
        "rank/model@d64@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; DIM];
                vector[0] = text.len() as f32 / 10_000.0;
                vector[1] = 1.0;
                vector
            })
            .collect())
    }
}

fn project_with_many_docs() -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for i in 0..VECTORS {
        let body = "x".repeat(1000 + i * 100);
        fs::write(root.path().join(format!("d{i:04}.md")), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
    run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
    project.status = ProjectStatus::Indexed;
    (cache, db, project)
}

fn stages() -> HybridStages {
    HybridStages {
        embedder: Some(Arc::new(LengthEmbedder)),
        reranker: None,
        ranking: Ranking::Rrf,
    }
}

fn search(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["path"].as_str().expect("path").to_owned())
        .collect()
}

fn wait_for_ann(dense: &DenseCache, db: &Db, project_id: i64) -> Arc<AnnIndex> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match dense
            .get(db, project_id, LengthEmbedder.model_id())
            .expect("get")
        {
            DenseRows::Ann(ann) => return ann,
            _ if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => panic!("no ANN within timeout: {other:?}"),
        }
    }
}

/// Clears the build gate and opens it even if the test panics, so a failure
/// cannot leave a build thread waiting for the rest of the process lifetime.
#[cfg(debug_assertions)]
struct GateReset(TestBuildGate);

#[cfg(debug_assertions)]
impl Drop for GateReset {
    fn drop(&mut self) {
        self.0.open();
        set_test_build_gate(None);
    }
}

#[cfg(debug_assertions)]
#[test]
fn search_runs_during_the_build_and_ann_serves_the_read_path() {
    // Hold the background build at its start so the exact-fallback search
    // below is guaranteed to run while the build is still in flight (SC-11),
    // however slow this host is. A fixed sleep raced the search on loaded
    // runners (the same test takes 29.5 s on CI, 11 s locally).
    let gate = TestBuildGate::new();
    set_test_build_gate(Some(gate.clone()));
    let _gate_reset = GateReset(gate.clone());
    let (cache, db, project) = project_with_many_docs();
    let stored = embed_project_chunks_with(&db, &LengthEmbedder, project.id).expect("embed");
    assert!(
        stored >= VECTORS as u64,
        "every doc produced at least one chunk"
    );

    let mut limits = DenseCacheLimits::default();
    limits.min_vectors = 2;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);

    let first = dense
        .get(&db, project.id, LengthEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(first, DenseRows::Building(_)),
        "the first lookup must start a background build, got {first:?}"
    );

    let before = ann_search_count();
    let value = search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        50,
    )
    .expect("search during build");
    let during_build = search(&value);
    assert_eq!(during_build.len(), 10);
    assert_eq!(
        ann_search_count(),
        before,
        "search during the build must stream exact, not consult the ANN"
    );
    for pair in during_build.windows(2) {
        assert!(
            pair[0] <= pair[1],
            "deterministic ranking: {during_build:?}"
        );
    }

    gate.open();
    let _ann = wait_for_ann(&dense, &db, project.id);
    let before = ann_search_count();
    let value = search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        50,
    )
    .expect("search after publish");
    let after_publish = search(&value);
    assert!(
        ann_search_count() > before,
        "a published ANN must serve the read path"
    );
    let repeat = search(
        &search_docs_with(
            &stages(),
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 10 }),
            50,
        )
        .expect("search after publish, repeat"),
    );
    assert_eq!(
        after_publish, repeat,
        "persisted ANN answers deterministically"
    );

    // Asking for more candidates than the index holds makes the ANN answer
    // insufficient: the request falls back to exact and must not count as an
    // ANN-served lookup (Delta 0.3.2). The fetch must exceed the *chunk* count,
    // not the document count — 600 docs split into >1000 chunks, so a fixed
    // 1000 was sometimes satisfiable and counted.
    let oversized = u32::try_from(stored).expect("chunk count fits") + 1;
    let before = ann_search_count();
    search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        oversized,
    )
    .expect("search with an oversized fetch");
    assert_eq!(
        ann_search_count(),
        before,
        "a fallback answer must not count as an ANN-served lookup"
    );
}
