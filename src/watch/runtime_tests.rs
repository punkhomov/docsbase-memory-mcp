//! Prepared watcher context regressions with real storage and consumer threads.
use super::*;
use crate::hybrid::HybridStages;
use crate::store::repo;
use crate::vector_cache::{Embedder, VectorCacheSource, VectorStore};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Provider(AtomicUsize);
impl Embedder for Provider {
    fn model_id(&self) -> &'static str {
        "watcher@d2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

struct Fixture {
    db: Db,
    project: Project,
    source: VectorCacheSource,
    provider: Arc<Provider>,
    cache: tempfile::TempDir,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::create_dir(root.path().join("bbb")).unwrap();
        fs::write(root.path().join("bbb/b.md"), "betawidget").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        crate::daemon::tools::run_project_index(
            &mut db,
            cache.path(),
            &project,
            &Config::default(),
        )
        .unwrap();
        let project = registry::project_by_id(&db, project.id).unwrap().unwrap();
        let source = VectorCacheSource::Owned(
            VectorStore::acquire(
                cache.path(),
                crate::vector_cache::settings::standalone()
                    .unwrap()
                    .settings(),
            )
            .unwrap(),
        );
        Self {
            db,
            project,
            source,
            provider: Arc::new(Provider::default()),
            cache,
            _root: root,
        }
    }
    fn runtime(&self, config: Config) -> Arc<WatchRuntime> {
        Arc::new(WatchRuntime::new(
            Arc::new(config),
            Arc::new(HybridStages {
                embedder: Some(self.provider.clone()),
                ..HybridStages::default()
            }),
            self.source.clone(),
        ))
    }
    fn consume(&self, paths: Vec<PathBuf>, runtime: Arc<WatchRuntime>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        tx.send(paths).unwrap();
        drop(tx);
        spawn_consumer(
            self.cache.path().to_path_buf(),
            self.project.clone(),
            Arc::new(Mutex::new(WatchContext::Prepared(runtime))),
            self.db.settings().clone(),
            rx,
            Weak::new(),
        )
        .unwrap()
        .join()
        .unwrap();
    }
    fn search(&self, query: &str) -> usize {
        crate::index::tantivy_index::ReadIndex::open(&lifecycle::index_dir(
            self.cache.path(),
            self.project.id,
        ))
        .unwrap()
        .search(query, 10)
        .unwrap()
        .len()
    }
}

#[test]
fn prepared_consumer_uses_owner_and_reuses_stages_on_noop() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(Config::default());
    let path = fixture.project.canonical_root.join("bbb/b.md");
    fs::write(&path, "betawidget9").unwrap();
    fixture.consume(vec![path.clone()], runtime.clone());
    assert_eq!(fixture.search("betawidget9"), 1);
    assert_eq!(fixture.search("betawidget"), 0);
    let coverage = crate::hybrid::jobs::coverage_for_key_from(
        &fixture.db,
        fixture.provider.model_id(),
        fixture.project.id,
        &fixture.source,
    )
    .unwrap();
    assert_eq!((coverage.cached, coverage.total), (1, 1));
    let generation = repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap();
    fixture.consume(vec![path], runtime);
    assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap(),
        generation
    );
}

#[test]
fn prepared_watcher_root_admission_preserves_resources_and_revision() {
    let fixture = Fixture::new();
    let watchers = Arc::new(Watchers::new());
    let runtime = fixture.runtime(Config::default());
    let foreign = tempfile::tempdir().unwrap();
    let missing = foreign.path().join("must-not-create");
    assert!(matches!(
        watchers.ensure_with_runtime(
            &missing,
            &fixture.project,
            runtime.clone(),
            fixture.db.settings(),
        ),
        Err(Error::Admission { .. })
    ));
    assert!(!missing.exists());
    assert_eq!(watchers.count(), 0);
    assert!(
        watchers
            .ensure_with_runtime(
                fixture.cache.path(),
                &fixture.project,
                runtime.clone(),
                fixture.db.settings(),
            )
            .unwrap()
    );
    assert!(matches!(
        watchers.revise_with_runtime(&missing, fixture.project.id, runtime.clone()),
        Err(Error::Admission { .. })
    ));
    let legacy = Arc::new(WatchRuntime::new(
        Arc::new(Config::default()),
        Arc::new(HybridStages::default()),
        VectorCacheSource::Legacy(crate::vector_cache::CacheLimits::default()),
    ));
    assert!(matches!(
        watchers.revise_with_runtime(foreign.path(), fixture.project.id, legacy),
        Err(Error::Admission { .. })
    ));
    // A compatibility call must never silently replace an owner-prepared path.
    assert!(!watchers.revise(fixture.project.id, Arc::new(Config::default())));
    let slot = watchers.map()[&fixture.project.id].context.clone();
    let context = slot.lock().unwrap();
    let WatchContext::Prepared(current) = &*context else {
        panic!("owner-backed context must be preserved");
    };
    assert!(Arc::ptr_eq(current, &runtime));
    assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 0);
}

