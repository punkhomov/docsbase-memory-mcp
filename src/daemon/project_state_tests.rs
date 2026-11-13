//! Regression checks for project config/stage publication (issue #76).

use super::*;
use crate::store::models::Project;
use std::sync::mpsc as sync_mpsc;

// Failure watchdog only: successful ordering is driven by channel messages.
const WATCHDOG: Duration = Duration::from_mins(1);

struct Fixture {
    shared: Arc<Shared>,
    project: Project,
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let cache = tempfile::TempDir::new().expect("cache");
        let root = tempfile::TempDir::new().expect("root");
        let shared = Shared::open(cache.path()).expect("empty daemon state");
        let mut db = Db::open(cache.path()).expect("registry");
        let project = registry::ensure_project(
            &mut db,
            &root.path().canonicalize().expect("canonical root"),
        )
        .expect("project");
        let fixture = Self {
            shared,
            project,
            root,
            cache,
        };
        fixture.write_config(
            "[hybrid]\nenabled = true\n[hybrid.reranker]\nprovider = \"openrouter\"\nmodel = \"old-model\"\nwindow = 10\n",
        );
        fixture
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.root.path().join(".docsbase.toml"), text).expect("project config");
    }
}

struct OldReranker;

impl crate::hybrid::Reranker for OldReranker {
    fn rerank(&self, _: &str, _: &[String], _: usize) -> Result<Vec<(usize, f32)>> {
        Err(Error::internal("obsolete reranker must never run"))
    }
}

fn old_stages() -> HybridStages {
    HybridStages {
        reranker: Some(Arc::new(OldReranker)),
        ..HybridStages::default()
    }
}

#[test]
fn project_reload_discards_an_obsolete_lazy_stage_build() {
    assert_obsolete_build_is_discarded(false);
}

#[test]
fn project_reload_discards_an_obsolete_lazy_stage_error() {
    assert_obsolete_build_is_discarded(true);
}

fn assert_obsolete_build_is_discarded(fail_build: bool) {
    let fixture = Fixture::new();
    let (entered_tx, entered_rx) = sync_mpsc::channel();
    let (release_tx, release_rx) = sync_mpsc::channel();
    std::thread::scope(|scope| {
        let fixture = &fixture;
        let old = scope.spawn(move || {
            fixture
                .shared
                .project_runtime_with(&fixture.project, |config| {
                    assert!(config.hybrid_cfg.enabled);
                    assert_eq!(
                        config
                            .hybrid_cfg
                            .reranker
                            .as_ref()
                            .expect("old reranker")
                            .window,
                        10
                    );
                    entered_tx.send(()).expect("old construction entered");
                    release_rx
                        .recv_timeout(WATCHDOG)
                        .expect("release old construction");
                    if fail_build {
                        Err(Error::Admission {
                            message: "obsolete stage failure".to_owned(),
                        })
                    } else {
                        Ok(old_stages())
                    }
                })
        });
        entered_rx
            .recv_timeout(WATCHDOG)
            .expect("old config captured");
        fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
        let reloaded = fixture.shared.open_project_config(&fixture.project);
        // Always release the worker before assertions, including reload errors.
        release_tx.send(()).expect("release old builder");
        let returned = old.join().expect("old worker").expect("current runtime");
        let reloaded = reloaded.expect("new reload must complete before old construction");
        let current = fixture
            .shared
            .project_runtime(&fixture.project)
            .expect("cached runtime");
        assert!(!reloaded.hybrid_cfg.enabled);
        assert_eq!(reloaded.max_docs_per_project, 23);
        assert!(
            Arc::ptr_eq(&reloaded, &returned.0),
            "obsolete caller must use the current config"
        );
        assert!(
            returned.1.reranker.is_none(),
            "obsolete stages must not escape to a caller"
        );
        assert!(
            current.1.reranker.is_none(),
            "obsolete stages must not replace the reload"
        );
        assert!(Arc::ptr_eq(&returned.1, &current.1));
        assert!(Arc::ptr_eq(&reloaded, &current.0));
    });
}

