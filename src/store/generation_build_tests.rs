use super::*;
use crate::daemon::{registry::ensure_project, tools::run_project_index};
use crate::index::tantivy_index::ReadIndex;
use crate::store::generation_catalog;
use crate::store::registry_inspect::{ProjectIndexObservation, inspect_project_index};
use crate::store::{DB_FILE, Db, settings};
use rusqlite::Connection;
use std::fs;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    source: TempDir,
    base: ObservedProjectIndex,
}

fn limits() -> InspectionLimits {
    InspectionLimits {
        max_projects: 8,
        max_metadata_bytes: 65_536,
        max_sqlite_value_bytes: 1_048_576,
        sqlite_cache_kib: 64,
    }
}

impl Env {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("guide.md"),
            "# Apple\n\nApplemigration\n",
        )
        .unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = ensure_project(&mut db, source.path()).unwrap();
        run_project_index(&mut db, cache.path(), &project, &Config::default()).unwrap();
        let project_dir = cache.path().join("projects").join(project.id.to_string());
        fs::write(project_dir.join(".legacy-generation.lock"), []).unwrap();
        let tx = db.begin_write().unwrap();
        tx.execute_batch(generation_catalog::SCHEMA_SQL).unwrap();
        tx.execute(
            "INSERT INTO generation_store VALUES (1, 1, ?1)",
            [[7u8; 16].as_slice()],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO project_generations
               (id, project_id, layout, phase, base_revision, reserved_disk_bytes)
             VALUES (1, ?1, 'legacy', 'active', 0, 0)",
            [project.id],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO project_generation_head VALUES (?1, 1, 'active')",
            [project.id],
        )
        .unwrap();
        tx.pragma_update(None, "user_version", generation_catalog::SCHEMA_VERSION)
            .unwrap();
        tx.commit().unwrap();
        drop(db);
        let ProjectIndexObservation::Present(base) = inspect_project_index(
            cache.path(),
            project.id,
            &settings::standalone().unwrap(),
            limits(),
        )
        .unwrap() else {
            panic!("fixture must have an active legacy pair")
        };
        Self {
            cache,
            source,
            base,
        }
    }

    fn request(&self) -> LexicalBuildRequest {
        LexicalBuildRequest {
            project_id: self.base.project_id,
            epoch: [7; 16],
            base: Some(self.base),
            plan_digest: [5; 32],
            reserved_disk_bytes: 16 * 1024 * 1024,
        }
    }

    fn build(&self, request: LexicalBuildRequest) -> Result<PreparedLexicalGeneration> {
        prepare_lexical_generation(
            self.cache.path(),
            request,
            &settings::standalone().unwrap(),
            limits(),
            &Config::default(),
        )
    }

    fn register_second_project(&self, root: &Path) -> LexicalBuildRequest {
        let conn = Connection::open(self.cache.path().join(DB_FILE)).unwrap();
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (2, ?1, 'second', 'not_indexed', 3, 0)",
            [root.canonicalize().unwrap().to_str().unwrap()],
        )
        .unwrap();
        LexicalBuildRequest {
            project_id: 2,
            base: None,
            ..self.request()
        }
    }

    fn generation(&self, id: i64) -> std::path::PathBuf {
        // Internal verification takes the canonical cache used by the publisher.
        self.cache
            .path()
            .canonicalize()
            .unwrap()
            .join("projects/1/generations")
            .join(id.to_string())
    }

    fn catalog_state(&self) -> (i64, Option<String>) {
        let conn = Connection::open(self.cache.path().join(DB_FILE)).unwrap();
        conn.query_row(
            "SELECT active_id, (SELECT phase FROM project_generations WHERE id = 2) FROM project_generation_head",
            [],
            |row| Ok((row.get(0)?, row.get(1)?))
        ).unwrap()
    }

    fn assert_legacy_search(&self) {
        let path = self.cache.path().join("projects/1/tantivy");
        let index = ReadIndex::open(&path).unwrap();
        assert_eq!(index.search("Applemigration", 5).unwrap().len(), 1);
        assert_eq!(index.search("Peachmigration", 5).unwrap(), []);
        assert_eq!(
            inspect_project_index(
                self.cache.path(),
                1,
                &settings::standalone().unwrap(),
                limits()
            )
            .unwrap(),
            ProjectIndexObservation::Present(self.base)
        );
    }
}

