//! Causal profile gates: factories and unrelated callbacks must never run.
use super::*;
use crate::store::models::{Project, ProjectStatus};

struct Fixture {
    cache: tempfile::TempDir,
    global: tempfile::TempDir,
    root: tempfile::TempDir,
    other: tempfile::TempDir,
    project: Project,
    foreign: Project,
}
impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let foreign = registry::ensure_project(&mut db, other.path()).unwrap();
        for p in [&project, &foreign] {
            std::fs::write(p.canonical_root.join("guide.md"), "profilewidget").unwrap();
            let mut index = crate::index::tantivy_index::IndexHandle::open_or_create(
                &lifecycle::index_dir(cache.path(), p.id),
            )
            .unwrap();
            crate::index::job::run_full(&mut db, &mut index, p, &Config::default()).unwrap();
            registry::mark_indexed(&db, p.id).unwrap();
        }
        let project = registry::project_by_id(&db, project.id).unwrap().unwrap();
        let foreign = registry::project_by_id(&db, foreign.id).unwrap().unwrap();
        drop(db);
        let f = Self {
            cache,
            global,
            root,
            other,
            project,
            foreign,
        };
        f.source("");
        f
    }
    fn source(&self, prefix: &str) {
        std::fs::write(
            self.global.path().join("config.toml"),
            format!(
                "{prefix}\n[daemon]\nprofile={{mode='website',project_root={}}}\n",
                toml::Value::String(self.root.path().to_string_lossy().into_owned())
            ),
        )
        .unwrap();
    }
    fn open(&self) -> Result<Arc<Shared>> {
        Shared::open_with_launch(
            self.cache.path(),
            &crate::store::settings::standalone().unwrap(),
            &crate::store::settings::standalone_read_pool().unwrap(),
            crate::hybrid::dense_settings::standalone()
                .unwrap()
                .settings(),
            &LaunchSnapshot::capture_from(self.global.path()),
            DaemonPatch::default(),
        )
    }
    fn no_activation_effects(&self) {
        for id in [self.project.id, self.foreign.id] {
            assert!(
                !self
                    .cache
                    .path()
                    .join("projects")
                    .join(id.to_string())
                    .join("ann")
                    .exists()
            );
        }
        for name in ["state", "vectors.db", "logs"] {
            assert!(!self.cache.path().join(name).exists(), "unexpected {name}");
        }
    }
}

#[test]
fn fixed_runtime_skips_factories_and_every_background_hook() {
    let f = Fixture::new();
    let shared = f.open().unwrap();
    let (_, stages) = shared
        .project_runtime_with(&f.project, |_| panic!("provider factory"))
        .unwrap();
    assert!(stages.embedder.is_none() && stages.reranker.is_none());
    shared
        .open_project_config_with(&f.project, |_| panic!("reload provider factory"))
        .unwrap();
    assert!(matches!(
        shared.project_runtime_with(&f.foreign, |_| panic!("foreign provider")),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        shared.project_config(&f.foreign),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        shared.config_for_root(f.other.path()),
        Err(Error::Admission { .. })
    ));
    assert!(
        !shared
            .ensure_watcher_observing(&f.project, || panic!("watch admission"))
            .unwrap()
    );
    assert!(matches!(
        shared.ensure_watcher(&f.foreign),
        Err(Error::Admission { .. })
    ));
    assert!(
        shared
            .refresh_ann_after_sync_before_with(
                &f.project,
                || panic!("ANN scheduling"),
                |_, _, _| panic!("ANN metadata")
            )
            .is_none()
    );
    shared.schedule_backfill(&f.project).unwrap();
    assert_eq!(shared.watchers.count(), 0);
    assert_eq!(shared.project_states.lock().unwrap().len(), 1);
    f.no_activation_effects();
}

#[test]
fn status_and_panel_filter_before_unrelated_runtime_or_coverage() {
    let f = Fixture::new();
    std::fs::write(f.other.path().join(".docsbase.toml"), "hybrid=true").unwrap();
    let shared = f.open().unwrap();
    for tool in ["status", "panel_projects", "list_projects"] {
        let response = route_metadata_tool(&shared, None, tool, &json!({})).unwrap();
        let projects = if tool == "status" {
            response["projects"].as_array().unwrap()
        } else {
            response.as_array().unwrap()
        };
        assert_eq!(projects.len(), 1, "{tool}: {response}");
        assert_eq!(projects[0]["id"], f.project.id);
    }
    assert_eq!(shared.project_states.lock().unwrap().len(), 1);
    assert_eq!(shared.watchers.count(), 0);
    f.no_activation_effects();
}

