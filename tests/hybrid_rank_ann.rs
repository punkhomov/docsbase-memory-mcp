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
use docsbase_memory::index::tantivy_index::MAX_HITS;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{Db, repo};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const VECTORS: usize = 600;
/// Small enough that `MAX_HITS` exceeds its chunk count (~40 chunks).
const SMALL_DOCS: usize = 20;
const DIM: usize = 64;

/// Deterministic, spread embedding of the text (FNV-1a seeded, L2-normalized).
///
/// A length-only mapping put every point on one line (dim 0 = length, dim 1
/// constant), i.e. ~1000 near-duplicate points. `hnsw_rs` builds a
/// nondeterministic graph over such a set, and on macOS its search sometimes
/// reached only 37 of the 50 requested neighbours. The read path then fell back
/// to exact — correct behaviour — and the "published ANN must serve" assertion
/// failed for a reason that had nothing to do with publishing. Spread vectors
/// make the graph well connected, so the assertion tests the read path instead
/// of hnsw's luck on degenerate geometry.
struct SpreadEmbedder;

impl Embedder for SpreadEmbedder {
    fn model_id(&self) -> &'static str {
        "rank/model@d64@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| spread(text)).collect())
    }
}

fn spread(text: &str) -> Vec<f32> {
    // FNV-1a over the bytes, then a per-dimension LCG so one hash fills DIM.
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x0100_0000_01b3);
    }
    let mut vector = Vec::with_capacity(DIM);
    for _ in 0..DIM {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        vector.push(((state >> 40) as f32 / 16_777_216.0) - 0.5);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn project_with_docs(count: usize) -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for i in 0..count {
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

fn project_with_many_docs() -> (TempDir, Db, Project) {
    project_with_docs(VECTORS)
}

fn stages() -> HybridStages {
    HybridStages {
        embedder: Some(Arc::new(SpreadEmbedder)),
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
            .get(db, project_id, SpreadEmbedder.model_id())
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
    let stored = embed_project_chunks_with(&db, &SpreadEmbedder, project.id).expect("embed");
    assert!(
        stored >= VECTORS as u64,
        "every doc produced at least one chunk"
    );

    let mut limits = DenseCacheLimits::default();
    limits.min_vectors = 2;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);

    let first = dense
        .get(&db, project.id, SpreadEmbedder.model_id())
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
    // Determinism, not path order: the length-only embedder ranked documents
    // monotonically by path, so a lexicographic check passed by accident.
    let during_build_repeat = search(
        &search_docs_with(
            &stages(),
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 10 }),
            50,
        )
        .expect("search during build, repeat"),
    );
    assert_eq!(
        during_build, during_build_repeat,
        "the exact path answers deterministically while the build is in flight"
    );

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
}

/// A fetch the graph cannot satisfy must fall back to exact and must not be
/// counted as an ANN-served lookup (Delta 0.3.2).
///
/// The window is clamped to `MAX_HITS` (1000) before the dense stage sees it,
/// so "more than the graph holds" is only reachable on a corpus smaller than
/// that clamp. Asking a 12k-chunk project for `chunks + 1` does not test this:
/// the request becomes 1000 and the graph answers it — which is exactly how the
/// old assertion flaked, since it depended on whether the build had published.
#[cfg(debug_assertions)]
#[test]
fn an_unsatisfiable_fetch_falls_back_without_counting() {
    let (cache, db, project) = project_with_docs(SMALL_DOCS);
    embed_project_chunks_with(&db, &SpreadEmbedder, project.id).expect("embed");

    let mut limits = DenseCacheLimits::default();
    limits.min_vectors = 2;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let _ann = wait_for_ann(&dense, &db, project.id);

    let chunks = repo::project_counts(db.connection(), project.id)
        .expect("project counts")
        .chunks;
    assert!(
        usize::try_from(chunks).expect("chunk count fits") < MAX_HITS,
        "the clamp must exceed this corpus, got {chunks} chunks"
    );

    let before = ann_search_count();
    search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        u32::try_from(MAX_HITS).expect("MAX_HITS fits"),
    )
    .expect("search with a fetch the graph cannot satisfy");
    assert_eq!(
        ann_search_count(),
        before,
        "a fallback answer must not count as an ANN-served lookup"
    );
}