#[test]
fn real_lexical_build_binds_the_inactive_pair_and_preserves_legacy_search() {
    let env = Env::new();
    fs::write(
        env.source.path().join("guide.md"),
        "# Peach\n\nPeachmigration\n",
    )
    .unwrap();
    fs::write(env.source.path().join("ignored.md"), "# Forbiddenorange").unwrap();
    let config = Config {
        ignores: vec!["ignored.md".into()],
        ..Config::default()
    };
    let request = env.request();
    let result = prepare_lexical_generation(
        env.cache.path(),
        request,
        &settings::standalone().unwrap(),
        limits(),
        &config,
    )
    .unwrap();
    assert_eq!(
        (
            result.project_id,
            result.generation_id,
            result.content_revision
        ),
        (1, 2, 1)
    );
    assert!((1..=request.reserved_disk_bytes).contains(&result.bytes));
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    let registry = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
    let fields: (i64, i64, String, i64) = registry.query_row(
        "SELECT base_id, base_revision, plan_id, reserved_disk_bytes FROM project_generations WHERE id = 2",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    ).unwrap();
    assert_eq!(
        fields,
        (
            env.base.generation_id.unwrap(),
            env.base.content_revision,
            blake3::Hash::from_bytes(request.plan_digest)
                .to_hex()
                .to_string(),
            i64::try_from(request.reserved_disk_bytes).unwrap(),
        )
    );
    drop(registry);
    env.assert_legacy_search();

    let generation = env.generation(2);
    let index = ReadIndex::open(&generation.join("tantivy")).unwrap();
    let hits = index.search("Peachmigration", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(index.search("Applemigration", 5).unwrap(), []);
    assert_eq!(index.search("Forbiddenorange", 5).unwrap(), []);
    let conn = Connection::open(generation.join("metadata.db")).unwrap();
    let ids: (i64, i64, i64) = conn
        .query_row(
            "SELECT project_id, generation_id, content_revision FROM generation_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(ids, (1, 2, 1));
    let payload: String = conn
        .query_row(
            "SELECT text FROM chunks WHERE doc_id = ?1",
            [hits[0].doc_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(payload.contains("Peachmigration"));
    let outcome_json: String = conn
        .query_row(
            "SELECT index_outcome_json FROM projects WHERE id = ?1",
            [result.project_id],
            |row| row.get(0),
        )
        .unwrap();
    let outcome: crate::index::outcome::IndexOutcome = serde_json::from_str(&outcome_json).unwrap();
    assert_eq!(outcome.recipe_id, crate::index::outcome::recipe_id(&config));
    assert!(!generation.join("seal.json").exists());
}

#[test]
#[cfg(unix)]
fn publication_selects_the_sealed_pair_and_preserves_the_previous_pair() {
    let env = Env::new();
    fs::write(
        env.source.path().join("guide.md"),
        "# Peach\n\nPeachmigration\n",
    )
    .unwrap();
    let request = env.request();
    let staged = env.build(request).unwrap();
    let db = settings::standalone().unwrap();
    env.assert_legacy_search();
    let target = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &db,
        limits(),
        &Config::default(),
    )
    .unwrap();
    assert_eq!(target.layout, ProjectIndexLayout::Local);
    assert_eq!(target.generation_id, Some(staged.generation_id));
    assert_eq!(
        inspect_project_index(env.cache.path(), staged.project_id, &db, limits()).unwrap(),
        ProjectIndexObservation::Present(target)
    );
    let current = ReadIndex::open(&env.generation(staged.generation_id).join("tantivy")).unwrap();
    assert_eq!(current.search("Peachmigration", 2).unwrap().len(), 1);
    let previous = ReadIndex::open(&env.cache.path().join("projects/1/tantivy")).unwrap();
    assert_eq!(previous.search("Applemigration", 2).unwrap().len(), 1);
    let retried = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &db,
        limits(),
        &Config::default(),
    )
    .unwrap();
    assert_eq!(retried, target);
}

#[cfg(unix)]
fn retire_local_pair(env: &Env) -> (ObservedProjectIndex, ObservedProjectIndex) {
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let build_and_publish = |request| {
        let staged = env.build(request).unwrap();
        publish_lexical_generation(
            env.cache.path(),
            request,
            staged,
            &settings,
            limits(),
            &config,
        )
        .unwrap()
    };
    let previous_request = env.request();
    let previous = build_and_publish(previous_request);
    fs::write(
        env.source.path().join("guide.md"),
        "# New\n\nCherryupdate\n",
    )
    .unwrap();
    let next_request = LexicalBuildRequest {
        base: Some(previous),
        ..previous_request
    };
    (previous, build_and_publish(next_request))
}

#[test]
#[cfg(unix)]
fn catalog_reader_holds_the_pair_lease_and_distinguishes_equal_revisions() {
    use crate::store::project_index::with_catalog_pair;

    let env = Env::new();
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let publish = |request, staged| {
        publish_lexical_generation(
            env.cache.path(),
            request,
            staged,
            &settings,
            limits(),
            &config,
        )
        .unwrap()
    };
    let read = |expected: ObservedProjectIndex, needle: &str| {
        let db = Db::open_readonly_for_search(env.cache.path(), &settings).unwrap();
        let project = registry::project_by_id(&db, expected.project_id)
            .unwrap()
            .unwrap();
        let directory = env.generation(expected.generation_id.unwrap());
        let path = directory.join(".read.lock");
        let mut exclusive = RwLock::new(open_coordination_file(&path, false).unwrap());
        let binding = with_catalog_pair(&db, &project, |_, local, binding| {
            let local = local.unwrap();
            let tantivy = local.tantivy_dir()?;
            assert_eq!(tantivy, directory.join("tantivy"));
            drop(local);
            assert_eq!(
                exclusive.try_write().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
            );
            let index = ReadIndex::open(&tantivy)?;
            assert_eq!(index.search(needle, 2)?.len(), 1);
            Ok(binding)
        })
        .unwrap();
        assert!(exclusive.try_write().is_ok());
        binding
    };

    let first_request = env.request();
    let first_staged = env.build(first_request).unwrap();
    let first = publish(first_request, first_staged);
    let first_binding = read(first, "Applemigration");
    // The read-only WAL client may leave coordination files. An accepted
    // maintenance retry recovers them before the next quiescent preparation.
    assert_eq!(publish(first_request, first_staged), first);
    fs::write(
        env.source.path().join("guide.md"),
        "# New\n\nCherryupdate\n",
    )
    .unwrap();
    let next_request = LexicalBuildRequest {
        base: Some(first),
        ..env.request()
    };
    let next = publish(next_request, env.build(next_request).unwrap());
    let next_binding = read(next, "Cherryupdate");

    assert_eq!(
        first_binding.content_revision(),
        next_binding.content_revision()
    );
    assert_ne!(first_binding, next_binding);

    let current = env.generation(next.generation_id.unwrap()).join("tantivy");
    let previous = env.generation(first.generation_id.unwrap()).join("tantivy");
    fs::rename(&current, current.with_file_name("saved-tantivy")).unwrap();
    fs::rename(previous, current).unwrap();

    let db = Db::open_readonly_for_search(env.cache.path(), &settings).unwrap();
    let project = registry::project_by_id(&db, next.project_id)
        .unwrap()
        .unwrap();
    let error =
        with_catalog_pair(&db, &project, |_, local, _| local.unwrap().tantivy_dir()).unwrap_err();
    assert!(error.to_string().contains("Tantivy"), "{error}");
}

#[cfg(unix)]
struct RevisablePair {
    env: Env,
    policy: Resolved<DbSettings>,
    config: Config,
    registry: Db,
    project: Project,
    request: LexicalBuildRequest,
    generation: i64,
}

#[cfg(unix)]
impl RevisablePair {
    fn new() -> Self {
        let env = Env::new();
        let request = env.request();
        let policy = settings::standalone().unwrap();
        let config = Config::default();
        let first = publish_lexical_generation(
            env.cache.path(),
            request,
            env.build(request).unwrap(),
            &policy,
            limits(),
            &config,
        )
        .unwrap();
        let generation = first.generation_id.unwrap();
        let registry = Db::open_readonly_for_search(env.cache.path(), &policy).unwrap();
        let project = registry::project_by_id(&registry, first.project_id)
            .unwrap()
            .unwrap();
        Self {
            env,
            policy,
            config,
            registry,
            project,
            request,
            generation,
        }
    }

    fn dir(&self) -> std::path::PathBuf {
        self.env.generation(self.generation)
    }

    fn use_schema_1(&self) -> Vec<u8> {
        let metadata_path = self.dir().join("metadata.db");
        let metadata = Connection::open(&metadata_path).unwrap();
        metadata
            .execute_batch(
                "ALTER TABLE generation_state DROP COLUMN mutation_pending;
             ALTER TABLE generation_state DROP COLUMN tantivy_manifest;
             PRAGMA user_version=1;",
            )
            .unwrap();
        drop(metadata);
        let seal_path = self.dir().join("seal.json");
        let mut seal: crate::store::generation_seal::GenerationSeal =
            serde_json::from_slice(&fs::read(&seal_path).unwrap()).unwrap();
        seal.metadata_schema = 1;
        for (_, bytes) in seal
            .files
            .iter_mut()
            .filter(|(name, _)| name == "metadata.db")
        {
            *bytes = metadata_path.metadata().unwrap().len();
        }
        let seal = serde_json::to_vec(&seal).unwrap();
        fs::write(&seal_path, &seal).unwrap();
        let connection = Connection::open(self.env.cache.path().join(DB_FILE)).unwrap();
        connection
            .execute(
                "UPDATE project_generations SET seal_digest=?1 WHERE id=?2",
                params![blake3::hash(&seal).as_bytes().as_slice(), self.generation],
            )
            .unwrap();
        seal
    }

    fn write_source(&self, text: &str) {
        fs::write(self.env.source.path().join("guide.md"), text).unwrap();
    }

    fn apply(&self) -> Result<job::JobStats> {
        let input =
            job::JobInput::Incremental(vec![self.env.source.path().join("guide.md")].into());
        crate::store::project_index::apply_lexical_batch(
            &self.registry,
            &self.project,
            &self.config,
            &input,
            None,
            || Ok(()),
        )
    }

    fn search(&self, query: &str) -> Result<(crate::store::project_index::ProjectIndex, usize)> {
        crate::store::project_index::with_catalog_pair(
            &self.registry,
            &self.project,
            |_, local, binding| {
                let dir = local.unwrap().tantivy_dir()?;
                let hits = ReadIndex::open(&dir)?.search(query, 2)?;
                Ok((binding, hits.len()))
            },
        )
    }

    fn rebuild(&self, admit: impl FnOnce() -> Result<()>) -> Result<job::JobStats> {
        let budget = crate::search::budget::PayloadBudget::new(self.config.search.clone()).unwrap();
        let operation = crate::search::operation::SearchOperation::new(
            budget.settings(),
            std::time::Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )
        .unwrap();
        let account = budget.reserve(0).unwrap();
        let mut capture = job::CaptureBudget {
            account: &account,
            operation: &operation,
            observed: None,
        };
        let rebuild = crate::index::generation_job::GenerationJob {
            cache: self.env.cache.path(),
            project: &self.project,
            epoch: self.request.epoch,
            settings: &self.policy,
            config: &self.config,
        };
        rebuild.prepare(&mut capture, |pair| {
            pair.publish(|commit| commit.commit(admit))
        })
    }
}

#[test]
#[cfg(unix)]
fn catalog_reads_incremental_revision_after_schema_1_upgrade() {
    let pair = RevisablePair::new();
    let directory = pair.dir();
    let tantivy = directory.join("tantivy");
    let seal = pair.use_schema_1();
    let (initial, hits) = pair.search("Applemigration").unwrap();
    assert_eq!(hits, 1);

    pair.write_source("# Cherry\n\nCherryupdatedbatch\n");
    assert_eq!(pair.apply().unwrap().docs, 1);
    let (current, hits) = pair.search("Cherryupdatedbatch").unwrap();
    assert_eq!(current.generation_id(), Some(pair.generation));
    assert_eq!(current.content_revision(), initial.content_revision() + 1);
    assert_eq!(hits, 1);
    assert_eq!(pair.search("Applemigration").unwrap().1, 0);
    let manifest = fs::read(tantivy.join("meta.json")).unwrap();
    assert_eq!(pair.apply().unwrap().docs, 0);
    assert_eq!(pair.search("Cherryupdatedbatch").unwrap().0, current);
    assert_eq!(fs::read(tantivy.join("meta.json")).unwrap(), manifest);
    assert_eq!(fs::read(directory.join("seal.json")).unwrap(), seal);
    let cache = pair.env.cache.path().canonicalize().unwrap();
    assert_eq!(
        registry_inspect::observe_project(
            pair.registry.connection(),
            &cache,
            pair.project.id,
            limits(),
            0,
        )
        .unwrap()
        .unwrap()
        .content_revision,
        current.content_revision(),
    );

    let metadata = Connection::open(directory.join("metadata.db")).unwrap();
    let schema: i64 = metadata
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(schema, 2);
    metadata
        .execute(
            "UPDATE generation_state SET tantivy_manifest=zeroblob(32)",
            [],
        )
        .unwrap();
    assert!(
        pair.search("Cherryupdatedbatch")
            .unwrap_err()
            .to_string()
            .contains("Tantivy")
    );
}

#[test]
#[cfg(unix)]
fn catalog_rebuild_replaces_an_interrupted_revision_and_resumes_cleanup() {
    let pair = RevisablePair::new();
    let directory = pair.dir();
    let tantivy = directory.join("tantivy");
    let metadata_path = directory.join("metadata.db");
    let metadata = Connection::open(&metadata_path).unwrap();
    metadata
        .execute_batch(
            "CREATE TRIGGER fail_terminal_revision BEFORE UPDATE OF mutation_pending
         ON generation_state WHEN NEW.mutation_pending=0
         BEGIN SELECT RAISE(ABORT, 'interrupted revision publication'); END;",
        )
        .unwrap();
    drop(metadata);

    pair.write_source("# Plum\n\nPlumincompletebatch\n");
    let error = pair.apply().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interrupted revision publication"),
        "{error}"
    );
    assert_eq!(
        ReadIndex::open(&tantivy)
            .unwrap()
            .search("Plumincompletebatch", 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        pair.search("Applemigration")
            .unwrap_err()
            .to_string()
            .contains("RecoveryRequired")
    );
    assert_eq!(
        inspect_project_index(
            pair.env.cache.path(),
            pair.project.id,
            &pair.policy,
            limits()
        )
        .unwrap(),
        ProjectIndexObservation::Unavailable(registry_inspect::UnavailableReason::RecoveryRequired),
    );

    let unknown = tantivy.join("12345678901234567890123456789012.idx");
    fs::write(&unknown, "not owned by the index").unwrap();
    let refusal = pair
        .rebuild(|| Err(admission("rebuild cancelled")))
        .unwrap_err();
    assert!(
        refusal.to_string().contains("rebuild cancelled"),
        "{refusal}"
    );
    assert_eq!(pair.env.catalog_state().0, pair.generation);
    assert!(pair.search("Plumincompletebatch").is_err());

    assert_eq!(pair.rebuild(|| Ok(())).unwrap().docs, 1);
    let (repaired, hits) = pair.search("Plumincompletebatch").unwrap();
    assert_ne!(repaired.generation_id(), Some(pair.generation));
    assert_eq!(hits, 1);

    let reclaim = || {
        reclaim_retired_lexical_generation(
            pair.env.cache.path(),
            pair.request,
            pair.generation,
            &pair.policy,
            limits(),
        )
    };
    assert!(reclaim().is_err());
    assert!(unknown.is_file());
    fs::remove_file(unknown).unwrap();
    let db = Connection::open(pair.env.cache.path().join(DB_FILE)).unwrap();
    db.execute(
        "UPDATE project_generations SET phase='reclaiming' WHERE id=?1",
        [pair.generation],
    )
    .unwrap();
    drop(db);
    fs::remove_file(metadata_path).unwrap();
    reclaim().unwrap();
    assert!(!directory.exists());
    assert_eq!(pair.search("Plumincompletebatch").unwrap().0, repaired);
}

#[test]
#[cfg(unix)]
fn explicit_rebuild_recovers_local_rollback_journals() {
    const CHILD_METADATA: &str = "DOCSBASE_TEST_ABANDON_LOCAL_METADATA";
    const CHILD_SPILL: &str = "DOCSBASE_TEST_ABANDON_LOCAL_SPILL";
    if let Some(path) = std::env::var_os(CHILD_METADATA) {
        let db = Connection::open(path).unwrap();
        db.pragma_update(None, "cache_spill", std::env::var(CHILD_SPILL).unwrap())
            .unwrap();
        db.execute_batch(
            "PRAGMA synchronous=EXTRA;
             PRAGMA cache_size=1;
             BEGIN IMMEDIATE;
             UPDATE generation_state SET content_revision=content_revision+99;
             UPDATE chunks SET text=hex(zeroblob(65536));",
        )
        .unwrap();
        std::process::exit(0);
    }
    for (spill, header) in [
        ("1", [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]),
        ("0", [0; 8]),
    ] {
        check_local_rollback_journal(CHILD_METADATA, CHILD_SPILL, spill, header);
    }
}

#[cfg(unix)]
fn check_local_rollback_journal(metadata_env: &str, spill_env: &str, spill: &str, header: [u8; 8]) {
    let env = Env::new();
    let request = env.request();
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let active = publish_lexical_generation(
        env.cache.path(),
        request,
        env.build(request).unwrap(),
        &settings,
        limits(),
        &config,
    )
    .unwrap();
    let dir = env.generation(active.generation_id.unwrap());
    let metadata = dir.join("metadata.db");
    let db = Connection::open(&metadata).unwrap();
    db.execute("UPDATE generation_state SET mutation_pending=1", [])
        .unwrap();
    drop(db);

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::generation_build::tests::explicit_rebuild_recovers_local_rollback_journals",
        ])
        .env(metadata_env, &metadata)
        .env(spill_env, spill)
        .status()
        .unwrap();
    assert!(child.success());
    let journal = dir.join("metadata.db-journal");
    let bytes = fs::read(&journal).unwrap();
    assert!(
        bytes.len() > 512 && bytes[..8] == header,
        "spill={spill}, header={:?}",
        &bytes[..8]
    );
    let cache = env.cache.path().canonicalize().unwrap();
    assert!(
        observe_index_base(
            &cache,
            active.project_id,
            request.epoch,
            &settings,
            limits()
        )
        .is_err()
    );
    assert!(
        journal.is_file(),
        "a normal observer changed the SQLite journal"
    );

    let (base, interrupted) = observe_replacement_base(
        &cache,
        active.project_id,
        request.epoch,
        &settings,
        limits(),
    )
    .unwrap();
    assert_eq!(base, Some(active));
    assert!(interrupted.is_some());
    assert!(!journal.exists());
    let db = Connection::open(&metadata).unwrap();
    let text: String = db
        .query_row("SELECT text FROM chunks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(text.contains("Applemigration"));
    assert!(text.len() < 128);
}

#[test]
#[cfg(unix)]
fn an_unindexed_vanished_file_does_not_drop_an_unrelated_edit() {
    let pair = RevisablePair::new();
    pair.write_source("addedowl");
    let input = job::JobInput::Incremental(
        vec![
            pair.env.source.path().join("guide.md"),
            pair.env.source.path().join("vanished.md"),
        ]
        .into(),
    );
    let stats = crate::store::project_index::apply_lexical_batch(
        &pair.registry,
        &pair.project,
        &pair.config,
        &input,
        None,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.docs, 1);
    assert_eq!(pair.search("addedowl").unwrap().1, 1);
}

#[test]
#[cfg(unix)]
fn retired_local_pair_waits_for_readers_and_preserves_unowned_files() {
    let env = Env::new();
    let settings = settings::standalone().unwrap();
    let (previous, next) = retire_local_pair(&env);
    let previous_request = env.request();
    let next_request = LexicalBuildRequest {
        base: Some(previous),
        ..previous_request
    };
    let old_id = previous.generation_id.unwrap();
    let old = env.generation(old_id);
    let next_id = next.generation_id.unwrap();
    let reclaim = |request, id| {
        reclaim_retired_lexical_generation(env.cache.path(), request, id, &settings, limits())
    };
    assert!(reclaim(next_request, next_id).is_err());
    let other_source = tempfile::tempdir().unwrap();
    let other_project = env.register_second_project(other_source.path());
    fs::create_dir(env.cache.path().join("projects/2")).unwrap();
    let error = reclaim(other_project, old_id).unwrap_err();
    assert!(error.to_string().contains("different project"), "{error}");
    let mut wrong_base = previous_request;
    wrong_base.base.as_mut().unwrap().content_revision += 1;
    assert!(reclaim(wrong_base, old_id).is_err());
    let wrong_reservation = LexicalBuildRequest {
        reserved_disk_bytes: previous_request.reserved_disk_bytes + 1,
        ..previous_request
    };
    assert!(reclaim(wrong_reservation, old_id).is_err());
    let lock = RwLock::new(open_coordination_file(&old.join(".read.lock"), false).unwrap());
    let lease = lock.try_read().unwrap();
    assert!(reclaim(previous_request, old_id).is_err());
    assert_eq!(env.catalog_state(), (next_id, Some("retired".into())));
    drop(lease);
    drop(lock);

    let seal = old.join("seal.json");
    let stored = fs::read(&seal).unwrap();
    fs::write(&seal, "foreign seal").unwrap();
    assert!(reclaim(previous_request, old_id).is_err());
    assert!(old.join("metadata.db").exists());
    assert_eq!(env.catalog_state(), (next_id, Some("retired".into())));
    fs::write(&seal, &stored).unwrap();

    let foreign = old.join("tantivy/00000000000000000000000000000000.idx");
    fs::write(&foreign, "preserve me").unwrap();
    assert!(reclaim(previous_request, old_id).is_err());
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "preserve me");
    assert_eq!(env.catalog_state(), (next_id, Some("retired".into())));
    fs::remove_file(foreign).unwrap();

    let registry = env.cache.path().join(DB_FILE);
    Connection::open(&registry)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_release BEFORE DELETE ON project_generations
             BEGIN SELECT RAISE(ABORT, 'release interrupted'); END;",
        )
        .unwrap();
    let error = reclaim(previous_request, old_id).unwrap_err();
    assert!(error.to_string().contains("release interrupted"), "{error}");
    assert!(!old.exists());
    assert_eq!(env.catalog_state(), (next_id, Some("reclaiming".into())));
    Connection::open(registry)
        .unwrap()
        .execute_batch("DROP TRIGGER reject_release")
        .unwrap();
    reclaim(previous_request, old_id).unwrap();
    reclaim(previous_request, old_id).unwrap();
    assert_eq!(env.catalog_state(), (next_id, None));
    let current = ReadIndex::open(&env.generation(next_id).join("tantivy")).unwrap();
    assert_eq!(current.search("Cherryupdate", 2).unwrap().len(), 1);
}