#[test]
fn project_runtime_reuses_stages_and_keeps_captured_snapshots_immutable() {
    let fixture = Fixture::new();
    let before = fixture
        .shared
        .project_runtime_with(&fixture.project, |_| Ok(old_stages()))
        .expect("old runtime");
    let cached = fixture
        .shared
        .project_runtime_with(&fixture.project, |_| {
            Err(Error::internal("cached stages must not be rebuilt"))
        })
        .expect("cache hit");
    assert!(Arc::ptr_eq(&before.0, &cached.0));
    assert!(Arc::ptr_eq(&before.1, &cached.1));

    fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("reload");
    let current = fixture
        .shared
        .project_runtime(&fixture.project)
        .expect("current runtime");
    assert!(!current.0.hybrid_cfg.enabled);
    assert!(current.1.reranker.is_none());
    assert!(before.0.hybrid_cfg.enabled);
    assert!(before.1.reranker.is_some());
    assert!(!Arc::ptr_eq(&before.0, &current.0));
    assert!(!Arc::ptr_eq(&before.1, &current.1));
}

#[test]
fn project_reload_failure_preserves_the_last_valid_runtime() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("initial open");
    let before = fixture
        .shared
        .project_runtime(&fixture.project)
        .expect("initial runtime");
    for text in [
        "[invalid toml",
        "hybrid = false\nmax_docs_per_project = 29\n",
    ] {
        fixture.write_config(text);
        let error = fixture
            .shared
            .open_project_config_with(&fixture.project, |config| {
                assert_eq!(config.max_docs_per_project, 29);
                Err(Error::Admission {
                    message: "injected stage admission failure".to_owned(),
                })
            })
            .expect_err("reload must fail");
        if text.starts_with("[invalid") {
            assert!(error.to_string().contains("parse"));
        } else {
            assert!(matches!(error, Error::Admission { .. }));
        }
        let after = fixture
            .shared
            .project_runtime(&fixture.project)
            .expect("old runtime remains usable");
        assert!(Arc::ptr_eq(&before.0, &after.0));
        assert!(Arc::ptr_eq(&before.1, &after.1));
        assert_eq!(after.0.max_docs_per_project, 11);
    }
    fixture.write_config("hybrid = false\nmax_docs_per_project = 31\n");
    let recovered = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("a later valid reload must recover");
    assert_eq!(recovered.max_docs_per_project, 31);
}

