//! Fixed-project read-only readiness, using real metadata/index producers.
use docsbase_memory::config::Config;
use docsbase_memory::daemon::{readiness, registry, tools};
use docsbase_memory::index::{job, outcome, tantivy_index::IndexHandle};
use docsbase_memory::store::{Db, models::Project, read_pool::ReadPool};
use serde_json::json;

struct Env {
    db: Db,
    project: Project,
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
    config: Config,
}
impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "# Guide\n\nreadinesswidget\n").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let mut env = Self {
            db,
            project,
            root,
            cache,
            config: Config::default(),
        };
        env.direct();
        env
    }
    fn direct(&mut self) {
        tools::run_project_index(&mut self.db, self.cache.path(), &self.project, &self.config)
            .unwrap();
    }
    fn sync(&mut self) -> i64 {
        let (id, created) = tools::enqueue_sync(&self.db, &self.project).unwrap();
        assert!(created);
        tools::run_sync_job_in(
            &mut self.db,
            self.cache.path(),
            id,
            &self.project,
            &self.config,
        )
        .unwrap();
        id
    }
    fn view(&self) -> serde_json::Value {
        let pool = ReadPool::prepare(
            self.cache.path(),
            &docsbase_memory::store::settings::standalone().unwrap(),
            &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        )
        .unwrap();
        serde_json::to_value(readiness::observe(&pool, &self.project, &self.config, None).unwrap())
            .unwrap()
    }
    fn reason(&self, reason: &str) {
        let view = self.view();
        assert_eq!(view["ready"], false, "{view}");
        assert_eq!(view["reason"], reason, "{view}");
    }
    fn sql(&self, sql: &str) {
        self.db.connection().execute_batch(sql).unwrap();
    }
}

#[test]
fn full_direct_and_skipped_only_noop_are_ready() {
    let mut env = Env::new();
    assert_eq!(env.view()["ready"], true);
    let generation = env.view()["index_generation"].clone();
    env.direct();
    let view = env.view();
    assert_eq!(view["ready"], true);
    assert_eq!(view["index_generation"], generation);
    assert_eq!(view["outcome"]["stats"]["skipped"], 1);
}

#[test]
fn legacy_missing_corrupt_and_missing_strict_counts_require_sync() {
    for expression in [
        "NULL",
        "'{'",
        "json_remove(index_outcome_json, '$.stats.errors')",
    ] {
        let env = Env::new();
        env.sql(&format!(
            "UPDATE projects SET index_outcome_json = {expression}"
        ));
        env.reason("unknown_outcome");
    }
}

#[test]
fn stale_done_does_not_mask_later_same_generation_direct_partial() {
    let mut env = Env::new();
    env.sync();
    let generation = env.view()["index_generation"].clone();
    env.config.max_file_size = 1;
    env.direct();
    assert_eq!(env.view()["index_generation"], generation);
    env.reason("partial");
}

#[test]
fn latest_failed_sync_stays_blocking_after_full_direct_success() {
    let mut env = Env::new();
    env.sync();
    env.sql("INSERT INTO sync_jobs(project_id,state,started_at,finished_at,stats_json) VALUES(1,'error',1,2,NULL)");
    env.direct();
    env.reason("sync_failed");
    let generation = env.view()["index_generation"].clone();
    env.sync();
    assert_eq!(env.view()["index_generation"], generation);
    assert_eq!(env.view()["ready"], true);
}

#[test]
fn partial_sync_stays_blocking_after_full_direct_success() {
    let mut env = Env::new();
    env.config.max_file_size = 1;
    env.sync();
    env.config = Config::default();
    env.direct();
    env.reason("sync_failed");
    env.sync();
    assert_eq!(env.view()["ready"], true);
}

#[test]
fn running_queued_and_restart_interrupted_jobs_are_not_ready() {
    for (state, reason, stats) in [
        ("queued", "indexing", "null"),
        ("running", "indexing", "null"),
        ("error", "interrupted", "'{\"orphaned\":true}'"),
    ] {
        let env = Env::new();
        env.sql(&format!("INSERT INTO sync_jobs(project_id,state,started_at,stats_json) VALUES(1,'{state}',1,{stats})"));
        env.reason(reason);
    }
}

#[test]
fn zero_error_subset_revokes_full_survey_even_with_unchanged_generation() {
    let mut env = Env::new();
    env.sync();
    let generation = env.view()["index_generation"].clone();
    let mut index = IndexHandle::open_or_create(
        &env.cache
            .path()
            .join("projects")
            .join(env.project.id.to_string())
            .join("tantivy"),
    )
    .unwrap();
    job::run_incremental_with(
        &mut env.db,
        &mut index,
        &env.project,
        &[env.root.path().join("guide.md")],
        &env.config,
    )
    .unwrap();
    drop(index);
    assert_eq!(env.view()["index_generation"], generation);
    env.reason("incomplete_scope");
}