#[test]
#[cfg(unix)]
fn retired_reclamation_requires_the_seal_while_payload_remains() {
    let env = Env::new();
    let settings = settings::standalone().unwrap();
    let (previous, next) = retire_local_pair(&env);
    let old_id = previous.generation_id.unwrap();
    let old = env.generation(old_id);
    let request = env.request();
    let reclaim = |request| {
        reclaim_retired_lexical_generation(env.cache.path(), request, old_id, &settings, limits())
    };
    let db = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
    db.execute(
        "UPDATE project_generations SET phase='reclaiming' WHERE id=?1",
        [old_id],
    )
    .unwrap();
    drop(db);
    let seal = old.join("seal.json");
    let stored = fs::read(&seal).unwrap();
    fs::remove_file(&seal).unwrap();
    assert!(reclaim(request).is_err());
    assert!(old.join("metadata.db").exists());
    fs::write(seal, stored).unwrap();
    fs::remove_file(old.join("metadata.db")).unwrap();
    let wrong_reservation = LexicalBuildRequest {
        reserved_disk_bytes: request.reserved_disk_bytes + 1,
        ..request
    };
    assert!(reclaim(wrong_reservation).is_err());
    reclaim(request).unwrap();
    assert!(!old.exists());
    assert_eq!(env.catalog_state(), (next.generation_id.unwrap(), None));
}

