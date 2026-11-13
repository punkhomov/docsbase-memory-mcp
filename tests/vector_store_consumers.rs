//! Real owner-backed jobs and retrieval; any accidental legacy open conflicts
//! with the live exclusive owner, so success proves connection propagation.
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::dense::AnnPolicy;
use docsbase_memory::hybrid::dense::{DenseCache, DenseCacheLimits, DenseRequest, DenseRows};
use docsbase_memory::hybrid::dense_metadata::DenseMetadata;
use docsbase_memory::hybrid::dense_settings::DenseSettings;
use docsbase_memory::hybrid::jobs;
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::hybrid::{HybridStages, Ranking};
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{Db, repo};
use docsbase_memory::vector_cache::settings::VectorStoreSettings;
use docsbase_memory::vector_cache::{
    CacheLimits, Embedder, VectorCache, VectorCacheSource, VectorStore,
};
use serde_json::{Value, json};

fn alias_path(root: &Path) -> std::path::PathBuf {
    let child = root.join("alias-child");
    fs::create_dir(&child).unwrap();
    child.join("..")
}

fn store_settings() -> VectorStoreSettings {
    docsbase_memory::vector_cache::settings::standalone()
        .unwrap()
        .settings()
        .clone()
}
fn dense_settings() -> DenseSettings {
    docsbase_memory::hybrid::dense_settings::standalone()
        .unwrap()
        .settings()
        .clone()
}

#[derive(Default)]
struct Geometry {
    calls: AtomicUsize,
    queries: AtomicUsize,
}
impl Embedder for Geometry {
    fn model_id(&self) -> &'static str {
        "consumer/model@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1.0, 0.0])
    }
}

struct Fixture {
    db: Db,
    owner: VectorStore,
    project: Project,
    _root: tempfile::TempDir,
    cache: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for (name, body) in [("a.md", "alpha"), ("b.md", "beta"), ("c.md", "gamma")] {
            fs::write(root.path().join(name), body).unwrap();
        }
        let mut db = Db::open(cache.path()).unwrap();
        let mut project = registry::ensure_project(&mut db, root.path()).unwrap();
        tools::run_project_index(&mut db, cache.path(), &project, &Config::default()).unwrap();
        project.status = ProjectStatus::Indexed;
        let owner = VectorStore::acquire(cache.path(), &store_settings()).unwrap();
        Self {
            db,
            owner,
            project,
            _root: root,
            cache,
        }
    }
    fn source(&self) -> VectorCacheSource {
        VectorCacheSource::Owned(self.owner.clone())
    }
    fn embed(&self, embedder: &dyn Embedder) {
        jobs::embed_project_chunks_from(
            &self.db,
            embedder,
            self.project.id,
            false,
            &DenseCacheLimits::default(),
            &self.source(),
        )
        .unwrap();
    }
    fn dense(&self, cap: usize) -> DenseCache {
        let settings = DenseSettings {
            max_vectors: cap,
            ..dense_settings()
        };
        DenseCache::with_vector_store(AnnPolicy::Disabled, &self.owner, &settings).unwrap()
    }
}

#[test]
fn source_checks_root_without_creating_or_falling_back() {
    let fixture = Fixture::new();
    let alias = alias_path(fixture.cache.path());
    assert!(fixture.source().connect(&alias).is_ok());
    let other = tempfile::tempdir().unwrap();
    assert!(matches!(
        fixture.source().connect(other.path()),
        Err(Error::Admission { .. })
    ));
    assert!(!other.path().join("vectors.db").exists());
    let missing = other.path().join("missing");
    assert!(fixture.source().connect(&missing).is_err());
    assert!(!missing.exists());
    assert!(matches!(
        VectorCache::open(fixture.cache.path()),
        Err(Error::Admission { .. })
    ));
}

#[test]
fn jobs_coverage_and_two_projects_reuse_one_owner() {
    let mut fixture = Fixture::new();
    let geometry = Geometry::default();
    let before = jobs::coverage_for_key_from(
        &fixture.db,
        geometry.model_id(),
        fixture.project.id,
        &fixture.source(),
    )
    .unwrap();
    assert_eq!((before.cached, before.total), (0, 3));
    fixture.embed(&geometry);
    fixture.embed(&geometry);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("copy.md"), "alpha").unwrap();
    let second = registry::ensure_project(&mut fixture.db, root.path()).unwrap();
    tools::run_project_index(
        &mut fixture.db,
        fixture.cache.path(),
        &second,
        &Config::default(),
    )
    .unwrap();
    jobs::embed_project_chunks_from(
        &fixture.db,
        &geometry,
        second.id,
        false,
        &DenseCacheLimits::default(),
        &fixture.source(),
    )
    .unwrap();
    assert_eq!(
        geometry.calls.load(Ordering::SeqCst),
        1,
        "content reused across passes/projects"
    );
    for (project, total) in [(fixture.project.id, 3), (second.id, 1)] {
        let result = jobs::coverage_for_key_from(
            &fixture.db,
            geometry.model_id(),
            project,
            &fixture.source(),
        )
        .unwrap();
        assert_eq!((result.cached, result.total), (total, total));
    }
}

