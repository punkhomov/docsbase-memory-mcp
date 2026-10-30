//! `DenseCache` ANN integration (spec ann-index T4): threshold gating, exact
//! fallback, background build, LRU and cached `TooLarge` hashes.

#![allow(clippy::cast_precision_loss, clippy::field_reassign_with_default)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools::run_project_index};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore, chunk_set_digest};
use docsbase_memory::hybrid::dense::{
    AnnPolicy, DenseCache, DenseCacheLimits, DenseIndex, DenseRows,
};
use docsbase_memory::hybrid::jobs::embed_project_chunks_with;
use docsbase_memory::store::Db;
use docsbase_memory::store::repo;
use docsbase_memory::vector_cache::{Embedder, VectorCache};

fn project_with_files(cache: &Path, names: &[&str]) -> (Db, i64) {
    let root = TempDir::new().expect("root");
    for (i, name) in names.iter().enumerate() {
        fs::write(
            root.path().join(name),
            format!("# Doc {i}\n\nbody with widgets {i}.\n"),
        )
        .expect("write doc");
    }
    let mut db = Db::open(cache).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let project = registry::ensure_project(&mut db, &canonical).expect("project");
    run_project_index(&mut db, cache, &project, &Config::default()).expect("index");
    (db, project.id)
}

struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "fake/model@d3@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| vec![text.len() as f32, 0.5, 0.25])
            .collect())
    }
}

fn limits_with_min_vectors(min_vectors: usize) -> DenseCacheLimits {
    let mut limits = DenseCacheLimits::default();
    limits.min_vectors = min_vectors;
    limits
}

fn ready(dense: &DenseCache, db: &Db, project_id: i64, model: &str) -> Arc<DenseIndex> {
    match dense.get(db, project_id, model).expect("get") {
        DenseRows::Ready(index) => index,
        other => panic!("expected Ready, got {other:?}"),
    }
}

fn wait_for_ann(dense: &DenseCache, db: &Db, project_id: i64, model: &str) -> Arc<AnnIndex> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match dense.get(db, project_id, model).expect("get") {
            DenseRows::Ann(ann) => return ann,
            other if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
                let _ = other;
            }
            other => panic!("no ANN within timeout: {other:?}"),
        }
    }
}

fn wait_for_failed(dense: &DenseCache, db: &Db, project_id: i64, model: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match dense.get(db, project_id, model).expect("get") {
            DenseRows::Failed(_) => return,
            _ if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => panic!("no Failed state within timeout: {other:?}"),
        }
    }
}

fn ann_key(cache: &Path, db: &Db, project_id: i64, model: &str) -> AnnKey {
    let chunks = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
    let mut cache = VectorCache::open(cache).expect("cache");
    let first = chunks
        .iter()
        .find_map(|(id, hash)| {
            cache
                .get(model, hash)
                .expect("get")
                .map(|vector| (id, vector.len()))
        })
        .expect("vectors");
    AnnKey {
        project_id,
        generation: repo::index_generation(db.connection(), project_id).expect("generation"),
        model_id: model.to_owned(),
        dim: first.1,
        chunk_count: chunks.len(),
        chunk_set_digest: chunk_set_digest(&chunks),
        format_version: ANN_FORMAT_VERSION,
    }
}

#[test]
fn ann_is_built_in_background_and_published() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    let stored = embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");
    assert!(stored >= 3);

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::BuildOrLoad,
        cache.path(),
        limits_with_min_vectors(2),
    );
    let first = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(first, DenseRows::Building(_)),
        "build starts async"
    );

    let ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    let cache_hit = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(matches!(cache_hit, DenseRows::Ann(again) if Arc::ptr_eq(&ann, &again)));
    assert!(ann.estimated_bytes() > 0);

    let store = AnnStore::new(cache.path().to_path_buf());
    let key = ann_key(cache.path(), &db, project_id, FakeEmbedder.model_id());
    let slot = store.load(&key).expect("load").expect("slot");
    assert!(slot.dir.join("READY").is_file());
}

