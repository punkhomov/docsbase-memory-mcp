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
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
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

fn wait_for_ann(dense: &DenseCache, db: &Db, project_id: i64, model: &str) -> Arc<AnnIndex> {
    let deadline = Instant::now() + Duration::from_secs(10);
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