#[test]
fn project_reload_serializes_one_project_without_locking_the_global_map() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    let other_root = tempfile::TempDir::new().expect("other root");
    std::fs::write(other_root.path().join(".docsbase.toml"), "hybrid = false\n")
        .expect("other config");
    let mut db = Db::open(fixture.cache.path()).expect("registry");
    let other = registry::ensure_project(
        &mut db,
        &other_root
            .path()
            .canonicalize()
            .expect("other canonical root"),
    )
    .expect("other project");
    drop(db);
    let (entered_tx, entered_rx) = sync_mpsc::channel();
    let (release_tx, release_rx) = sync_mpsc::channel();
    std::thread::scope(|scope| {
        let fixture = &fixture;
        let first = scope.spawn(move || {
            fixture
                .shared
                .open_project_config_with(&fixture.project, |config| {
                    assert_eq!(config.max_docs_per_project, 11);
                    entered_tx.send(()).expect("first reload entered");
                    release_rx
                        .recv_timeout(WATCHDOG)
                        .expect("release first reload");
                    crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
                })
        });
        entered_rx
            .recv_timeout(WATCHDOG)
            .expect("first constructing");
        let slot = fixture.shared.project_slot(fixture.project.id);
        assert!(matches!(
            slot.reload.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        assert!(
            slot.state.try_lock().is_ok(),
            "state is free during model construction"
        );
        assert!(
            slot.preparation.try_write().is_ok(),
            "preparation is free during model construction"
        );
        assert!(
            fixture.shared.project_states.try_lock().is_ok(),
            "global map is free during construction"
        );
        fixture
            .shared
            .open_project_config(&other)
            .expect("another project can reload before release");

        fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
        let (attempt_tx, attempt_rx) = sync_mpsc::channel();
        let second = scope.spawn(move || {
            attempt_tx.send(()).expect("second reload attempted");
            fixture.shared.open_project_config(&fixture.project)
        });
        attempt_rx
            .recv_timeout(WATCHDOG)
            .expect("second caller started");
        release_tx.send(()).expect("release first reload");
        assert_eq!(
            first
                .join()
                .expect("first thread")
                .expect("first reload")
                .max_docs_per_project,
            11
        );
        let newest = second
            .join()
            .expect("second thread")
            .expect("second reload");
        assert_eq!(newest.max_docs_per_project, 23);
        let current = fixture
            .shared
            .project_runtime(&fixture.project)
            .expect("current runtime");
        assert!(Arc::ptr_eq(&newest, &current.0));
        assert!(current.1.reranker.is_none());
    });
}

#[test]
fn preparation_rejects_obsolete_revision_before_running_work() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    let before = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("initial revision");
    let slot = fixture.shared.project_slot(fixture.project.id);
    fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("reload");
    assert!(
        Arc::ptr_eq(&slot, &fixture.shared.project_slot(fixture.project.id)),
        "stable cell"
    );
    let ran = AtomicBool::new(false);
    let error = slot
        .with_preparation(&before, || {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect_err("obsolete revision");
    assert!(matches!(error, Error::Project { .. }), "{error}");
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn preparation_retains_only_its_read_gate_and_releases_after_error() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\n");
    let config = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("revision");
    let slot = fixture.shared.project_slot(fixture.project.id);
    let value = slot
        .with_preparation(&config, || {
            assert!(slot.state.try_lock().is_ok(), "no state mutex over work");
            assert!(
                slot.reload.try_lock().is_ok(),
                "no reload serializer over work"
            );
            assert!(matches!(
                slot.preparation.try_write(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            Ok("owned result".to_owned())
        })
        .expect("prepared value");
    assert_eq!(value, "owned result");
    assert!(
        slot.preparation.try_write().is_ok(),
        "success releases read gate"
    );
    let error = slot
        .with_preparation(&config, || {
            Err::<(), _>(Error::internal("prepared failure"))
        })
        .expect_err("closure failure");
    assert!(error.to_string().contains("prepared failure"));
    assert!(
        slot.preparation.try_write().is_ok(),
        "error releases read gate"
    );
}

#[test]
fn preparation_releases_its_gate_after_unwind() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\n");
    let config = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("revision");
    let slot = fixture.shared.project_slot(fixture.project.id);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slot.with_preparation(&config, || -> Result<()> { panic!("prepared unwind") })
    }));
    assert!(unwind.is_err());
    assert!(
        slot.preparation.try_write().is_ok(),
        "unwind releases read gate"
    );
    assert!(slot.state.try_lock().is_ok(), "work did not poison state");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("later reload");
}

#[test]
fn reload_panic_preserves_last_pair_and_allows_later_recovery() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("initial open");
    let before = fixture
        .shared
        .project_runtime(&fixture.project)
        .expect("initial pair");
    fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture
            .shared
            .open_project_config_with(&fixture.project, |_| panic!("model admission unwind"))
    }));
    assert!(panic.is_err());
    let retained = fixture
        .shared
        .project_runtime(&fixture.project)
        .expect("retained pair");
    assert!(Arc::ptr_eq(&before.0, &retained.0));
    assert!(Arc::ptr_eq(&before.1, &retained.1));
    assert_eq!(retained.0.max_docs_per_project, 11);
    let recovered = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("recovered reload");
    assert_eq!(recovered.max_docs_per_project, 23);
}

#[test]
fn semantic_reload_invalidates_resident_rows_but_unchanged_reload_keeps_them() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    let before = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("initial open");
    let load = || {
        let db = Db::open(fixture.cache.path()).expect("metadata");
        match fixture
            .shared
            .dense
            .get(&db, fixture.project.id, "test/reload@d3@l2")
            .expect("rows")
        {
            crate::hybrid::dense::DenseRows::Ready(rows) => rows,
            _ => panic!("empty project has exact rows"),
        }
    };
    let rows = load();
    let same = fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("same config reopen");
    assert!(
        !Arc::ptr_eq(&before, &same),
        "reopen still publishes a fresh revision"
    );
    assert!(
        Arc::ptr_eq(&rows, &load()),
        "semantic equality preserves resident rows"
    );
    fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("changed config");
    assert!(
        !Arc::ptr_eq(&rows, &load()),
        "changed revision clears resident identity"
    );
}