#[test]
fn below_threshold_stays_exact() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::BuildOrLoad,
        cache.path(),
        limits_with_min_vectors(1_000),
    );
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(out, DenseRows::Ready(_)),
        "below threshold: {out:?}"
    );
    let store = AnnStore::new(cache.path().to_path_buf());
    assert!(
        store
            .load(&ann_key(
                cache.path(),
                &db,
                project_id,
                FakeEmbedder.model_id()
            ))
            .expect("load")
            .is_none()
    );
}

#[test]
fn byte_cap_disables_ann() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let mut limits = limits_with_min_vectors(2);
    limits.max_bytes = 64;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(matches!(out, DenseRows::Ready(_)), "byte cap: {out:?}");
}

#[test]
fn incomplete_coverage_falls_back_to_exact() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    let chunks = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
    assert!(chunks.len() >= 3);
    let mut vcache = VectorCache::open(cache.path()).expect("cache");
    for (id, hash) in &chunks[..2] {
        vcache
            .put(FakeEmbedder.model_id(), hash, &[*id as f32, 0.5, 0.25])
            .expect("put");
    }
    drop(vcache);

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::BuildOrLoad,
        cache.path(),
        limits_with_min_vectors(2),
    );
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(out, DenseRows::Ready(_)),
        "partial coverage: {out:?}"
    );
}

#[test]
fn ann_load_failure_falls_back_to_exact() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let store = AnnStore::new(cache.path().to_path_buf());
    let key = ann_key(cache.path(), &db, project_id, FakeEmbedder.model_id());
    store
        .persist_dump(&key, |dir| {
            fs::write(dir.join("index.hnsw.graph"), b"garbage").expect("graph");
            fs::write(dir.join("index.hnsw.data"), b"garbage").expect("data");
            Ok("index".to_owned())
        })
        .expect("persist");

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::LoadOnly,
        cache.path(),
        limits_with_min_vectors(2),
    );
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(matches!(out, DenseRows::Ready(_)), "corrupt ann: {out:?}");
}

#[test]
fn stale_generation_rebuilds_under_a_new_key() {
    let cache = TempDir::new().expect("cache");
    let (mut db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::BuildOrLoad,
        cache.path(),
        limits_with_min_vectors(2),
    );
    let ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    let old_key = ann_key(cache.path(), &db, project_id, FakeEmbedder.model_id());

    {
        let tx = db.begin_write().expect("tx");
        repo::bump_index_generation(&tx, project_id).expect("bump");
        tx.commit().expect("commit");
    }
    let new_ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    assert!(!Arc::ptr_eq(&ann, &new_ann));
    let new_key = ann_key(cache.path(), &db, project_id, FakeEmbedder.model_id());
    assert_ne!(old_key.generation, new_key.generation);
    let store = AnnStore::new(cache.path().to_path_buf());
    assert!(store.load(&new_key).expect("load").is_some());
}

#[test]
fn too_large_streams_with_cached_hashes() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    let chunks = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
    let mut vcache = VectorCache::open(cache.path()).expect("cache");
    for (id, hash) in &chunks {
        vcache
            .put(FakeEmbedder.model_id(), hash, &[*id as f32, 0.5, 0.25])
            .expect("put");
    }
    drop(vcache);

    let mut limits = DenseCacheLimits::default();
    limits.max_vectors = 1;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let first = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    let DenseRows::TooLarge(hashes) = first else {
        panic!("expected TooLarge")
    };
    assert_eq!(hashes.len(), chunks.len());
    let second = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    let DenseRows::TooLarge(again) = second else {
        panic!("expected TooLarge")
    };
    assert!(Arc::ptr_eq(&hashes, &again), "hashes cached between calls");
}