#[test]
fn dense_cold_shared_query_copy_and_root_identity() {
    let fixture = Fixture::new();
    let geometry = Geometry::default();
    fixture.embed(&geometry);
    let dense = fixture.dense(10);
    let copy = dense.sharing_query_cache();
    for cache in [&dense, &copy] {
        match cache
            .get(&fixture.db, fixture.project.id, geometry.model_id())
            .unwrap()
        {
            DenseRows::Ready(rows) => assert_eq!(rows.rows().len(), 3),
            _ => panic!("expected owned cold rows"),
        }
    }
    let alias = alias_path(fixture.cache.path());
    let request = DenseRequest {
        cache_root: &alias,
        project_id: fixture.project.id,
        generation: repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap(),
        model_id: geometry.model_id(),
    };
    assert!(
        fixture
            .dense(10)
            .get_with_metadata(request, &Cancellation::new(), || DenseMetadata::capture(
                &fixture.db,
                fixture.project.id
            ))
            .is_ok()
    );
    let wrong = tempfile::tempdir().unwrap();
    let loads = AtomicUsize::new(0);
    let error = copy.get_with_metadata(
        DenseRequest {
            cache_root: wrong.path(),
            ..request
        },
        &Cancellation::new(),
        || {
            loads.fetch_add(1, Ordering::SeqCst);
            DenseMetadata::capture(&fixture.db, fixture.project.id)
        },
    );
    assert!(matches!(error, Err(Error::Admission { .. })));
    assert_eq!(loads.load(Ordering::SeqCst), 0);
}

#[test]
fn strict_budget_refusal_and_legacy_zero_semantics() {
    let fixture = Fixture::new();
    let dense = fixture.dense(10);
    for cap in [0, 1, fixture.owner.settings().max_bytes] {
        assert!(matches!(
            dense.set_vector_cache_max_bytes(fixture.project.id, cap),
            Err(Error::Admission { .. })
        ));
    }
    let root = tempfile::tempdir().unwrap();
    let legacy = DenseCache::new();
    legacy.set_vector_cache_max_bytes(1, 0).unwrap();
    let mut connection = legacy.vector_source(1).connect(root.path()).unwrap();
    connection.put("m", "h", &[1.0, 0.0]).unwrap();
    assert!(connection.get("m", "h").unwrap().is_none());
    let mut owned = dense
        .vector_source(fixture.project.id)
        .connect(fixture.cache.path())
        .unwrap();
    owned.put("m", "h", &[1.0, 0.0]).unwrap();
    assert!(owned.get("m", "h").unwrap().is_some());
}

fn search(fixture: &Fixture, dense: &DenseCache, stages: &HybridStages, args: &Value) -> Value {
    tools::search_docs_with(
        stages,
        dense,
        &fixture.db,
        fixture.cache.path(),
        &fixture.project,
        args,
        40,
    )
    .unwrap()
}

#[test]
fn actual_full_scoped_and_streaming_search_use_owned_vectors() {
    let fixture = Fixture::new();
    let geometry = Arc::new(Geometry::default());
    fixture.embed(geometry.as_ref());
    let stages = HybridStages {
        embedder: Some(geometry),
        ranking: Ranking::Rrf,
        reranker: None,
    };
    let args = json!({"query":"semanticprobe", "limit":3});
    let full = search(&fixture, &fixture.dense(10), &stages, &args);
    assert_eq!(
        full.as_array().unwrap().len(),
        3,
        "query has no lexical matches"
    );
    let streamed = search(&fixture, &fixture.dense(1), &stages, &args);
    assert_eq!(streamed, full, "streaming preserves actual fused results");
    let scoped = search(
        &fixture,
        &fixture.dense(10),
        &stages,
        &json!({"query":"semanticprobe", "limit":3, "path_prefix":"b.md"}),
    );
    assert_eq!(scoped.as_array().unwrap().len(), 1);
    assert_eq!(scoped[0]["path"], "b.md");
}