#[test]
fn queued_prepared_paths_use_revised_ignores_and_provider() {
    let fixture = Fixture::new();
    let before = repo::doc_states(fixture.db.connection(), fixture.project.id).unwrap();
    let old = fixture.runtime(Config::default());
    let ignored = fixture.project.canonical_root.join("bbb/b.md");
    let allowed = fixture.project.canonical_root.join("a.md");
    fs::write(&ignored, "betawidget9").unwrap();
    fs::write(&allowed, "alphawidget9").unwrap();
    let config = Arc::new(Config {
        ignores: vec!["bbb/**".into()],
        ..Config::default()
    });
    let provider = Arc::new(Provider::default());
    let new = Arc::new(WatchRuntime::new(
        config,
        Arc::new(HybridStages {
            embedder: Some(provider.clone()),
            ..HybridStages::default()
        }),
        fixture.source.clone(),
    ));
    let slot = Arc::new(Mutex::new(WatchContext::Prepared(old)));
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    tx.send(vec![ignored]).unwrap();
    *slot.lock().unwrap() = WatchContext::Prepared(new);
    tx.send(vec![allowed]).unwrap();
    drop(tx);
    spawn_consumer(
        fixture.cache.path().to_path_buf(),
        fixture.project.clone(),
        slot,
        fixture.db.settings().clone(),
        rx,
        Weak::new(),
    )
    .unwrap()
    .join()
    .unwrap();
    assert!(
        repo::doc_states(fixture.db.connection(), fixture.project.id)
            .unwrap()
            .contains(&before[0])
    );
    assert_eq!(fixture.search("betawidget9"), 0);
    assert_eq!(fixture.search("betawidget"), 1);
    assert_eq!(fixture.search("alphawidget9"), 1);
    assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 0);
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
}

#[test]
fn complete_prepared_revision_guards_incremental_and_full_recovery() {
    for recreate in [false, true] {
        let mut fixture = Fixture::new();
        let old = fixture.runtime(Config::default());
        // Same Config Arc; replacing stages is still a complete new revision.
        let new = Arc::new(WatchRuntime::new(
            old.config.clone(),
            Arc::new(HybridStages::default()),
            fixture.source.clone(),
        ));
        let before = repo::doc_states(fixture.db.connection(), fixture.project.id).unwrap();
        let generation =
            repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap();
        let path = fixture.project.canonical_root.join("bbb/b.md");
        fs::write(&path, "betawidget9").unwrap();
        let marker = lifecycle::index_dir(fixture.cache.path(), fixture.project.id)
            .join(crate::index::tantivy_index::REBUILD_MARKER);
        if recreate {
            fs::write(&marker, "").unwrap();
        }
        let mut reads = 0;
        let error = run_batch_using(
            &mut fixture.db,
            fixture.cache.path(),
            &fixture.project,
            std::slice::from_ref(&path),
            || {
                reads += 1;
                WatchContext::Prepared(if reads == 1 { old.clone() } else { new.clone() })
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("config changed while preparing batch")
        );
        assert_eq!(
            repo::doc_states(fixture.db.connection(), fixture.project.id).unwrap(),
            before
        );
        assert_eq!(
            repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap(),
            generation
        );
        assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 0);
        if recreate {
            assert!(marker.exists());
        }
        run_batch_using(
            &mut fixture.db,
            fixture.cache.path(),
            &fixture.project,
            &[path],
            || WatchContext::Prepared(new.clone()),
        )
        .unwrap();
        assert_eq!(fixture.search("betawidget9"), 1);
        assert!(!marker.exists());
        assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn provider_failure_preserves_marker_for_same_byte_retry() {
    struct Fails;
    impl Embedder for Fails {
        fn model_id(&self) -> &'static str {
            "watcher@d2"
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(Error::internal("controlled watcher provider failure"))
        }
    }
    let mut fixture = Fixture::new();
    let failed = Arc::new(WatchRuntime::new(
        Arc::new(Config::default()),
        Arc::new(HybridStages {
            embedder: Some(Arc::new(Fails)),
            ..HybridStages::default()
        }),
        fixture.source.clone(),
    ));
    let path = fixture.project.canonical_root.join("bbb/b.md");
    fs::write(&path, "betawidget9").unwrap();
    let generation = repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap();
    let error = run_batch_using(
        &mut fixture.db,
        fixture.cache.path(),
        &fixture.project,
        std::slice::from_ref(&path),
        || WatchContext::Prepared(failed.clone()),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("controlled watcher provider failure")
    );
    let rows = repo::doc_states(fixture.db.connection(), fixture.project.id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].content_hash, "");
    assert_eq!(
        repo::index_generation(fixture.db.connection(), fixture.project.id).unwrap(),
        generation
    );
    fixture.consume(vec![path], fixture.runtime(Config::default()));
    assert_eq!(fixture.search("betawidget9"), 1);
    assert_ne!(
        repo::doc_states(fixture.db.connection(), fixture.project.id).unwrap()[0].content_hash,
        ""
    );
    assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 1);
}

