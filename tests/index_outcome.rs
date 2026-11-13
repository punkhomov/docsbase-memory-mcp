use std::fs;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::index::job::{JobStats, run_full, run_incremental_with};
use docsbase_memory::index::outcome::{self, SurveyScope};
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use tempfile::TempDir;

struct Env {
    index: Option<IndexHandle>,
    db: Option<Db>,
    project: Project,
    root: TempDir,
    cache: TempDir,
}

impl Env {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let cache = TempDir::new().unwrap();
        fs::write(root.path().join("guide.md"), "# Guide\n\nwidgets\n").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let index = IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).unwrap();
        Self {
            index: Some(index),
            db: Some(db),
            project,
            root,
            cache,
        }
    }

    fn db(&self) -> &Db {
        self.db.as_ref().unwrap()
    }

    fn full(&mut self, config: &Config) -> JobStats {
        run_full(
            self.db.as_mut().unwrap(),
            self.index.as_mut().unwrap(),
            &self.project,
            config,
        )
        .unwrap()
    }

    fn subset(&mut self, paths: &[&str], config: &Config) -> JobStats {
        let paths: Vec<_> = paths
            .iter()
            .map(|path| self.root.path().join(path))
            .collect();
        run_incremental_with(
            self.db.as_mut().unwrap(),
            self.index.as_mut().unwrap(),
            &self.project,
            &paths,
            config,
        )
        .unwrap()
    }

    fn read(&self) -> outcome::IndexOutcome {
        let reader = Db::open_readonly(self.cache.path()).unwrap();
        outcome::read(&reader, self.project.id).unwrap().unwrap()
    }
}

#[test]
fn full_and_unchanged_survey_publish_fresh_attributable_stats() {
    let mut env = Env::new();
    let config = Config::default();
    let stats = env.full(&config);
    let first = env.read();
    assert_eq!(first.stats, stats);
    assert_eq!(first.scope, SurveyScope::FullSurvey);
    assert!(first.is_complete());
    assert_eq!(
        first.canonical_root,
        env.project.canonical_root.to_string_lossy()
    );
    assert_eq!(first.recipe_id, outcome::recipe_id(&config));
    assert_eq!(
        registry::project_by_id(env.db(), env.project.id)
            .unwrap()
            .unwrap()
            .status,
        ProjectStatus::Indexed
    );
    let stats = env.full(&config);
    let second = env.read();
    assert_eq!(stats.skipped, 1);
    assert_eq!(second.stats, stats);
    assert!(second.is_complete());
    assert!(second.attempt_id > first.attempt_id);
    assert_eq!(second.index_generation, first.index_generation);
}

#[test]
fn partial_full_survey_supersedes_success_without_generation_change() {
    let mut env = Env::new();
    let config = Config {
        max_file_size: 128,
        ..Config::default()
    };
    env.full(&config);
    let before = env.read();
    fs::write(env.root.path().join("too-large.md"), "x".repeat(256)).unwrap();
    let stats = env.full(&config);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.skipped, 1);
    let partial = env.read();
    assert_eq!(partial.stats, stats);
    assert!(!partial.is_complete());
    assert_eq!(partial.index_generation, before.index_generation);
    assert!(partial.attempt_id > before.attempt_id);
    env.full(&config);
    let repeated = env.read();
    assert_eq!(repeated.stats.errors, 1);
    assert!(!repeated.is_complete());
    assert!(repeated.attempt_id > partial.attempt_id);
}

#[test]
fn subset_never_promotes_unknown_partial_or_previous_full_coverage() {
    let mut env = Env::new();
    let config = Config::default();
    env.full(&config);
    registry::mark_indexed(env.db(), env.project.id).unwrap();
    assert!(outcome::read(env.db(), env.project.id).unwrap().is_none());
    env.subset(&["guide.md"], &config);
    let unknown = env.read();
    assert_eq!(unknown.scope, SurveyScope::Subset);
    assert_eq!(unknown.stats.errors, 0);
    assert!(!unknown.is_complete());
    env.full(&config);
    let complete = env.read();
    assert!(complete.is_complete());
    env.subset(&["guide.md"], &config);
    let subset = env.read();
    assert_eq!(subset.stats.skipped, 1);
    assert_eq!(subset.index_generation, complete.index_generation);
    assert!(!subset.is_complete());
    let partial_config = Config {
        max_file_size: 2,
        ..config.clone()
    };
    assert_eq!(env.full(&partial_config).errors, 1);
    env.subset(&[], &partial_config);
    let subset = env.read();
    assert_eq!(subset.stats.errors, 0);
    assert!(!subset.is_complete());
}

