use super::*;
use crate::daemon::readiness;
use crate::index::outcome;

struct Env {
    db: Db,
    project: Project,
    cache: tempfile::TempDir,
    _root: tempfile::TempDir,
}
impl Env {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "causalwidget").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        Self {
            db,
            project,
            cache,
            _root: root,
        }
    }
    fn observe(&self) -> crate::ipc::panel::ServerStatus {
        let pool = crate::store::read_pool::ReadPool::prepare(
            self.cache.path(),
            &crate::store::settings::standalone().unwrap(),
            &crate::store::settings::standalone_read_pool().unwrap(),
        )
        .unwrap();
        readiness::observe(&pool, &self.project, &Config::default(), None).unwrap()
    }
}

#[test]
fn later_direct_noop_cannot_be_attributed_to_an_older_sync() {
    interleaved_direct(false);
}

#[test]
fn later_same_generation_direct_partial_revokes_sync_readiness() {
    interleaved_direct(true);
}

fn interleaved_direct(partial: bool) {
    let mut env = Env::new();
    let (id, _) = enqueue_sync(&env.db, &env.project).unwrap();
    let mut captured = None;
    run_sync_job_using(&mut env.db, id, |db| {
        let stats =
            run_project_index_using(db, env.cache.path(), &env.project, Some(id), |db, index| {
                run_full(db, index, &env.project, &Config::default())
            })?;
        captured = outcome::read(db, env.project.id)?;
        // The lexical writer lease has ended, but this Sync is not terminal.
        // A direct full/no-op/partial attempt now runs before Sync persistence.
        let mut config = Config::default();
        if partial {
            config.max_file_size = 1;
        }
        run_project_index(db, env.cache.path(), &env.project, &config)?;
        Ok(stats)
    })
    .unwrap();
    let captured = captured.unwrap();
    let current = outcome::read(&env.db, env.project.id).unwrap().unwrap();
    assert!(current.attempt_id > captured.attempt_id);
    assert_eq!(current.index_generation, captured.index_generation);
    let view = env.observe();
    assert_eq!(
        view.latest_sync.unwrap().attempt_id,
        Some(captured.attempt_id)
    );
    assert_eq!(view.outcome.unwrap().attempt_id, current.attempt_id);
    if partial {
        assert!(!view.ready);
        assert_eq!(
            view.reason,
            Some(crate::ipc::panel::ReadinessReason::Partial)
        );
    } else {
        assert!(view.ready);
    }
}

#[test]
fn backfill_failure_preserves_exact_lexical_attribution_but_stays_not_ready() {
    let mut env = Env::new();
    let (id, _) = enqueue_sync(&env.db, &env.project).unwrap();
    let error = run_sync_job_using(&mut env.db, id, |db| {
        run_project_index_using(db, env.cache.path(), &env.project, Some(id), |db, index| {
            run_full(db, index, &env.project, &Config::default())
        })?;
        Err(Error::internal(
            "deterministic post-lexical backfill failure",
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains("backfill failure"));
    let view = env.observe();
    let lexical = view.outcome.unwrap();
    let sync = view.latest_sync.unwrap();
    assert_eq!(sync.state, "error");
    assert_eq!(sync.attempt_id, Some(lexical.attempt_id));
    assert_eq!(sync.stats, Some(lexical.stats));
    assert_eq!(
        view.reason,
        Some(crate::ipc::panel::ReadinessReason::SyncFailed)
    );
}