#[test]
#[cfg(unix)]
fn publication_resumes_a_durable_seal_left_before_the_catalog_commit() {
    let env = Env::new();
    let request = env.request();
    let staged = env.build(request).unwrap();
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let project = super::probe_project(
        env.cache.path(),
        request,
        &settings,
        limits(),
        CatalogAccess::Stopped,
    )
    .unwrap();
    let dir = env.generation(staged.generation_id);
    let written = super::publication::verify_and_seal(
        &dir,
        request,
        staged,
        &project,
        &settings,
        limits(),
        &config,
    )
    .unwrap();
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();

    let active = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings,
        limits(),
        &config,
    )
    .unwrap();
    assert_eq!(active.generation_id, Some(staged.generation_id));
    assert_eq!(
        blake3::hash(&fs::read(dir.join("seal.json")).unwrap()),
        written
    );
    assert_eq!(
        env.catalog_state(),
        (staged.generation_id, Some("active".into()))
    );
}

#[test]
#[cfg(unix)]
fn publication_retry_recovers_an_abandoned_registry_wal() {
    const CACHE: &str = "DOCSBASE_GENERATION_PUBLICATION_CRASH_CACHE";
    if let Some(cache) = std::env::var_os(CACHE) {
        let registry = Connection::open(Path::new(&cache).join(DB_FILE)).unwrap();
        registry
            .execute_batch("PRAGMA synchronous=FULL; PRAGMA user_version=5;")
            .unwrap();
        // Persist a WAL frame and interrupt before SQLite can close the file.
        std::process::exit(0);
    }

    let env = Env::new();
    let request = env.request();
    let staged = env.build(request).unwrap();
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let publish = || {
        publish_lexical_generation(
            env.cache.path(),
            request,
            staged,
            &settings,
            limits(),
            &config,
        )
    };
    let active = publish().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::generation_build::tests::publication_retry_recovers_an_abandoned_registry_wal",
        ])
        .env(CACHE, env.cache.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(env.cache.path().join(format!("{DB_FILE}-wal")).is_file());
    assert_eq!(publish().unwrap(), active);
    assert!(!env.cache.path().join(format!("{DB_FILE}-wal")).exists());
}