#[test]
fn active_preparation_blocks_publication_without_blocking_state_reads() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .expect("initial open");
    let before = fixture
        .shared
        .project_runtime(&fixture.project)
        .expect("initial pair");
    fixture.write_config("hybrid = false\nmax_docs_per_project = 23\n");
    let slot = fixture.shared.project_slot(fixture.project.id);
    let (entered_tx, entered_rx) = sync_mpsc::channel();
    let (release_tx, release_rx) = sync_mpsc::channel();
    let (contended_tx, contended_rx) = sync_mpsc::channel();
    std::thread::scope(|scope| {
        let slot_ref = &slot;
        let before_ref = &before.0;
        let preparation = scope.spawn(move || {
            slot_ref.with_preparation(before_ref, || {
                entered_tx.send(()).expect("preparation entered");
                release_rx
                    .recv_timeout(WATCHDOG)
                    .expect("release active preparation");
                Ok(())
            })
        });
        entered_rx
            .recv_timeout(WATCHDOG)
            .expect("active read phase");
        let reload = scope.spawn(|| {
            fixture.shared.open_project_config_observing_contention(
                &fixture.project,
                |_| Ok(HybridStages::default()),
                || contended_tx.send(()).expect("publication WRITE contended"),
            )
        });
        let contended = contended_rx.recv_timeout(WATCHDOG);
        let observed = slot
            .state
            .try_lock()
            .map(|state| state.config.clone().zip(state.stages.clone()))
            .map_err(|error| error.to_string());
        // Release all workers before inspecting a failure, including wrong lock order.
        release_tx.send(()).expect("release preparation");
        preparation
            .join()
            .expect("preparation thread")
            .expect("preparation result");
        let after = reload
            .join()
            .expect("reload thread")
            .expect("reload result");
        contended.expect("reload attempted publication WRITE during preparation");
        let observed = observed
            .expect("state read before preparation release")
            .expect("old pair");
        assert!(
            Arc::ptr_eq(&before.0, &observed.0) && Arc::ptr_eq(&before.1, &observed.1),
            "publication waits for preparation"
        );
        assert_eq!(after.max_docs_per_project, 23);
    });
}