#[test]
fn incremental_recreated_index_records_real_full_recovery_scope() {
    let mut env = Env::new();
    env.full(&Config::default());
    fs::write(env.root.path().join("other.md"), "# Other\n\nwalruses\n").unwrap();
    drop(env.index.take());
    let dir = env.cache.path().join("projects/1/tantivy");
    fs::remove_dir_all(&dir).unwrap();
    env.index = Some(IndexHandle::open_or_create(&dir).unwrap());
    let stats = env.subset(&["guide.md"], &Config::default());
    assert_eq!(stats.docs, 2);
    let result = env.read();
    assert_eq!(result.scope, SurveyScope::FullSurvey);
    assert!(result.is_complete());
    assert_eq!(
        env.index
            .as_ref()
            .unwrap()
            .search("walruses", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn fatal_full_walk_failure_revokes_previous_proof_before_any_apply() {
    let mut env = Env::new();
    env.full(&Config::default());
    let before = env.read();
    let bad = Config {
        ignores: vec!["../outside".to_owned()],
        ..Config::default()
    };
    let error = run_full(
        env.db.as_mut().unwrap(),
        env.index.as_mut().unwrap(),
        &env.project,
        &bad,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("outside") || error.to_string().contains("escape"),
        "{error}"
    );
    assert!(outcome::read(env.db(), env.project.id).unwrap().is_none());
    assert_eq!(
        docsbase_memory::store::repo::index_generation(env.db().connection(), env.project.id)
            .unwrap(),
        before.index_generation
    );
}

#[test]
fn manual_lifecycle_status_cannot_reuse_an_old_outcome() {
    let mut env = Env::new();
    env.full(&Config::default());
    registry::set_status(env.db(), env.project.id, ProjectStatus::Indexed).unwrap();
    assert!(outcome::read(env.db(), env.project.id).unwrap().is_none());
    env.full(&Config::default());
    registry::mark_indexed(env.db(), env.project.id).unwrap();
    assert!(outcome::read(env.db(), env.project.id).unwrap().is_none());
}

#[test]
fn recipe_changes_only_with_index_inputs_and_algorithm_identity() {
    let config = Config::default();
    let recipe = outcome::recipe_id(&config);
    let mut changed = config.clone();
    changed.max_file_size += 1;
    assert_ne!(outcome::recipe_id(&changed), recipe);
    changed = config.clone();
    changed.max_docs_per_project += 1;
    assert_ne!(outcome::recipe_id(&changed), recipe);
    changed = config.clone();
    changed.ignores.push("private/**".to_owned());
    assert_ne!(outcome::recipe_id(&changed), recipe);
    let mut irrelevant = config.clone();
    irrelevant.auto_index = !irrelevant.auto_index;
    irrelevant.hybrid = !irrelevant.hybrid;
    irrelevant.search.request_timeout_ms += 1;
    assert_eq!(outcome::recipe_id(&irrelevant), recipe);
}

#[test]
fn corrupt_oversized_or_unattributable_metadata_is_unknown() {
    let mut env = Env::new();
    env.full(&Config::default());
    let original: String = env
        .db()
        .connection()
        .query_row(
            "SELECT index_outcome_json FROM projects WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for raw in [
        "{}".to_owned(),
        "not json".to_owned(),
        "x".repeat(1_048_577),
    ] {
        env.db()
            .connection()
            .execute(
                "UPDATE projects SET index_outcome_json=?1 WHERE id=1",
                [&raw],
            )
            .unwrap();
        assert!(outcome::read(env.db(), 1).unwrap().is_none());
    }
    for (key, value) in [
        ("schema_version", serde_json::json!(999)),
        ("attempt_id", serde_json::json!(999)),
        ("index_generation", serde_json::json!(999)),
        ("canonical_root", serde_json::json!("different-root")),
        ("scope", serde_json::json!("unknown")),
        ("recipe_id", serde_json::json!("unversioned")),
    ] {
        let mut record: serde_json::Value = serde_json::from_str(&original).unwrap();
        record[key] = value;
        env.db()
            .connection()
            .execute(
                "UPDATE projects SET index_outcome_json=?1 WHERE id=1",
                [record.to_string()],
            )
            .unwrap();
        assert!(outcome::read(env.db(), 1).unwrap().is_none(), "field {key}");
    }
    env.db()
        .connection()
        .execute(
            "UPDATE projects SET index_outcome_json=?1 WHERE id=1",
            [&original],
        )
        .unwrap();
    assert!(outcome::read(env.db(), 1).unwrap().is_some());
    let large = "x".repeat(1_048_577);
    env.db()
        .connection()
        .execute("UPDATE projects SET canonical_root=?1 WHERE id=1", [&large])
        .unwrap();
    assert!(outcome::read(env.db(), 1).unwrap().is_none());
    env.db()
        .connection()
        .execute(
            "UPDATE projects SET canonical_root=?1, status=?2 WHERE id=1",
            rusqlite::params![env.project.canonical_root.to_string_lossy(), large],
        )
        .unwrap();
    assert!(outcome::read(env.db(), 1).unwrap().is_none());
    env.db()
        .connection()
        .execute("UPDATE projects SET status='indexed' WHERE id=1", [])
        .unwrap();
    assert!(outcome::read(env.db(), 1).unwrap().is_some());
    env.db()
        .connection()
        .execute("UPDATE docs SET content_hash='' WHERE project_id=1", [])
        .unwrap();
    assert!(outcome::read(env.db(), 1).unwrap().is_none());
}

#[test]
fn v2_upgrade_preserves_lexical_data_without_inventing_historical_completeness() {
    let mut env = Env::new();
    env.full(&Config::default());
    let before = env.read().index_generation;
    let conn = env.db().connection();
    conn.execute("INSERT INTO projects (canonical_root,name,status,schema_version,created_at) VALUES (?1,'stale','indexed',1,0)", [env.project.canonical_root.join("stale").to_string_lossy()]).unwrap();
    conn.execute_batch("ALTER TABLE projects DROP COLUMN index_attempt; ALTER TABLE projects DROP COLUMN index_outcome_json; UPDATE projects SET schema_version=2 WHERE id=1; PRAGMA user_version=2;").unwrap();
    drop(env.db.take());
    env.db = Some(Db::open(env.cache.path()).unwrap());
    let project = registry::project_by_id(env.db(), 1).unwrap().unwrap();
    assert_eq!(project.status, ProjectStatus::Indexed);
    assert_eq!(
        project.schema_version,
        docsbase_memory::store::migrations::SCHEMA_VERSION
    );
    assert_eq!(
        docsbase_memory::store::repo::index_generation(env.db().connection(), 1).unwrap(),
        before
    );
    assert!(outcome::read(env.db(), 1).unwrap().is_none());
    assert_eq!(
        registry::project_by_id(env.db(), 2)
            .unwrap()
            .unwrap()
            .schema_version,
        1,
        "the additive migration must not revive stale lexical stamps"
    );
    assert_eq!(
        env.index
            .as_ref()
            .unwrap()
            .search("widgets", 10)
            .unwrap()
            .len(),
        1
    );
    let stats = env.full(&Config::default());
    assert_eq!(stats.skipped, 1);
    assert!(env.read().is_complete());
}

#[test]
fn direct_cli_and_real_sync_publish_the_shared_outcome() {
    let mut env = Env::new();
    drop(env.index.take());
    let output = assert_cmd::cargo::cargo_bin_cmd!("docsbase")
        .args(["index", "."])
        .current_dir(env.root.path())
        .env("DOCSBASE_CACHE_DIR", env.cache.path())
        .env("DOCSBASE_CONFIG_DIR", env.cache.path().join("config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let direct = env.read();
    assert!(direct.is_complete());
    let (job_id, created) =
        docsbase_memory::daemon::tools::enqueue_sync(env.db(), &env.project).unwrap();
    assert!(created);
    let stats = docsbase_memory::daemon::tools::run_sync_job_in(
        env.db.as_mut().unwrap(),
        env.cache.path(),
        job_id,
        &env.project,
        &Config::default(),
    )
    .unwrap();
    let synced = env.read();
    assert!(synced.is_complete());
    assert_eq!(synced.stats, stats);
    assert_eq!(stats.skipped, 1);
    assert_eq!(synced.index_generation, direct.index_generation);
    assert!(synced.attempt_id > direct.attempt_id);
}

struct ExitEmbedder;
impl docsbase_memory::vector_cache::Embedder for ExitEmbedder {
    fn model_id(&self) -> &'static str {
        "io1-interrupted@d2@l2"
    }
    fn embed(&self, _: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        std::process::exit(23)
    }
}

#[test]
fn interrupted_index_child() {
    let Some(cache) = std::env::var_os("DOCSBASE_IO1_CHILD_CACHE") else {
        return;
    };
    let cache = std::path::PathBuf::from(cache);
    let mut db = Db::open(&cache).unwrap();
    let project = registry::project_by_id(&db, 1).unwrap().unwrap();
    let stages = docsbase_memory::hybrid::HybridStages {
        embedder: Some(std::sync::Arc::new(ExitEmbedder)),
        ..docsbase_memory::hybrid::HybridStages::default()
    };
    let source = docsbase_memory::vector_cache::VectorCacheSource::Legacy(
        docsbase_memory::vector_cache::CacheLimits::default(),
    );
    docsbase_memory::daemon::tools::run_project_index_with_vectors(
        &mut db,
        &cache,
        &project,
        &Config::default(),
        &stages,
        &source,
    )
    .unwrap();
    panic!("child must terminate inside embedding");
}

#[test]
fn process_death_after_begin_leaves_no_historical_readiness() {
    let mut env = Env::new();
    env.full(&Config::default());
    let before = env.read();
    fs::write(
        env.root.path().join("guide.md"),
        "# Guide\n\nchanged widgets\n",
    )
    .unwrap();
    drop(env.index.take());
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interrupted_index_child", "--nocapture"])
        .env("DOCSBASE_IO1_CHILD_CACHE", env.cache.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert!(outcome::read(env.db(), 1).unwrap().is_none());
    assert_eq!(
        docsbase_memory::store::repo::index_generation(env.db().connection(), 1).unwrap(),
        before.index_generation
    );
    assert!(docsbase_memory::store::repo::has_pending_docs(env.db().connection(), 1).unwrap());
    assert_eq!(
        registry::project_by_id(env.db(), 1)
            .unwrap()
            .unwrap()
            .status,
        ProjectStatus::Indexing
    );
}

#[test]
fn embedding_observes_revoked_proof_and_writer_lease_without_a_sql_transaction() {
    struct Observer {
        cache: std::path::PathBuf,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl docsbase_memory::vector_cache::Embedder for Observer {
        fn model_id(&self) -> &'static str {
            "io1-observer@d2@l2"
        }
        fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let db = Db::open_readonly(&self.cache).unwrap();
            assert!(outcome::read(&db, 1).unwrap().is_none());
            assert_eq!(
                registry::project_by_id(&db, 1).unwrap().unwrap().status,
                ProjectStatus::Indexing
            );
            let conflict =
                docsbase_memory::daemon::lifecycle::with_project_read_lease(&self.cache, 1, || {
                    Ok(())
                })
                .unwrap_err();
            assert!(conflict.to_string().contains("another writer"));
            let raw = rusqlite::Connection::open(self.cache.join(docsbase_memory::store::DB_FILE))
                .unwrap();
            raw.busy_timeout(std::time::Duration::ZERO).unwrap();
            raw.execute("UPDATE projects SET name=name WHERE id=1", [])
                .expect("embedding must not retain a SQL writer");
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }
    let mut env = Env::new();
    env.full(&Config::default());
    fs::write(
        env.root.path().join("guide.md"),
        "# Guide\n\nnew embedded widgets\n",
    )
    .unwrap();
    drop(env.index.take());
    let observer = std::sync::Arc::new(Observer {
        cache: env.cache.path().to_path_buf(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let stages = docsbase_memory::hybrid::HybridStages {
        embedder: Some(observer.clone()),
        ..Default::default()
    };
    let source = docsbase_memory::vector_cache::VectorCacheSource::Legacy(
        docsbase_memory::vector_cache::CacheLimits::default(),
    );
    let stats = docsbase_memory::daemon::tools::run_project_index_with_vectors(
        env.db.as_mut().unwrap(),
        env.cache.path(),
        &env.project,
        &Config::default(),
        &stages,
        &source,
    )
    .unwrap();
    assert_eq!(observer.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(env.read().stats, stats);
    assert!(env.read().is_complete());
}

#[test]
fn malformed_numeric_and_non_utf8_metadata_is_unknown() {
    let mut env = Env::new();
    env.full(&Config::default());
    let original = env.read();
    let json = serde_json::to_string(&original).unwrap();
    for (column, value) in [
        ("index_generation", "'not-an-integer'"),
        ("index_generation", "1.25"),
        ("schema_version", "'not-an-integer'"),
        ("schema_version", "3.25"),
        ("canonical_root", "CAST(X'80' AS TEXT)"),
        ("index_outcome_json", "CAST(X'80' AS TEXT)"),
    ] {
        env.db()
            .connection()
            .execute(
                &format!("UPDATE projects SET {column}={value} WHERE id=1"),
                [],
            )
            .unwrap();
        assert!(
            matches!(outcome::read(env.db(), 1), Ok(None)),
            "corrupt {column} must be unknown"
        );
        env.db().connection().execute("UPDATE projects SET index_generation=?1, schema_version=?2, canonical_root=?3, index_outcome_json=?4 WHERE id=1", rusqlite::params![original.index_generation, docsbase_memory::store::migrations::SCHEMA_VERSION, original.canonical_root, json]).unwrap();
    }
    assert!(env.read().is_complete());
}