#[test]
#[cfg(unix)]
fn publication_rolls_back_phases_if_the_head_cannot_change() {
    let env = Env::new();
    let request = env.request();
    let staged = env.build(request).unwrap();
    let registry_path = env.cache.path().join(DB_FILE);
    let conn = Connection::open(&registry_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_publication BEFORE UPDATE ON project_generation_head
         BEGIN SELECT RAISE(ABORT, 'test refused head update'); END;",
    )
    .unwrap();
    drop(conn);

    let settings = settings::standalone().unwrap();
    let config = Config::default();
    let publish = || {
        publish_lexical_generation(
            env.cache.path(),
            request,
            staged,
            &settings,
            limits(),
            &config,
        )
    };
    assert!(publish().is_err());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
    assert!(
        env.generation(staged.generation_id)
            .join("seal.json")
            .is_file()
    );

    let conn = Connection::open(&registry_path).unwrap();
    conn.execute_batch("DROP TRIGGER reject_publication")
        .unwrap();
    drop(conn);
    assert_eq!(publish().unwrap().generation_id, Some(staged.generation_id));
    let conn = Connection::open(&registry_path).unwrap();
    let previous: String = conn
        .query_row(
            "SELECT phase FROM project_generations WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(previous, "retired");
}

#[test]
#[cfg(unix)]
fn publication_refuses_a_source_edit_of_the_same_length() {
    let env = Env::new();
    let request = env.request();
    let staged = env.build(request).unwrap();
    fs::write(
        env.source.path().join("guide.md"),
        "# Peach\n\nPeachmigration\n",
    )
    .unwrap();
    let error = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings::standalone().unwrap(),
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("source content changed"),
        "{error}"
    );
    assert!(
        !env.generation(staged.generation_id)
            .join("seal.json")
            .exists()
    );
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn publication_does_not_match_two_source_paths_to_one_saved_row() {
    let env = Env::new();
    let root = env.source.path();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/a.md"), "# Shared file\n").unwrap();
    fs::write(root.join("retired.md"), "# Retired file\n").unwrap();
    let request = env.request();
    let staged = env.build(request).unwrap();
    fs::remove_file(root.join("retired.md")).unwrap();
    fs::write(root.join(r"sub\a.md"), "# Shared file\n").unwrap();

    let error = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings::standalone().unwrap(),
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("source path changed"), "{error}");
    assert!(
        !env.generation(staged.generation_id)
            .join("seal.json")
            .exists()
    );
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn publication_refuses_a_tantivy_stamp_over_the_observer_limit() {
    use crate::index::tantivy_index::TOKENIZER_VERSION_FILE;
    use crate::limits::MAX_STATE_BYTES;

    let env = Env::new();
    let request = env.request();
    let mut staged = env.build(request).unwrap();
    let stamp = env
        .generation(staged.generation_id)
        .join("tantivy")
        .join(TOKENIZER_VERSION_FILE);
    let mut valid = fs::read(&stamp).unwrap();
    let oversized = usize::try_from(MAX_STATE_BYTES).unwrap() + 1;
    staged.bytes += (oversized - valid.len()) as u64;
    valid.resize(oversized, b' ');
    fs::write(&stamp, valid).unwrap();

    let error = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings::standalone().unwrap(),
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("LimitExceeded"), "{error}");
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn publication_requires_a_directory_for_an_empty_source_survey() {
    let env = Env::new();
    let root = env.source.path();
    fs::remove_file(root.join("guide.md")).unwrap();
    let request = env.request();
    let staged = env.build(request).unwrap();
    fs::remove_dir(root).unwrap();
    fs::write(root, b"not a project directory").unwrap();

    let error = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings::standalone().unwrap(),
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("source root changed"), "{error}");
    fs::remove_file(root).unwrap();
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn publication_rechecks_a_local_base_and_rejects_a_stale_revision() {
    let env = Env::new();
    let initial = env.request();
    let configured = settings::standalone().unwrap();
    let active = publish_lexical_generation(
        env.cache.path(),
        initial,
        env.build(initial).unwrap(),
        &configured,
        limits(),
        &Config::default(),
    )
    .unwrap();
    fs::write(
        env.source.path().join("guide.md"),
        "# Peach\n\nPeachmigration\n",
    )
    .unwrap();
    let next = LexicalBuildRequest {
        base: Some(active),
        plan_digest: [6; 32],
        ..initial
    };
    let staged = env.build(next).unwrap();
    let directory = env.generation(active.generation_id.unwrap());
    let tantivy = directory.join("tantivy");
    let registry = Db::open_readonly_for_search(env.cache.path(), &configured).unwrap();
    let project = registry::project_by_id(&registry, active.project_id)
        .unwrap()
        .unwrap();
    drop(registry);
    let mut metadata = Db {
        conn: Connection::open(directory.join("metadata.db")).unwrap(),
        cache_root: directory.clone(),
        settings: configured.clone(),
        generation_catalog: false,
    };
    let config = Config::default();
    let mut writer =
        IndexHandle::open_or_create_with_settings(&tantivy, config.index_writer).unwrap();
    job::run_incremental_with(
        &mut metadata,
        &mut writer,
        &project,
        &[env.source.path().join("guide.md")],
        &config,
    )
    .unwrap();
    writer.finish().unwrap();
    let manifest = blake3::hash(&fs::read(tantivy.join("meta.json")).unwrap());
    metadata
        .connection()
        .execute(
            "UPDATE generation_state SET content_revision = content_revision + 1, tantivy_manifest = ?1",
            [manifest.as_bytes().as_slice()],
        )
        .unwrap();
    drop(metadata);
    let error = publish_lexical_generation(
        env.cache.path(),
        next,
        staged,
        &configured,
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("generation base changed"),
        "{error}"
    );
    assert!(
        !env.generation(staged.generation_id)
            .join("seal.json")
            .exists()
    );
    assert_eq!(
        env.catalog_state(),
        (active.generation_id.unwrap(), Some("active".into()))
    );
}

#[test]
#[cfg(windows)]
fn publication_without_a_directory_barrier_keeps_the_original_head() {
    let env = Env::new();
    let request = env.request();
    let staged = env.build(request).unwrap();
    let error = publish_lexical_generation(
        env.cache.path(),
        request,
        staged,
        &settings::standalone().unwrap(),
        limits(),
        &Config::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("directory durability barrier"),
        "{error}"
    );
    assert!(
        !env.generation(staged.generation_id)
            .join("seal.json")
            .exists()
    );
    env.assert_legacy_search();
}

#[test]
fn completed_pair_rejects_the_wrong_chunk_ids_even_at_equal_counts() {
    let env = Env::new();
    let prepared = env.build(env.request()).unwrap();
    let root = env.generation(prepared.generation_id);
    let db = Connection::open(root.join("metadata.db")).unwrap();
    let index_dir = root.join("tantivy");
    verify_chunk_pair(&db, prepared.project_id, &index_dir, limits()).unwrap();
    let tight_limit = InspectionLimits {
        max_metadata_bytes: size_of::<u64>(),
        ..limits()
    };
    let error = verify_chunk_pair(&db, prepared.project_id, &index_dir, tight_limit).unwrap_err();
    assert!(error.to_string().contains("metadata budget"), "{error}");

    let changed = db.execute("UPDATE chunks SET seq = seq + 1", []).unwrap();
    assert_eq!(changed, 1);
    let error = verify_chunk_pair(&db, prepared.project_id, &index_dir, limits()).unwrap_err();
    assert!(error.to_string().contains("chunk IDs"), "{error}");
    env.assert_legacy_search();
}

#[test]
fn completed_pair_requires_positions_for_phrase_search() {
    let env = Env::new();
    let prepared = env.build(env.request()).unwrap();
    let root = env.generation(prepared.generation_id);
    let db = Connection::open(root.join("metadata.db")).unwrap();
    let index_dir = root.join("tantivy");
    let positions = fs::read_dir(&index_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "pos"))
        .expect("generated Tantivy positions");
    fs::remove_file(positions).unwrap();

    assert!(verify_chunk_pair(&db, prepared.project_id, &index_dir, limits()).is_err());
    env.assert_legacy_search();
}