#[test]
fn search_rejects_wrong_owner_before_query_provider() {
    let fixture = Fixture::new();
    let other = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(other.path(), &store_settings()).unwrap();
    let dense =
        DenseCache::with_vector_store(AnnPolicy::Disabled, &owner, &dense_settings()).unwrap();
    let geometry = Arc::new(Geometry::default());
    let stages = HybridStages {
        embedder: Some(geometry.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    };
    for args in [
        json!({"query":"semanticprobe"}),
        json!({"query":"semanticprobe", "path_prefix":"b.md"}),
    ] {
        assert!(matches!(
            tools::search_docs_with(
                &stages,
                &dense,
                &fixture.db,
                fixture.cache.path(),
                &fixture.project,
                &args,
                40
            ),
            Err(Error::Admission { .. })
        ));
    }
    assert_eq!(geometry.queries.load(Ordering::SeqCst), 0);
}

struct TransactionProbe<'a>(&'a Path);
impl Embedder for TransactionProbe<'_> {
    fn model_id(&self) -> &'static str {
        "probe@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let connection = rusqlite::Connection::open(self.0.join("vectors.db")).unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        connection
            .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
            .unwrap();
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[test]
fn backfill_and_text_embed_release_sql_before_provider() {
    let fixture = Fixture::new();
    let probe = TransactionProbe(fixture.cache.path());
    let limits = docsbase_memory::hybrid::dense::DenseCacheLimits {
        min_vectors: 1,
        ..Default::default()
    };
    jobs::embed_project_chunks_from(
        &fixture.db,
        &probe,
        fixture.project.id,
        true,
        &limits,
        &fixture.source(),
    )
    .unwrap();
    assert_eq!(
        rusqlite::Connection::open(fixture.cache.path().join("vectors.db"))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM vector_cache_leases WHERE model_id = ?1",
                [probe.model_id()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    jobs::embed_texts_from(
        fixture.cache.path(),
        &probe,
        &["extra".into()],
        &fixture.source(),
    )
    .unwrap();
    let other = tempfile::tempdir().unwrap();
    let geometry = Geometry::default();
    assert!(
        jobs::embed_texts_from(other.path(), &geometry, &["x".into()], &fixture.source()).is_err()
    );
    assert_eq!(geometry.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn source_preserves_global_eviction_and_explicit_legacy_adapter() {
    let root = tempfile::tempdir().unwrap();
    let settings = VectorStoreSettings {
        max_entries: 1,
        max_bytes: 8,
        ..store_settings()
    };
    let owner = VectorStore::acquire(root.path(), &settings).unwrap();
    let source = VectorCacheSource::Owned(owner.clone());
    let geometry = Geometry::default();
    for text in ["first", "second"] {
        jobs::embed_texts_from(root.path(), &geometry, &[text.into()], &source).unwrap();
    }
    let mut cache = owner.connect().unwrap();
    assert!(
        cache
            .get(
                geometry.model_id(),
                &docsbase_memory::vector_cache::chunk_hash("first")
            )
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .get(
                geometry.model_id(),
                &docsbase_memory::vector_cache::chunk_hash("second")
            )
            .unwrap()
            .is_some()
    );
    let legacy_root = tempfile::tempdir().unwrap();
    jobs::embed_texts_from(
        legacy_root.path(),
        &geometry,
        &["third".into()],
        &VectorCacheSource::Legacy(CacheLimits {
            max_entries: 0,
            max_bytes: 0,
        }),
    )
    .unwrap();
    assert!(
        VectorCache::open(legacy_root.path())
            .unwrap()
            .get(
                geometry.model_id(),
                &docsbase_memory::vector_cache::chunk_hash("third")
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn sharing_query_cache_retains_owner_after_originals_drop() {
    let root = tempfile::tempdir().unwrap();
    let shared = {
        let owner = VectorStore::acquire(root.path(), &store_settings()).unwrap();
        let original =
            DenseCache::with_vector_store(AnnPolicy::Disabled, &owner, &dense_settings()).unwrap();
        original.sharing_query_cache()
    };
    assert!(matches!(
        VectorCache::open(root.path()),
        Err(Error::Admission { .. })
    ));
    shared
        .vector_source(1)
        .connect(root.path())
        .unwrap()
        .put("m", "key", &[1.0])
        .unwrap();
    drop(shared);
    assert!(
        VectorCache::open(root.path())
            .unwrap()
            .get("m", "key")
            .unwrap()
            .is_some()
    );
}

#[test]
fn db_adapters_admit_owned_root_before_reading_generation() {
    let fixture = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let other_db = Db::open(root.path()).unwrap();
    let dense = fixture.dense(10);
    // The other registry has no project with this ID. A SELECT before root
    // admission would return Internal and hide the ownership violation.
    let get = dense.get(&other_db, fixture.project.id, "m").err().unwrap();
    assert!(matches!(get, Error::Admission { .. }), "{get}");
    let warm = dense.warm(&other_db, fixture.project.id, "m").unwrap_err();
    assert!(matches!(warm, Error::Admission { .. }), "{warm}");
    let legacy = DenseCache::new();
    assert!(matches!(
        legacy.get(&other_db, fixture.project.id, "m"),
        Err(Error::Internal { .. })
    ));
    assert!(matches!(
        legacy.warm(&other_db, fixture.project.id, "m"),
        Err(Error::Internal { .. })
    ));
}
