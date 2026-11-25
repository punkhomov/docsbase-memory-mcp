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

    fn generation(&self, id: i64) -> std::path::PathBuf {
        self.cache
            .path()
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
fn disk_reservation_refuses_oversized_completed_files() {
    let env = Env::new();
    let mut request = env.request();
    request.reserved_disk_bytes = 1;
    assert!(env.build(request).is_err());
    assert_eq!(env.catalog_state(), (1, Some("building".into())));
    assert!(!env.generation(2).join("seal.json").exists());
    env.assert_legacy_search();
}

#[test]
fn newly_registered_project_builds_its_first_pair_without_an_active_head() {
    let env = Env::new();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("start.md"), "# Firstgarden\n").unwrap();
    let root = source.path().canonicalize().unwrap();
    let conn = Connection::open(env.cache.path().join(DB_FILE)).unwrap();
    conn.execute(
        "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
          VALUES (2, ?1, 'new', 'not_indexed', 3, 0)",
        [root.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    let result = env
        .build(LexicalBuildRequest {
            project_id: 2,
            base: None,
            ..env.request()
        })
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
