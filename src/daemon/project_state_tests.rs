//! Regression checks for project config/stage publication (issue #76).

use super::*;
use crate::store::models::Project;
use std::sync::mpsc as sync_mpsc;

// Failure watchdog only: successful ordering is driven by channel messages.
const WATCHDOG: Duration = Duration::from_secs(60);

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
            slot.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
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