#[test]
fn an_incomplete_source_survey_leaves_a_recoverable_building_attempt() {
    let env = Env::new();
    fs::write(env.source.path().join("broken.md"), [255]).unwrap();
    assert!(env.build(env.request()).is_err());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    let generation = env.generation(2);
    assert!(generation.join(".build-owner").is_file());
    assert!(!generation.join("seal.json").exists());
    assert!(
        env.build(env.request()).is_err(),
        "another build cannot replace the partial generation"
    );
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn failed_generation_can_be_discarded_and_retried_without_reusing_its_path() {
    let env = Env::new();
    let source = env.source.path().join("broken.md");
    fs::write(&source, [255]).unwrap();
    let request = env.request();
    assert!(env.build(request).is_err());
    assert!(env.generation(2).is_dir());
    // The real Tantivy writer leaves a sibling marker on a failed commit.
    let marker = env
        .generation(2)
        .join(crate::index::tantivy_index::REBUILD_MARKER);
    fs::write(marker, b"rebuild required\n").unwrap();

    let discard = || {
        discard_lexical_generation(
            env.cache.path(),
            request,
            2,
            &settings::standalone().unwrap(),
            limits(),
        )
    };
    discard().unwrap();
    discard().unwrap();
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!env.generation(2).exists());

    fs::remove_file(source).unwrap();
    let rebuilt = env.build(request).unwrap();
    assert!(rebuilt.generation_id > 2);
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn discard_refuses_a_busy_reader_and_an_unknown_entry_before_removal() {
    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let root = env.generation(prepared.generation_id);
    let discard = || {
        discard_lexical_generation(
            env.cache.path(),
            request,
            prepared.generation_id,
            &settings::standalone().unwrap(),
            limits(),
        )
    };
    let read_lease = RwLock::new(open_coordination_file(&root.join(".read.lock"), false).unwrap());
    let held = read_lease.try_read().unwrap();
    assert!(discard().is_err());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    drop(held);

    fs::write(root.join("tantivy/notes.txt"), "preserve me").unwrap();
    let error = discard().unwrap_err();
    assert!(error.to_string().contains("unrecognized"), "{error}");
    assert_eq!(
        fs::read_to_string(root.join("tantivy/notes.txt")).unwrap(),
        "preserve me"
    );
    assert!(root.join("metadata.db").is_file());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn discard_resumes_an_empty_directory_after_ownership_marker_removal() {
    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let root = env.generation(prepared.generation_id);
    let registry = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
    registry
        .execute(
            "UPDATE project_generations SET phase = 'reclaiming' WHERE id = ?1",
            [prepared.generation_id],
        )
        .unwrap();
    drop(registry);

    assert!(
        env.build(request).is_err(),
        "the prior discard is still charged"
    );
    // Exact filesystem boundary after the payload and marker are removed.
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    discard_lexical_generation(
        env.cache.path(),
        request,
        prepared.generation_id,
        &settings::standalone().unwrap(),
        limits(),
    )
    .unwrap();
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!root.exists());
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn discard_removes_partial_parent_intent_before_releasing_its_reservation() {
    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let root = env.generation(prepared.generation_id);
    let generations = root.parent().unwrap();
    let intent = claim_path(generations, prepared.generation_id);
    let digest = ownership_digest(request, prepared.generation_id);

    fs::remove_dir_all(&root).unwrap();
    fs::write(&intent, &digest.as_bytes()[..7]).unwrap();
    let settings = settings::standalone().unwrap();
    discard_lexical_generation(
        env.cache.path(),
        request,
        prepared.generation_id,
        &settings,
        limits(),
    )
    .unwrap();
    assert!(
        !intent.exists(),
        "the leaf claim must clear before releasing SQL"
    );
    assert_eq!(env.catalog_state(), (1, None));
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn discard_recovers_interruption_while_writing_the_inner_owner_record() {
    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let root = env.generation(prepared.generation_id);
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    let marker = root.join(".build-owner");
    let digest = ownership_digest(request, prepared.generation_id);
    fs::write(&marker, &digest.as_bytes()[..7]).unwrap();

    // This is the visible boundary after the parent claim and mkdir, but
    // before the inner owner and its directory entry have been synced.
    let settings = settings::standalone().unwrap();
    discard_lexical_generation(
        env.cache.path(),
        request,
        prepared.generation_id,
        &settings,
        limits(),
    )
    .unwrap();
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!root.exists());
    assert!(env.build(request).unwrap().generation_id > prepared.generation_id);
    env.assert_legacy_search();
}

#[test]
#[cfg(unix)]
fn discarded_build_can_be_located_and_recovered_with_abandoned_registry_wal() {
    const CRASH_CACHE: &str = "DOCSBASE_GENERATION_RECLAIM_CRASH_CACHE";
    if let Some(cache) = std::env::var_os(CRASH_CACHE) {
        let registry = Connection::open(Path::new(&cache).join(DB_FILE)).unwrap();
        assert_eq!(
            registry
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        registry
            .execute_batch(
                "PRAGMA synchronous=FULL;
             BEGIN IMMEDIATE;
             UPDATE project_generations SET phase = 'reclaiming' WHERE phase = 'building';
             COMMIT;",
            )
            .unwrap();
        // Skip SQLite close and its cleanup as when the process is interrupted.
        std::process::exit(0);
    }

    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "store::generation_build::tests::discarded_build_can_be_located_and_recovered_with_abandoned_registry_wal"])
        .env(CRASH_CACHE, env.cache.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(env.cache.path().join(format!("{DB_FILE}-wal")).is_file());

    let resolved = settings::standalone().unwrap();
    let found =
        find_lexical_build_to_discard(env.cache.path(), request, &resolved, limits()).unwrap();
    assert_eq!(found, Some(prepared.generation_id));
    discard_lexical_generation(
        env.cache.path(),
        request,
        found.unwrap(),
        &resolved,
        limits(),
    )
    .unwrap();
    assert_eq!(
        find_lexical_build_to_discard(env.cache.path(), request, &resolved, limits()).unwrap(),
        None
    );
    env.assert_legacy_search();
}

#[test]
#[cfg(windows)]
fn discard_without_a_qualified_directory_barrier_preserves_the_reservation() {
    let env = Env::new();
    let request = env.request();
    let prepared = env.build(request).unwrap();
    let root = env.generation(prepared.generation_id);

    let error = discard_lexical_generation(
        env.cache.path(),
        request,
        prepared.generation_id,
        &settings::standalone().unwrap(),
        limits(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("directory durability barrier"),
        "{error}"
    );
    assert!(root.join(".build-owner").is_file());
    assert!(root.join("metadata.db").is_file());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();

    // Windows cannot publish a local pair. Model a catalog moved from a
    // supported installation to exercise the same durability gate on retire.
    Connection::open(env.cache.path().join(DB_FILE))
        .unwrap()
        .execute(
            "UPDATE project_generations SET phase='retired', seal_digest=?1 WHERE id=?2",
            params![[1_u8; 32].as_slice(), prepared.generation_id],
        )
        .unwrap();
    let error = reclaim_retired_lexical_generation(
        env.cache.path(),
        request,
        prepared.generation_id,
        &settings::standalone().unwrap(),
        limits(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("directory durability barrier"),
        "{error}"
    );
    assert!(root.join(".build-owner").is_file());
    assert!(root.join("metadata.db").is_file());
    assert_eq!(env.catalog_state(), (1, Some("retired".into())));
}

#[test]
fn stale_base_and_a_contending_builder_cannot_reserve_a_generation() {
    let env = Env::new();
    let mut request = env.request();
    request.base.as_mut().unwrap().content_revision += 1;
    assert!(env.build(request).is_err());
    let lock_path = env.cache.path().join("projects/1/.prepare.lock");
    let mut lock =
        fd_lock::RwLock::new(crate::platform::fs::open_private_lock(&lock_path).unwrap());
    let _guard = lock.try_write().unwrap();
    assert!(env.build(env.request()).is_err());
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!env.generation(2).exists());
}

#[test]
fn disk_reservation_refuses_a_request_smaller_than_the_ownership_records() {
    let env = Env::new();
    let mut request = env.request();
    request.reserved_disk_bytes = 1;
    assert!(env.build(request).is_err());
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!env.generation(2).exists());
    env.assert_legacy_search();
}

#[test]
fn metadata_growth_stops_at_the_staged_disk_reservation() {
    let env = Env::new();
    let request = LexicalBuildRequest {
        reserved_disk_bytes: 4096 + 2 * blake3::OUT_LEN as u64,
        ..env.request()
    };
    let error = env.build(request).unwrap_err();
    let cause =
        std::error::Error::source(&error).and_then(|cause| cause.downcast_ref::<rusqlite::Error>());
    assert!(
        matches!(cause, Some(rusqlite::Error::SqliteFailure(code, _))
            if code.code == rusqlite::ErrorCode::DiskFull),
        "{error}"
    );
    let stage = env.generation(2);
    let metadata = fs::metadata(stage.join("metadata.db")).unwrap();
    assert!(
        metadata.len() <= 4096,
        "metadata used {} bytes",
        metadata.len()
    );
    assert!(!stage.join("tantivy").exists());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
    #[cfg(unix)]
    discard_failed_quota_and_retry(&env, request);
}

#[test]
fn tantivy_growth_stops_at_the_staged_disk_reservation() {
    let reference = Env::new();
    let staged = reference.build(reference.request()).unwrap();
    let metadata = fs::metadata(
        reference
            .generation(staged.generation_id)
            .join("metadata.db"),
    )
    .unwrap()
    .len();
    let metadata_and_owners = metadata + 2 * blake3::OUT_LEN as u64;
    let env = Env::new();
    let request = LexicalBuildRequest {
        reserved_disk_bytes: metadata_and_owners + (staged.bytes - metadata_and_owners) / 2,
        ..env.request()
    };
    let error = env.build(request).unwrap_err();
    assert!(
        matches!(&error, Error::Admission { message } if message.contains("Tantivy write exceeds")),
        "{error}"
    );
    let stage = env.generation(2);
    let used = size_of_pair(&stage).unwrap() + blake3::OUT_LEN as u64;
    assert!(
        used <= request.reserved_disk_bytes,
        "staging used {used} bytes with a {}-byte reservation",
        request.reserved_disk_bytes
    );
    assert!(!stage.join("seal.json").exists());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();
    #[cfg(unix)]
    discard_failed_quota_and_retry(&env, request);
}

#[cfg(unix)]
fn discard_failed_quota_and_retry(env: &Env, request: LexicalBuildRequest) {
    // A writer may release the caller before its last directory handle drops.
    let read_lock = env.generation(2).join(".read.lock");
    let mut lock = RwLock::new(open_coordination_file(&read_lock, false).unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match lock.try_write() {
            Ok(_) => break,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => panic!("staged writer did not release its lease: {error}"),
        }
    }
    drop(lock);
    discard_lexical_generation(
        env.cache.path(),
        request,
        2,
        &settings::standalone().unwrap(),
        limits(),
    )
    .unwrap();
    assert!(!env.generation(2).exists());
    let retried = env.build(env.request()).unwrap();
    assert!(retried.generation_id > 2);
    let reader = ReadIndex::open(&env.generation(retried.generation_id).join("tantivy")).unwrap();
    assert_eq!(reader.search("Applemigration", 5).unwrap().len(), 1);
}

#[test]
fn installation_reserves_space_for_other_projects_until_discard_finishes() {
    let env = Env::new();
    let first_request = env.request();
    let built = env.build(first_request).unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("guide.md"), "# Second project\n").unwrap();
    let registry = env.cache.path().join(DB_FILE);
    let second_request = env.register_second_project(source.path());
    let settings = settings::standalone().unwrap();
    let build_second = || {
        prepare_lexical_generation_with_space(
            env.cache.path(),
            second_request,
            &settings,
            limits(),
            &Config::default(),
            |path| {
                let expected = env
                    .cache
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join("projects/2/generations");
                assert_eq!(path, expected);
                assert_reservation_transaction(&registry);
                Ok(first_request.reserved_disk_bytes + second_request.reserved_disk_bytes - 1)
            },
        )
    };
    let assert_refused = || {
        let error = build_second().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("free space for all generation reservations"),
            "{error}"
        );
        let db = Connection::open(&registry).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM project_generations WHERE project_id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    };
    assert_refused();
    for (phase, digest) in [("sealed", Some([1_u8; 32])), ("reclaiming", None)] {
        Connection::open(&registry)
            .unwrap()
            .execute(
                "UPDATE project_generations SET phase = ?1, seal_digest = ?2 WHERE id = ?3",
                params![
                    phase,
                    digest.as_ref().map(<[u8; 32]>::as_slice),
                    built.generation_id
                ],
            )
            .unwrap();
        assert_refused();
    }
    env.assert_legacy_search();

    #[cfg(unix)]
    {
        discard_lexical_generation(
            env.cache.path(),
            first_request,
            built.generation_id,
            &settings,
            limits(),
        )
        .unwrap();
        let staged = build_second().unwrap();
        assert_eq!(staged.project_id, 2);
        let config = Config::default();
        let active = publish_lexical_generation(
            env.cache.path(),
            second_request,
            staged,
            &settings,
            limits(),
            &config,
        )
        .unwrap();
        assert_eq!(active.generation_id, Some(staged.generation_id));
        let next = prepare_lexical_generation_with_space(
            env.cache.path(),
            first_request,
            &settings,
            limits(),
            &config,
            |_| Ok(first_request.reserved_disk_bytes),
        )
        .unwrap();
        assert_eq!(
            next.project_id, 1,
            "available disk already accounts for the active pair"
        );
    }
}