#[test]
fn lru_evicts_the_oldest_project() {
    let cache = TempDir::new().expect("cache");
    let (db, first_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    let (_, second_id) = project_with_files(cache.path(), &["d.md", "e.md", "f.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, first_id).expect("embed");
    embed_project_chunks_with(&db, &FakeEmbedder, second_id).expect("embed");

    let mut limits = DenseCacheLimits::default();
    limits.max_projects = 1;
    limits.min_vectors = 1_000;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);

    let first = dense
        .get(&db, first_id, FakeEmbedder.model_id())
        .expect("get");
    let DenseRows::Ready(index) = first else {
        panic!("expected Ready")
    };
    let second = dense
        .get(&db, second_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(matches!(second, DenseRows::Ready(_)));

    let again = dense
        .get(&db, first_id, FakeEmbedder.model_id())
        .expect("get");
    let DenseRows::Ready(rebuilt) = again else {
        panic!("expected Ready")
    };
    assert!(
        !Arc::ptr_eq(&index, &rebuilt),
        "oldest project evicted and rebuilt"
    );
}

#[test]
fn disabled_policy_never_touches_the_store() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let dense = DenseCache::new();
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(matches!(out, DenseRows::Ready(_)));
    assert!(!cache.path().join("projects/1/ann").exists());
}

#[test]
fn load_only_never_builds() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::LoadOnly,
        cache.path(),
        limits_with_min_vectors(2),
    );
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(out, DenseRows::Ready(_)),
        "CLI never builds: {out:?}"
    );
    assert!(!cache.path().join("projects/1/ann").exists());
}

#[test]
fn warm_builds_in_background_and_is_idempotent() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let dense = DenseCache::with_policy_limits(
        AnnPolicy::BuildOrLoad,
        cache.path(),
        limits_with_min_vectors(2),
    );
    dense
        .warm(&db, project_id, FakeEmbedder.model_id())
        .expect("warm 1");
    dense
        .warm(&db, project_id, FakeEmbedder.model_id())
        .expect("warm 2");
    let ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    assert!(ann.estimated_bytes() > 0);
    dense
        .warm(&db, project_id, FakeEmbedder.model_id())
        .expect("warm after publish");
}

#[test]
fn byte_cap_streams_exact_rows_instead_of_loading_them() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);

    let mut limits = DenseCacheLimits::default();
    limits.max_bytes = 1;
    limits.min_vectors = 1_000;
    let dense = DenseCache::with_policy_limits(AnnPolicy::Disabled, cache.path(), limits);
    let out = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("get");
    assert!(
        matches!(out, DenseRows::TooLarge(_)),
        "over byte cap must stream exact, got {out:?}"
    );
    assert!(
        !cache.path().join("vectors.db").exists(),
        "the byte-cap check must run before decoding any vector"
    );
}