#[test]
fn done_missing_stats_or_missing_association_remains_unknown() {
    for stats in [
        json!({}),
        json!({"docs":0,"chunks":0,"skipped":1,"removed":0,"errors":0,"warnings":[]}),
    ] {
        let env = Env::new();
        env.db.connection().execute("INSERT INTO sync_jobs(project_id,state,started_at,finished_at,stats_json) VALUES(1,'done',1,2,?1)", [stats.to_string()]).unwrap();
        env.reason("unknown_outcome");
    }
}

#[test]
fn known_failed_project_is_failed_even_without_counts() {
    let env = Env::new();
    env.sql("UPDATE projects SET status='error',index_outcome_json=NULL");
    env.reason("failed");
}

#[test]
fn recipe_change_requires_full_sync() {
    let mut env = Env::new();
    env.config.ignores.push("changed/**".into());
    env.reason("recipe_changed");
    env.sync();
    assert_eq!(env.view()["ready"], true);
}

#[test]
fn absent_corrupt_and_rebuild_needed_index_never_repairs() {
    for alteration in ["missing", "corrupt", "tokenizer"] {
        let env = Env::new();
        let dir = env
            .cache
            .path()
            .join("projects")
            .join(env.project.id.to_string())
            .join("tantivy");
        match alteration {
            "missing" => std::fs::remove_file(dir.join("meta.json")).unwrap(),
            "corrupt" => std::fs::write(dir.join("meta.json"), "invalid-json").unwrap(),
            _ => std::fs::remove_file(
                dir.join(docsbase_memory::index::tantivy_index::TOKENIZER_VERSION_FILE),
            )
            .unwrap(),
        }
        let before: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let view = env.view();
        assert_eq!(view["ready"], false);
        assert!(
            matches!(
                view["reason"].as_str(),
                Some("index_unavailable" | "rebuild_required")
            ),
            "{view}"
        );
        let after: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before, after);
    }
}

#[test]
fn lexical_completion_is_not_terminal_sync_success() {
    let mut env = Env::new();
    let id = env.sync();
    env.sql(&format!("UPDATE sync_jobs SET state='error',stats_json='{{\"error\":\"backfill failed\"}}' WHERE id={id}"));
    assert!(
        outcome::read(&env.db, env.project.id)
            .unwrap()
            .unwrap()
            .is_complete()
    );
    env.reason("sync_failed");
}

#[test]
fn nonindexed_and_inflight_project_status_never_promote_old_proof() {
    for (status, reason) in [
        ("not_indexed", "index_unavailable"),
        ("indexing", "indexing"),
    ] {
        let env = Env::new();
        env.sql(&format!("UPDATE projects SET status='{status}'"));
        env.reason(reason);
    }
}

#[test]
fn readiness_does_not_create_a_missing_project_coordination_file() {
    let env = Env::new();
    let state = env
        .cache
        .path()
        .join("projects")
        .join(env.project.id.to_string());
    // Remove only the project coordination file after the actual writer exits.
    let lease = state.join(".writer.lock");
    if lease.exists() {
        std::fs::remove_file(&lease).unwrap();
    }
    let before: Vec<_> = std::fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let pool = ReadPool::prepare(
        env.cache.path(),
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
    )
    .unwrap();
    let _ = readiness::observe(&pool, &env.project, &env.config, None);
    let after: Vec<_> = std::fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn known_partial_sync_with_missing_other_counts_never_becomes_unknown_success() {
    let mut env = Env::new();
    env.sync();
    env.sql("UPDATE sync_jobs SET stats_json='{\"errors\":2}'");
    env.direct();
    env.reason("sync_failed");
}

#[test]
fn current_full_outcome_missing_count_does_not_reuse_prior_done() {
    let mut env = Env::new();
    env.sync();
    env.sql(
        "UPDATE projects SET index_outcome_json=json_remove(index_outcome_json,'$.stats.skipped')",
    );
    env.reason("unknown_outcome");
}

#[test]
fn exhausted_prepared_reader_admission_performs_no_repair_or_fallback_open() {
    let env = Env::new();
    let pool = ReadPool::prepare(
        env.cache.path(),
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
    )
    .unwrap();
    let held: Vec<_> = (0..pool.settings().settings().max_connections)
        .map(|_| pool.try_acquire().unwrap())
        .collect();
    let error = readiness::observe(&pool, &env.project, &env.config, None).unwrap_err();
    assert!(matches!(
        error,
        docsbase_memory::error::Error::Admission { .. }
    ));
    drop(held);
    assert!(
        readiness::observe(&pool, &env.project, &env.config, None)
            .unwrap()
            .ready
    );
}