fn assert_reservation_transaction(registry: &Path) {
    let contender = Connection::open(registry).unwrap();
    contender.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(matches!(
        contender.execute_batch("BEGIN IMMEDIATE"),
        Err(rusqlite::Error::SqliteFailure(code, _))
            if code.code == rusqlite::ErrorCode::DatabaseBusy
    ));
}

#[test]
fn newly_registered_project_builds_its_first_pair_without_an_active_head() {
    let env = Env::new();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("start.md"), "# Firstgarden\n").unwrap();
    let result = env
        .build(env.register_second_project(source.path()))
        .unwrap();
    assert_eq!(
        (
            result.project_id,
            result.generation_id,
            result.content_revision
        ),
        (2, 2, 1)
    );
    let generation = env.cache.path().join("projects/2/generations/2");
    let reader = ReadIndex::open(&generation.join("tantivy")).unwrap();
    assert_eq!(reader.search("Firstgarden", 5).unwrap().len(), 1);
    assert_eq!(
        inspect_project_index(
            env.cache.path(),
            2,
            &settings::standalone().unwrap(),
            limits()
        )
        .unwrap(),
        ProjectIndexObservation::Missing
    );
    env.assert_legacy_search();
}

#[test]
fn an_existing_numeric_directory_is_never_reused() {
    let env = Env::new();
    let existing = env.generation(2);
    fs::create_dir_all(&existing).unwrap();
    let sentinel = existing.join("foreign-file");
    fs::write(&sentinel, b"belongs to someone else").unwrap();
    assert!(env.build(env.request()).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"belongs to someone else");
    assert!(!existing.join("metadata.db").exists());
    assert!(!existing.join(".build-owner").exists());
    assert_eq!(env.catalog_state(), (1, None));
    assert!(env.build(env.request()).unwrap().generation_id > 2);
    env.assert_legacy_search();
}

#[test]
fn pending_registry_recovery_is_refused_before_creating_preparation_state() {
    let env = Env::new();
    let db_path = env.cache.path().join(DB_FILE);
    let registry = fs::read(&db_path).unwrap();
    let pending = env.cache.path().join("registry.db-journal");
    fs::write(&pending, b"evidence of an interrupted write").unwrap();
    assert!(env.build(env.request()).is_err());
    assert_eq!(
        fs::read(pending).unwrap(),
        b"evidence of an interrupted write"
    );
    assert_eq!(fs::read(db_path).unwrap(), registry);
    assert!(!env.cache.path().join("projects/1/.prepare.lock").exists());
    assert!(!env.generation(2).exists());
}

#[test]
fn a_registered_root_replaced_with_a_file_is_refused_before_reservation() {
    let env = Env::new();
    let moved = tempfile::tempdir().unwrap();
    let original = moved.path().join("original");
    fs::rename(env.source.path(), &original).unwrap();
    fs::write(env.source.path(), b"not a directory").unwrap();
    let result = env.build(env.request());
    fs::remove_file(env.source.path()).unwrap();
    fs::rename(original, env.source.path()).unwrap();

    assert!(result.is_err());
    assert_eq!(env.catalog_state(), (1, None));
    assert!(!env.generation(2).exists());
}