#[test]
fn global_byte_cap_evicts_the_oldest_entry() {
    let cache = TempDir::new().expect("cache");
    let (db, first_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    let (_, second_id) = project_with_files(cache.path(), &["d.md", "e.md", "f.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, first_id).expect("embed first");
    embed_project_chunks_with(&db, &FakeEmbedder, second_id).expect("embed second");

    let mut limits = DenseCacheLimits::default();
    limits.max_bytes = 60;
    limits.min_vectors = 1_000;
    let dense = DenseCache::with_policy_limits(AnnPolicy::Disabled, cache.path(), limits);

    let first = ready(&dense, &db, first_id, FakeEmbedder.model_id());
    let _second = ready(&dense, &db, second_id, FakeEmbedder.model_id());
    let first_again = ready(&dense, &db, first_id, FakeEmbedder.model_id());
    assert!(
        !Arc::ptr_eq(&first, &first_again),
        "global byte cap must evict the oldest project"
    );
}

#[test]
fn failed_build_retries_after_backoff() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let blocker = cache.path().join("ann-store");
    fs::write(&blocker, b"block the ann tree").expect("blocker");
    let mut limits = limits_with_min_vectors(2);
    // The Failed window must be observable under a loaded CI runner: `get`
    // starts a new build as soon as retry_at passes, so a millisecond-scale
    // backoff races the poll and flakes. Five seconds is a wide window and
    // still keeps the test fast.
    limits.build_backoff = Duration::from_secs(5);
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, &blocker, limits);

    let first = dense
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("first get");
    assert!(matches!(first, DenseRows::Building(_)), "{first:?}");
    wait_for_failed(&dense, &db, project_id, FakeEmbedder.model_id());
    fs::remove_file(&blocker).expect("unblock");

    let ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    assert!(ann.estimated_bytes() > 0);
}

#[test]
fn cold_start_loads_persisted_ann_without_vectors_db() {
    let cache = TempDir::new().expect("cache");
    let (db, project_id) = project_with_files(cache.path(), &["a.md", "b.md", "c.md"]);
    embed_project_chunks_with(&db, &FakeEmbedder, project_id).expect("embed");

    let mut limits = limits_with_min_vectors(2);
    limits.max_bytes = 1024 * 1024;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let _ann = wait_for_ann(&dense, &db, project_id, FakeEmbedder.model_id());
    drop(dense);
    drop(db);

    // This removal is the regression check for the publish/lease ordering in
    // `build_in_background`: `wait_for_ann` only sees `Ann` once the build
    // thread has released its `vectors.db` handle. Publishing before the lease
    // release made this fail on Windows with a sharing violation while Linux
    // unlinks the open file happily (CI run 41043371926).
    for suffix in ["", "-wal", "-shm"] {
        let path = cache.path().join(format!("vectors.db{suffix}"));
        if path.exists() {
            fs::remove_file(path).expect("remove vectors db");
        }
    }

    let db = Db::open(cache.path()).expect("db");
    let mut limits = limits_with_min_vectors(2);
    limits.max_bytes = 1024 * 1024;
    let cold = DenseCache::with_policy_limits(AnnPolicy::LoadOnly, cache.path(), limits);
    let out = cold
        .get(&db, project_id, FakeEmbedder.model_id())
        .expect("cold get");
    assert!(
        matches!(out, DenseRows::Ann(_)),
        "cold start must load persisted ANN, got {out:?}"
    );
    assert!(
        !cache.path().join("vectors.db").exists(),
        "cold start must not open the vector cache"
    );
}

/// OQ-4 evidence: a generation directory must be removable after the mmap
/// index that referenced it is dropped. Windows holds the mapping open until
/// then, so this is the runtime half that `cargo check` cannot prove.
#[test]
fn cleanup_removes_a_generation_after_the_loaded_index_is_dropped() {
    let cache = TempDir::new().expect("cache");
    let store = AnnStore::new(cache.path().to_path_buf());
    let dim = 8;
    let rows: Vec<(u64, Arc<[f32]>)> = (0..64)
        .map(|id| (id, Arc::from(vec![0.1_f32; dim])))
        .collect();
    let key_for = |generation: i64| AnnKey {
        project_id: 1,
        generation,
        model_id: "test/model@d8".to_owned(),
        dim,
        chunk_count: rows.len(),
        chunk_set_digest: format!("digest-{generation}"),
        format_version: ANN_FORMAT_VERSION,
    };

    let mut dirs = Vec::new();
    for generation in 1..=3 {
        let ann = AnnIndex::build(&rows, dim).expect("build");
        dirs.push(ann.dump(&store, &key_for(generation)).expect("dump").dir);
    }

    let slot = store.load(&key_for(1)).expect("load").expect("slot");
    let loaded = AnnIndex::load(&slot).expect("load index");
    drop(loaded);

    store.cleanup(1).expect("cleanup");
    assert!(
        !dirs[0].exists(),
        "the oldest generation must be removable after the loaded index is dropped"
    );
    assert!(dirs[2].exists(), "the newest generation is kept");
}