#[test]
fn rejected_project_vector_budget_preserves_published_runtime_and_dense_rows() {
    use crate::hybrid::dense::{AnnPolicy, DenseCache, DenseRows};
    use crate::vector_cache::VectorStore;

    let mut fixture = Fixture::new();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 11\n");
    fixture
        .shared
        .open_project_config(&fixture.project)
        .unwrap();
    let before = fixture.shared.project_runtime(&fixture.project).unwrap();
    let owner = VectorStore::acquire(
        fixture.cache.path(),
        crate::vector_cache::settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    Arc::get_mut(&mut fixture.shared).unwrap().dense = DenseCache::with_vector_store(
        AnnPolicy::Disabled,
        &owner,
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    let load = || {
        let db = Db::open(fixture.cache.path()).unwrap();
        match fixture
            .shared
            .dense
            .get(&db, fixture.project.id, "test/reload@d3@l2")
            .unwrap()
        {
            DenseRows::Ready(rows) => rows,
            _ => panic!("empty project has exact rows"),
        }
    };
    let rows = load();
    fixture.write_config("hybrid = false\nmax_docs_per_project = 29\n");
    let error = fixture
        .shared
        .open_project_config(&fixture.project)
        .unwrap_err();
    assert!(matches!(error, Error::Admission { .. }));
    let after = fixture.shared.project_runtime(&fixture.project).unwrap();
    assert!(Arc::ptr_eq(&before.0, &after.0));
    assert!(Arc::ptr_eq(&before.1, &after.1));
    assert!(
        Arc::ptr_eq(&rows, &load()),
        "failed reload must not invalidate dense state"
    );
}

#[test]
fn captured_legacy_sync_source_keeps_its_budget_after_dense_override() {
    let fixture = Fixture::new();
    let mut captured = Config::default();
    captured.hybrid_cfg.cache.max_bytes = 8;
    fixture
        .shared
        .dense
        .set_vector_cache_max_bytes(fixture.project.id, 64)
        .unwrap();
    let source = fixture
        .shared
        .vector_source_for(fixture.project.id, &captured);
    let mut vectors = source.connect(fixture.cache.path()).unwrap();
    vectors.put("snapshot@d2", "first", &[1.0, 0.0]).unwrap();
    vectors.put("snapshot@d2", "second", &[0.0, 1.0]).unwrap();
    assert!(vectors.get("snapshot@d2", "first").unwrap().is_none());
    assert!(vectors.get("snapshot@d2", "second").unwrap().is_some());
}

#[test]
fn daemon_status_and_backfill_coverage_use_owned_source() {
    struct Embed;
    impl crate::vector_cache::Embedder for Embed {
        fn model_id(&self) -> &'static str {
            "openrouter/model@d2@l2"
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }
    let mut fixture = Fixture::new();
    fixture.write_config("[hybrid]\nenabled=true\n[hybrid.embedding]\nprovider=\"openrouter\"\nmodel=\"model\"\ndimensions=2\nnormalize=true\n");
    std::fs::write(fixture.root.path().join("a.md"), "alphawidget").unwrap();
    fixture
        .shared
        .open_project_config_with(&fixture.project, |_| {
            Ok(HybridStages {
                embedder: Some(Arc::new(Embed)),
                ..HybridStages::default()
            })
        })
        .unwrap();
    let (config, stages) = fixture.shared.project_runtime(&fixture.project).unwrap();
    let owner = crate::vector_cache::VectorStore::acquire(
        fixture.cache.path(),
        crate::vector_cache::settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    Arc::get_mut(&mut fixture.shared).unwrap().dense = DenseCache::with_vector_store(
        AnnPolicy::Disabled,
        &owner,
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    let source = crate::vector_cache::VectorCacheSource::Owned(owner);
    let mut db = Db::open(fixture.cache.path()).unwrap();
    tools::run_project_index_with_vectors(
        &mut db,
        fixture.cache.path(),
        &fixture.project,
        &config,
        &stages,
        &source,
    )
    .unwrap();
    let project = registry::project_by_id(&db, fixture.project.id)
        .unwrap()
        .unwrap();
    let status = route_metadata_tool(&fixture.shared, None, "status", &json!({})).unwrap();
    assert_eq!(
        status["projects"][0]["embed_coverage"],
        json!({"cached":1,"total":1})
    );
    let panel = route_metadata_tool(&fixture.shared, None, "panel_projects", &json!({})).unwrap();
    assert_eq!(panel[0]["cached_vectors"], 1);
    assert_eq!(panel[0]["vector_chunks"], 1);
    fixture.shared.schedule_backfill(&project).unwrap();
    assert!(
        crate::store::repo::active_sync_job(db.connection(), project.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn sync_stage_admission_failure_precedes_enqueue() {
    // The runtime's documented env override outranks a disabled provider.
    // Isolate this consumer from other unit tests that mutate that env.
    const CHILD: &str = "DOCSBASE_CF7C2_PREFLIGHT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Other unit tests temporarily redirect the process-global config path.
        // The child needs a fixed empty launch config as well as isolated keys.
        let config = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "daemon::server::project_state_tests::sync_stage_admission_failure_precedes_enqueue"])
            .env(CHILD, "1")
            .env("DOCSBASE_CONFIG_DIR", config.path())
            .env_remove("ORT_DYLIB_PATH")
            .env_remove("OPENROUTER_API_KEY")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut fixture = Fixture::new();
    Arc::get_mut(&mut fixture.shared)
        .unwrap()
        .global_config
        .providers = crate::hybrid::ProvidersCfg::default().with(
        "fastembed",
        crate::hybrid::FastembedProvider {
            enabled: false,
            runtime: None,
            runtime_path: None,
        }
        .to_record(),
    );
    fixture.write_config("[hybrid]\nenabled=true\n[hybrid.embedding]\nprovider=\"fastembed\"\nmodel=\"bge-small-en-v1.5\"\ndimensions=384\nnormalize=true\n");
    // Full builds admit the file shape, then reject the disabled runtime.
    // Lean builds refuse the unavailable feature during config admission.
    if cfg!(feature = "hybrid-local") {
        fixture.shared.project_config(&fixture.project).unwrap();
    }
    let result = route_metadata_tool(
        &fixture.shared,
        None,
        "sync_start",
        &json!({"project_id":fixture.project.id}),
    );
    assert!(matches!(result, Err(Error::Admission { .. })));
    let db = Db::open(fixture.cache.path()).unwrap();
    assert!(
        crate::store::repo::active_sync_job(db.connection(), fixture.project.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn daemon_watcher_uses_owner_and_rejected_reload_preserves_snapshot() {
    let mut fixture = Fixture::new();
    fixture.write_config("hybrid=false\n");
    std::fs::write(fixture.root.path().join("a.md"), "alphawidget").unwrap();
    let (config, stages) = fixture.shared.project_runtime(&fixture.project).unwrap();
    let mut db = Db::open(fixture.cache.path()).unwrap();
    tools::run_project_index(&mut db, fixture.cache.path(), &fixture.project, &config).unwrap();
    let project = registry::project_by_id(&db, fixture.project.id)
        .unwrap()
        .unwrap();
    let owner = crate::vector_cache::VectorStore::acquire(
        fixture.cache.path(),
        crate::vector_cache::settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    Arc::get_mut(&mut fixture.shared).unwrap().dense = DenseCache::with_vector_store(
        AnnPolicy::Disabled,
        &owner,
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    assert!(fixture.shared.ensure_watcher(&project).unwrap());
    let first = fixture
        .shared
        .watchers
        .runtime_for_test(project.id)
        .expect("prepared daemon watcher");
    assert!(Arc::ptr_eq(&first.0, &config));
    assert!(Arc::ptr_eq(&first.1, &stages));
    let crate::vector_cache::VectorCacheSource::Owned(source) = first.2 else {
        panic!("owner source")
    };
    let _connection = source.connect().unwrap();
    // Strict reload refuses legacy project budgets, leaving watcher state intact.
    fixture.write_config("hybrid=false\nmax_docs_per_project=23\n");
    assert!(matches!(
        fixture.shared.open_project_config(&project),
        Err(Error::Admission { .. })
    ));
    let unchanged = fixture
        .shared
        .watchers
        .runtime_for_test(project.id)
        .unwrap();
    assert!(Arc::ptr_eq(&unchanged.0, &config));
    assert!(Arc::ptr_eq(&unchanged.1, &stages));
}

#[test]
fn stale_watcher_ensure_uses_current_runtime_after_reload() {
    let fixture = Fixture::new();
    fixture.write_config("hybrid=false\n");
    let mut db = Db::open(fixture.cache.path()).unwrap();
    tools::run_project_index(
        &mut db,
        fixture.cache.path(),
        &fixture.project,
        &Config::default(),
    )
    .unwrap();
    let indexed = registry::project_by_id(&db, fixture.project.id)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .shared
            .ensure_watcher_observing(&indexed, || {
                fixture.write_config("hybrid=false\nmax_docs_per_project=23\n");
                // A legitimate stale registry value does not itself start a watcher.
                fixture
                    .shared
                    .open_project_config(&fixture.project)
                    .unwrap();
            })
            .unwrap()
    );
    let current = fixture.shared.project_runtime(&indexed).unwrap();
    let watched = fixture
        .shared
        .watchers
        .runtime_for_test(indexed.id)
        .unwrap();
    assert_eq!(watched.0.max_docs_per_project, 23);
    assert!(Arc::ptr_eq(&current.0, &watched.0));
    assert!(Arc::ptr_eq(&current.1, &watched.1));
    fixture.write_config("hybrid=false\nmax_docs_per_project=24\n");
    fixture.shared.open_project_config(&indexed).unwrap();
    let current = fixture.shared.project_runtime(&indexed).unwrap();
    let revised = fixture
        .shared
        .watchers
        .runtime_for_test(indexed.id)
        .unwrap();
    assert_eq!(revised.0.max_docs_per_project, 24);
    assert!(Arc::ptr_eq(&current.0, &revised.0));
    assert!(Arc::ptr_eq(&current.1, &revised.1));
    assert!(!Arc::ptr_eq(&watched.0, &revised.0));
}

#[test]
fn identical_config_reload_revises_prepared_watcher_stages() {
    let fixture = Fixture::new();
    let mut db = Db::open(fixture.cache.path()).unwrap();
    tools::run_project_index(
        &mut db,
        fixture.cache.path(),
        &fixture.project,
        &Config::default(),
    )
    .unwrap();
    let indexed = registry::project_by_id(&db, fixture.project.id)
        .unwrap()
        .unwrap();
    fixture
        .shared
        .open_project_config_with(&indexed, |_| Ok(old_stages()))
        .unwrap();
    let first = fixture
        .shared
        .watchers
        .runtime_for_test(indexed.id)
        .unwrap();
    // Reconstructing a provider can refresh credentials with identical TOML.
    fixture
        .shared
        .open_project_config_with(&indexed, |_| Ok(old_stages()))
        .unwrap();
    let current = fixture.shared.project_runtime(&indexed).unwrap();
    assert_eq!(*first.0, *current.0);
    assert!(!Arc::ptr_eq(&first.1, &current.1));
    let watched = fixture
        .shared
        .watchers
        .runtime_for_test(indexed.id)
        .unwrap();
    assert!(Arc::ptr_eq(&watched.0, &current.0));
    assert!(Arc::ptr_eq(&watched.1, &current.1));
}