#[test]
fn an_active_index_writer_prevents_reservation() {
    let env = Env::new();
    let path = env.cache.path().join("projects/1/.writer.lock");
    let mut lock = fd_lock::RwLock::new(crate::platform::fs::open_private_lock(&path).unwrap());
    let _guard = lock.try_write().unwrap();
    let error = env.build(env.request()).unwrap_err();
    assert!(lifecycle::is_lease_conflict(&error), "{error}");
    assert_eq!(env.catalog_state(), (1, None));
}

#[cfg(unix)]
#[cfg(test)]
mod unix {
    use super::*;

    #[test]
    fn operational_registry_still_refuses_linked_database_and_sidecar_names() {
        let env = Env::new();
        let cache = env.cache.path();
        let policy = settings::standalone().unwrap();
        let registry = cache.join(DB_FILE);
        let sidecar = cache.join(format!("{DB_FILE}-wal"));
        std::os::unix::fs::symlink(&registry, &sidecar).unwrap();
        let error = open_index_registry(cache, &policy, env.request().epoch).unwrap_err();
        assert!(error.inner_message().contains("inspect registry sidecar"));
        fs::remove_file(sidecar).unwrap();

        let moved = cache.join("moved-registry.db");
        fs::rename(&registry, &moved).unwrap();
        std::os::unix::fs::symlink(moved, registry).unwrap();
        let error = open_index_registry(cache, &policy, env.request().epoch).unwrap_err();
        assert!(
            error
                .inner_message()
                .contains("inspect generation registry file")
        );
    }

    #[test]
    fn first_publication_does_not_create_a_dangling_writer_lock_target() {
        let env = Env::new();
        let source = tempfile::tempdir().unwrap();
        let registrar = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
        let project_id: i64 = registrar
            .query_row(
                "INSERT INTO projects (canonical_root, name, status, schema_version, created_at)
             VALUES (?1, 'other', 'not_indexed', ?2, 0) RETURNING id",
                params![
                    source.path().canonicalize().unwrap().to_str(),
                    crate::store::migrations::PROJECT_SCHEMA_VERSION
                ],
                |row| row.get(0),
            )
            .unwrap();
        drop(registrar);
        let request = LexicalBuildRequest {
            project_id,
            base: None,
            ..env.request()
        };
        let staged = env.build(request).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("unexpected-lock");
        let lease = lifecycle::lease_path(env.cache.path(), request.project_id);
        fs::remove_file(&lease).unwrap();
        std::os::unix::fs::symlink(&target, &lease).unwrap();

        publish_lexical_generation(
            env.cache.path(),
            request,
            staged,
            &settings::standalone().unwrap(),
            limits(),
            &Config::default(),
        )
        .unwrap_err();
        assert!(!target.exists());
        let registry = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
        let heads: i64 = registry
            .query_row(
                "SELECT count(*) FROM project_generation_head WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(heads, 0);
    }

    #[test]
    fn a_registered_root_link_to_a_different_directory_is_refused() {
        let env = Env::new();
        let moved = tempfile::tempdir().unwrap();
        let original = moved.path().join("original");
        fs::rename(env.source.path(), &original).unwrap();
        std::os::unix::fs::symlink(&original, env.source.path()).unwrap();
        let result = env.build(env.request());
        fs::remove_file(env.source.path()).unwrap();
        fs::rename(original, env.source.path()).unwrap();

        assert!(result.is_err());
        assert_eq!(env.catalog_state(), (1, None));
        assert!(!env.generation(2).exists());
    }
}

#[test]
#[cfg(unix)]
fn direct_index_refusal_at_the_commit_boundary_preserves_the_old_pair() {
    let env = Env::new();
    let request = env.request();
    let settings = settings::standalone().unwrap();
    let config = Config::default();
    std::fs::write(
        env.source.path().join("guide.md"),
        "# Peach\n\nPeachmigration\n",
    )
    .unwrap();
    let budget = crate::search::budget::PayloadBudget::new(config.search.clone()).unwrap();
    let operation = crate::search::operation::SearchOperation::new(
        budget.settings(),
        std::time::Instant::now(),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let account = budget.reserve(0).unwrap();
    let mut capture = job::CaptureBudget {
        account: &account,
        operation: &operation,
        observed: None,
    };
    let result = with_prepared_lexical_generation(
        env.cache.path(),
        OperationalBuild {
            request,
            interrupted: None,
        },
        &settings,
        limits(),
        &config,
        &mut capture,
        |pair| {
            pair.publish(|commit| {
                commit.commit(|| {
                    let contender = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
                    contender.busy_timeout(std::time::Duration::ZERO).unwrap();
                    let error = contender.execute_batch("BEGIN IMMEDIATE").unwrap_err();
                    assert!(matches!(error, rusqlite::Error::SqliteFailure(code, _)
                        if code.code == rusqlite::ErrorCode::DatabaseBusy));
                    Err(admission("late refusal"))
                })
            })
        },
    );
    assert!(result.unwrap_err().to_string().contains("late refusal"));
    assert!(env.generation(2).join("seal.json").exists());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    env.assert_legacy_search();

    let stats = with_prepared_lexical_generation(
        env.cache.path(),
        OperationalBuild {
            request,
            interrupted: None,
        },
        &settings,
        limits(),
        &config,
        &mut capture,
        |pair| pair.publish(|commit| commit.commit(|| Ok(()))),
    )
    .unwrap();
    assert_eq!(stats.docs, 1);
    assert_eq!(env.catalog_state(), (3, None));
}

#[test]
#[cfg(unix)]
fn index_cleanup_refuses_a_retired_catalog_base_that_disagrees_with_the_seal() {
    let env = Env::new();
    let request = env.request();
    let settings = settings::standalone().unwrap();
    let (previous, next) = retire_local_pair(&env);
    let generation = previous.generation_id.unwrap();
    let cache = env.cache.path().canonicalize().unwrap();
    let project = project_directory(&cache, request.project_id);
    let retired = env.generation(generation);
    let registry = Connection::open(cache.join(DB_FILE)).unwrap();
    registry
        .execute(
            "UPDATE project_generations SET base_revision=base_revision+1 WHERE id=?1",
            [generation],
        )
        .unwrap();
    drop(registry);

    // The catalog is still writable, but its recorded base no longer
    // agrees with the authenticated payload's seal.
    recovery::reclaim_index_pairs(&cache, &project, request, &settings, limits());
    assert!(retired.join("metadata.db").exists());
    assert_eq!(
        env.catalog_state(),
        (next.generation_id.unwrap(), Some("retired".into()))
    );

    let registry = Connection::open(cache.join(DB_FILE)).unwrap();
    registry
        .execute(
            "UPDATE project_generations SET base_revision=base_revision-1 WHERE id=?1",
            [generation],
        )
        .unwrap();
    drop(registry);
    recovery::reclaim_index_pairs(&cache, &project, request, &settings, limits());
    assert!(!retired.exists());
}

#[test]
#[cfg(unix)]
fn operational_base_applies_the_sqlite_value_limit_to_local_metadata() {
    let env = Env::new();
    let settings = settings::standalone().unwrap();
    let (_, active) = retire_local_pair(&env);
    let metadata = env
        .generation(active.generation_id.unwrap())
        .join("metadata.db");
    let connection = Connection::open(metadata).unwrap();
    connection
        .execute(
            "UPDATE projects SET index_outcome_json = index_outcome_json || ?1",
            [" ".repeat(32 * 1024)],
        )
        .unwrap();
    drop(connection);

    let observed = observe_index_base(
        &env.cache.path().canonicalize().unwrap(),
        active.project_id,
        active.epoch.unwrap(),
        &settings,
        InspectionLimits {
            max_sqlite_value_bytes: 16 * 1024,
            ..limits()
        },
    );
    assert!(
        observed.is_err(),
        "oversized local SQLite record was accepted"
    );
}