#[test]
fn admitted_provider_keeps_snapshot_without_slot_or_sql_guards() {
    struct SwapProvider {
        cache: PathBuf,
        path: PathBuf,
        slot: Weak<Mutex<WatchContext>>,
        next: Arc<WatchRuntime>,
        calls: AtomicUsize,
    }
    impl Embedder for SwapProvider {
        fn model_id(&self) -> &'static str {
            "watcher@d2"
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            for file in [crate::store::DB_FILE, "vectors.db"] {
                let connection = rusqlite::Connection::open(self.cache.join(file)).unwrap();
                connection.busy_timeout(Duration::ZERO).unwrap();
                connection
                    .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
                    .unwrap();
            }
            let slot = self.slot.upgrade().unwrap();
            *slot.try_lock().expect("no context guard through inference") =
                WatchContext::Prepared(self.next.clone());
            fs::write(&self.path, "betawidget9").unwrap();
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }
    let mut fixture = Fixture::new();
    let path = fixture.project.canonical_root.join("bbb/b.md");
    fs::write(&path, "betawidget8").unwrap();
    let next = fixture.runtime(Config {
        ignores: vec!["bbb/**".into()],
        ..Config::default()
    });
    let slot = Arc::new(Mutex::new(WatchContext::Configured(Arc::new(
        Config::default(),
    ))));
    let provider = Arc::new(SwapProvider {
        cache: fixture.cache.path().to_path_buf(),
        path: path.clone(),
        slot: Arc::downgrade(&slot),
        next,
        calls: AtomicUsize::new(0),
    });
    let runtime = Arc::new(WatchRuntime::new(
        Arc::new(Config::default()),
        Arc::new(HybridStages {
            embedder: Some(provider.clone()),
            ..HybridStages::default()
        }),
        fixture.source.clone(),
    ));
    *slot.lock().unwrap() = WatchContext::Prepared(runtime);
    let stats = run_batch_using(
        &mut fixture.db,
        fixture.cache.path(),
        &fixture.project,
        std::slice::from_ref(&path),
        || slot.lock().unwrap().clone(),
    )
    .unwrap();
    assert_eq!(stats.docs, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.search("betawidget8"), 1);
    assert_eq!(fixture.search("betawidget9"), 0);
    assert_eq!(
        run_batch_using(
            &mut fixture.db,
            fixture.cache.path(),
            &fixture.project,
            &[path],
            || slot.lock().unwrap().clone()
        )
        .unwrap(),
        JobStats::default()
    );
    assert_eq!(fixture.provider.0.load(Ordering::SeqCst), 0);
}

#[test]
fn captured_runtime_retains_vector_owner_until_consumer_finishes() {
    let mut fixture = Fixture::new();
    let runtime = fixture.runtime(Config::default());
    fixture.source = VectorCacheSource::Legacy(crate::vector_cache::CacheLimits::default());
    assert!(matches!(
        crate::vector_cache::VectorCache::open(fixture.cache.path()),
        Err(Error::Admission { .. })
    ));
    let path = fixture.project.canonical_root.join("bbb/b.md");
    fs::write(&path, "betawidget9").unwrap();
    fixture.consume(vec![path], runtime.clone());
    assert_eq!(fixture.search("betawidget9"), 1);
    drop(runtime);
    crate::vector_cache::VectorCache::open(fixture.cache.path()).unwrap();
}
