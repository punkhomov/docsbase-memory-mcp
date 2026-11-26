use super::*;
use crate::daemon::registry;
use std::cell::Cell;

struct Env {
    db: Db,
    project: Project,
    _root: tempfile::TempDir,
    cache: tempfile::TempDir,
}
impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "readinesswidget").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        super::super::tools::run_project_index(&mut db, cache.path(), &project, &Config::default())
            .unwrap();
        Self {
            db,
            project,
            _root: root,
            cache,
        }
    }
}

#[test]
fn stable_observation_reads_two_fresh_snapshots_and_checks_health_once() {
    let env = Env::new();
    let reads = Cell::new(0);
    let checks = Cell::new(0);
    let status = observe_with(
        &env.project,
        &outcome::recipe_id(&Config::default()),
        None,
        || {
            reads.set(reads.get() + 1);
            Snapshot::read(&env.db, env.project.id)
        },
        |snapshot| {
            checks.set(checks.get() + 1);
            assert!(
                env.db.connection().is_autocommit(),
                "O1 transaction must end before index IO"
            );
            let dir = snapshot.index.tantivy_dir(env.db.cache_root());
            lifecycle::with_existing_project_read_lease(
                env.db.cache_root(),
                env.project.id,
                || ReadIndex::check_health_with_project_lease(&dir),
            )
            .unwrap();
            IndexHealth::Healthy
        },
    )
    .unwrap();
    assert!(status.ready);
    assert_eq!(reads.get(), 2);
    assert_eq!(checks.get(), 1);
}

#[test]
fn generation_movement_is_state_changed_without_retry() {
    let env = Env::new();
    let reads = Cell::new(0);
    let checks = Cell::new(0);
    let generation =
        crate::store::repo::index_generation(env.db.connection(), env.project.id).unwrap();
    let status = observe_with(
        &env.project,
        &outcome::recipe_id(&Config::default()),
        None,
        || {
            reads.set(reads.get() + 1);
            Snapshot::read(&env.db, env.project.id)
        },
        |_| {
            assert!(env.db.connection().is_autocommit());
            checks.set(checks.get() + 1);
            let writer = Db::open(env.cache.path()).unwrap();
            writer
                .connection()
                .execute_batch("UPDATE projects SET index_generation=index_generation+1")
                .unwrap();
            IndexHealth::Healthy
        },
    )
    .unwrap();
    assert_eq!(status.reason, Some(ReadinessReason::StateChanged));
    assert_eq!(status.index_generation, generation + 1);
    assert_eq!((reads.get(), checks.get()), (2, 1));
}

#[test]
fn attempt_movement_without_generation_change_is_state_changed_without_retry() {
    movement("UPDATE projects SET index_attempt=index_attempt+1,index_outcome_json=NULL");
}

#[test]
fn queued_job_movement_without_generation_change_is_state_changed_without_retry() {
    movement("INSERT INTO sync_jobs(project_id,state,started_at) VALUES(1,'queued',1)");
}

#[test]
#[cfg(unix)]
fn catalog_readiness_observes_a_sync_enqueued_during_the_health_check() {
    let mut env = Env::new();
    let epoch = [7_u8; 16];
    let project = env.project.id;
    let dir = lifecycle::index_dir(env.cache.path(), project);
    std::fs::write(dir.parent().unwrap().join(".legacy-generation.lock"), []).unwrap();
    let tx = env.db.begin_write().unwrap();
    tx.execute_batch(include_str!("../store/generation_catalog/schema.sql"))
        .unwrap();
    tx.execute(
        "INSERT INTO generation_store VALUES (1, 1, ?1)",
        [epoch.as_slice()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO project_generations
         (id, project_id, layout, phase, base_revision, reserved_disk_bytes)
         VALUES (1, ?1, 'legacy', 'active', 0, 0)",
        [project],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO project_generation_head VALUES (?1, 1, 'active')",
        [project],
    )
    .unwrap();
    tx.pragma_update(None, "user_version", 5).unwrap();
    tx.commit().unwrap();

    let pool = ReadPool::prepare_for_catalog(
        env.cache.path(),
        &crate::store::settings::standalone().unwrap(),
        &crate::store::settings::standalone_read_pool().unwrap(),
        epoch,
    )
    .unwrap();
    let status = observe_catalog(&pool, &env.project, &Config::default(), None, |dir| {
        let job = crate::store::repo::create_sync_job(env.db.connection(), project).unwrap();
        assert!(job.is_some());
        index_health(dir)
    })
    .unwrap();
    assert_eq!(status.reason, Some(ReadinessReason::StateChanged));
    assert_eq!(status.active_sync.unwrap().state, "queued");
}

#[test]
fn strict_job_stats_movement_without_generation_change_is_state_changed_without_retry() {
    let env = Env::new();
    env.db.connection().execute_batch("INSERT INTO sync_jobs(project_id,state,started_at,finished_at,stats_json) VALUES(1,'error',1,2,'{}')").unwrap();
    changed(&env, "UPDATE sync_jobs SET stats_json='{\"errors\":1}'");
}

#[test]
fn project_status_movement_is_state_changed_even_when_health_fails() {
    movement("UPDATE projects SET status='error'");
}

fn movement(sql: &str) {
    changed(&Env::new(), sql);
}
fn changed(env: &Env, sql: &str) {
    let reads = Cell::new(0);
    let checks = Cell::new(0);
    let generation =
        crate::store::repo::index_generation(env.db.connection(), env.project.id).unwrap();
    let status = observe_with(
        &env.project,
        &outcome::recipe_id(&Config::default()),
        None,
        || {
            reads.set(reads.get() + 1);
            Snapshot::read(&env.db, env.project.id)
        },
        |_| {
            assert!(env.db.connection().is_autocommit());
            checks.set(checks.get() + 1);
            // A distinct connection commits between O1/O2: an O1 transaction
            // accidentally retained across health would keep O2 stale.
            let writer = Db::open(env.cache.path()).unwrap();
            writer.connection().execute_batch(sql).unwrap();
            IndexHealth::IndexUnavailable
        },
    )
    .unwrap();
    assert_eq!(status.reason, Some(ReadinessReason::StateChanged));
    assert!(!status.ready);
    assert_eq!(status.index_generation, generation);
    assert_eq!((reads.get(), checks.get()), (2, 1));
}

#[test]
fn unreadable_first_snapshot_never_checks_health_or_retries() {
    let env = Env::new();
    let reads = Cell::new(0);
    let checks = Cell::new(0);
    let error = observe_with(
        &env.project,
        &outcome::recipe_id(&Config::default()),
        None,
        || {
            reads.set(reads.get() + 1);
            Err(Error::internal("unavailable O1"))
        },
        |_| {
            checks.set(checks.get() + 1);
            IndexHealth::Healthy
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("unavailable O1"));
    assert_eq!((reads.get(), checks.get()), (1, 0));
}