#[tokio::test]
async fn website_rejects_mutating_registration_and_tool_bypasses_before_jobs() {
    let f = Fixture::new();
    let shared = f.open().unwrap();
    for unbound in [false, true] {
        assert!(matches!(
            register_session(&shared, std::process::id(), f.root.path(), unbound).await,
            Err(Error::Admission { .. })
        ));
    }
    assert_eq!(shared.sessions.stats().sessions, 0);
    for tool in ["panel_config", "list_docs", "get_doc", "read_neighbors"] {
        assert!(
            matches!(
                route_metadata_tool(&shared, None, tool, &json!({})),
                Err(Error::Admission { .. })
            ),
            "unbound {tool}"
        );
    }
    for (tool, args) in [
        ("index_project", json!({"path": f.other.path()})),
        ("sync_start", json!({"project_id": f.foreign.id})),
        ("sync_start", json!({"project_id": f.project.id})),
    ] {
        assert!(
            matches!(
                route_metadata_tool(&shared, None, tool, &args),
                Err(Error::Admission { .. })
            ),
            "{tool}"
        );
    }
    assert!(
        crate::store::repo::active_sync_job(shared.db().connection(), f.project.id)
            .unwrap()
            .is_none()
    );
    assert!(
        crate::store::repo::active_sync_job(shared.db().connection(), f.foreign.id)
            .unwrap()
            .is_none()
    );
    f.no_activation_effects();
}

#[test]
fn admission_rejects_global_and_project_hybrid_without_activation() {
    let f = Fixture::new();
    f.source("hybrid=true");
    let error = f.open().err().unwrap();
    assert!(
        matches!(error, Error::Admission { .. }) && error.to_string().contains("lexical"),
        "{error}"
    );
    f.source("");
    std::fs::write(f.root.path().join(".docsbase.toml"), "hybrid=true").unwrap();
    let error = f.open().err().unwrap();
    assert!(
        matches!(error, Error::Admission { .. }) && error.to_string().contains("lexical"),
        "{error}"
    );
    f.no_activation_effects();
}

#[test]
fn admission_requires_existing_indexed_exact_root_without_registration() {
    let f = Fixture::new();
    let db = Db::open(f.cache.path()).unwrap();
    registry::set_status(&db, f.project.id, ProjectStatus::NotIndexed).unwrap();
    drop(db);
    let error = f.open().err().unwrap();
    assert!(error.to_string().contains("indexed"), "{error}");
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(
        f.global.path().join("config.toml"),
        format!(
            "[daemon]\nprofile={{mode='website',project_root={}}}",
            toml::Value::String(fresh.path().to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let error = f.open().err().unwrap();
    assert!(error.to_string().contains("registered"), "{error}");
    assert_eq!(
        registry::list_projects(&Db::open_readonly(f.cache.path()).unwrap())
            .unwrap()
            .len(),
        2
    );
    f.no_activation_effects();
}

#[test]
fn valid_binding_cannot_bypass_sync_project_or_job_identity() {
    let f = Fixture::new();
    let db = Db::open(f.cache.path()).unwrap();
    let (foreign_job, _) = tools::enqueue_sync(&db, &f.foreign).unwrap();
    drop(db);
    let shared = f.open().unwrap();
    let (session, _) = shared
        .sessions
        .join(
            std::process::id(),
            f.root.path().to_owned(),
            Some(f.project.id),
        )
        .unwrap();
    for (tool, args) in [
        ("sync_start", json!({"project_id": f.foreign.id})),
        ("sync_status", json!({"job_id": foreign_job})),
        ("index_project", json!({"path": f.other.path()})),
    ] {
        assert!(
            matches!(
                route_metadata_tool(&shared, Some(session), tool, &args),
                Err(Error::Admission { .. })
            ),
            "{tool}"
        );
    }
    assert!(
        crate::store::repo::active_sync_job(shared.db().connection(), f.project.id)
            .unwrap()
            .is_none()
    );
    f.no_activation_effects();
}

#[test]
fn admitted_project_policy_is_forwarded_once_and_reload_remains_pure() {
    let f = Fixture::new();
    let db = crate::store::settings::standalone().unwrap();
    let launch = LaunchSnapshot::capture_from(f.global.path())
        .strict(DaemonPatch::default())
        .unwrap();
    let admitted = launch.admit(f.cache.path(), &db).unwrap();
    std::fs::write(f.root.path().join(".docsbase.toml"), "hybrid=true").unwrap();
    let shared = Shared::open_with_admitted_launch(
        f.cache.path(),
        &db,
        &crate::store::settings::standalone_read_pool().unwrap(),
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        admitted,
    )
    .unwrap();
    assert!(
        !shared
            .project_config(&f.project)
            .unwrap()
            .hybrid_cfg
            .enabled
    );
    assert!(matches!(
        shared.open_project_config_with(&f.project, |_| panic!("rejected reload provider")),
        Err(Error::Admission { .. })
    ));
    assert!(
        !shared
            .project_config(&f.project)
            .unwrap()
            .hybrid_cfg
            .enabled
    );
    f.no_activation_effects();
}

#[test]
fn malformed_project_policy_is_rejected_before_startup_resources() {
    let f = Fixture::new();
    std::fs::write(f.root.path().join(".docsbase.toml"), "[hybrid\n").unwrap();
    assert!(f.open().is_err());
    f.no_activation_effects();
}
