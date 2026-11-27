use super::*;
use crate::config::Config;
use crate::daemon::{registry::ensure_project, tools::run_project_index};
use crate::store::{DB_FILE, Db, repo, settings};
use std::{collections::BTreeMap, fs};

fn limits() -> InspectionLimits {
    InspectionLimits {
        max_projects: 8,
        max_metadata_bytes: 65_536,
        max_sqlite_value_bytes: 1_048_576,
        sqlite_cache_kib: 64,
    }
}

#[test]
fn live_ann_chunk_read_has_room_for_a_large_atomic_chunk() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, project_id INTEGER);
             CREATE TABLE chunks(doc_id INTEGER, seq INTEGER, text TEXT, UNIQUE(doc_id, seq));
             INSERT INTO docs VALUES(1, 7);",
        )
        .unwrap();
    let text = "a".repeat(1_100_000);
    connection
        .execute("INSERT INTO chunks VALUES(1, 0, ?1)", [&text])
        .unwrap();
    super::configure_connection(&connection, limits()).unwrap();
    let observe =
        || crate::hybrid::ann_store::catalog_chunk_set(&connection, 7, 2_000_000, || Ok(()));
    assert!(observe().is_err());
    super::allow_chunk_values(&connection).unwrap();
    assert_eq!(observe().unwrap().0, 1);
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            entries.extend(snapshot(&path));
            entries.insert(path, None);
        } else {
            entries.insert(path.clone(), Some(fs::read(path).unwrap()));
        }
    }
    entries
}

fn index(cache: &Path, root: &Path, text: &str) -> i64 {
    fs::write(root.join("guide.md"), text).unwrap();
    let mut db = Db::open(cache).unwrap();
    index_root(&mut db, cache, root)
}

fn index_root(db: &mut Db, cache: &Path, root: &Path) -> i64 {
    let project = ensure_project(db, root).unwrap();
    assert_eq!(project.id, 1);
    run_project_index(db, cache, &project, &Config::default()).unwrap();
    repo::index_generation(db.connection(), project.id).unwrap()
}

fn inspect(cache: &Path) -> ProjectIndexObservation {
    let before = snapshot(cache);
    let result =
        inspect_project_index(cache, 1, &settings::standalone().unwrap(), limits()).unwrap();
    assert_eq!(
        snapshot(cache),
        before,
        "an index observation must be read-only"
    );
    result
}

#[test]
fn v4_observation_uses_the_real_legacy_pair() {
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let revision = index(cache.path(), root.path(), "# Cherry\n\ncherry orchard\n");
    let ProjectIndexObservation::Present(binding) = inspect(cache.path()) else {
        panic!("existing v4 pair must be observable");
    };
    assert_eq!(binding.layout, ProjectIndexLayout::Legacy);
    assert_eq!(binding.content_revision, revision);
    assert_eq!(binding.generation_id, None);
    let mut db = Db::open(cache.path()).unwrap();
    assert_eq!(index_root(&mut db, cache.path(), root.path()), revision);
    drop(db);
    assert_eq!(
        inspect(cache.path()),
        ProjectIndexObservation::Present(binding)
    );
    fs::remove_file(cache.path().join("projects/1/tantivy/meta.json")).unwrap();
    assert!(matches!(
        inspect(cache.path()),
        ProjectIndexObservation::Unavailable(_)
    ));
}

#[test]
fn observation_accepts_a_healthy_index_with_no_segments() {
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    let revision = index_root(&mut db, cache.path(), root.path());
    drop(db);
    assert_eq!(
        inspect(cache.path()),
        ProjectIndexObservation::Present(ObservedProjectIndex {
            project_id: 1,
            epoch: None,
            generation_id: None,
            content_revision: revision,
            layout: ProjectIndexLayout::Legacy,
        })
    );
}

struct Pairs {
    cache: tempfile::TempDir,
    _source: tempfile::TempDir,
    revision: i64,
    local: PathBuf,
}

impl Pairs {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let revision = index(cache.path(), source.path(), "# Cherry\n\ncherry orchard\n");
        assert_eq!(
            index(other.path(), source.path(), "# Walnut\n\nwalnut forest\n"),
            revision
        );
        for writer_cache in [cache.path(), other.path()] {
            let lock = writer_cache
                .join("projects/1/tantivy")
                .join(&tantivy::directory::META_LOCK.filepath);
            if lock.exists() {
                fs::remove_file(lock).unwrap();
            }
        }
        let project = cache.path().join("projects/1");
        let local = project.join("generations/2");
        fs::create_dir_all(&local).unwrap();
        fs::write(project.join(".legacy-generation.lock"), []).unwrap();
        fs::write(local.join(".read.lock"), []).unwrap();
        fs::rename(
            other.path().join("projects/1/tantivy"),
            local.join("tantivy"),
        )
        .unwrap();
        fs::copy(other.path().join(DB_FILE), local.join("metadata.db")).unwrap();
        let mut metadata = Connection::open(local.join("metadata.db")).unwrap();
        // The fixture starts from a legacy WAL registry. Local generations
        // are published in rollback format.
        metadata
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        let tx = metadata.transaction().unwrap();
        tx.execute_batch(crate::store::project_index::LOCAL_SCHEMA_SQL)
            .unwrap();
        tx.execute("INSERT INTO generation_state VALUES (1,1,2,?1)", [revision])
            .unwrap();
        tx.pragma_update(None, "user_version", 1).unwrap();
        tx.commit().unwrap();
        drop(metadata);
        let seal = br#"{"project_id":1,"generation_id":2}"#;
        fs::write(local.join("seal.json"), seal).unwrap();
        let digest = blake3::hash(seal);
        let mut registry = Connection::open(cache.path().join(DB_FILE)).unwrap();
        registry.pragma_update(None, "foreign_keys", "ON").unwrap();
        let tx = registry.transaction().unwrap();
        tx.execute_batch(generation_catalog::SCHEMA_SQL).unwrap();
        tx.execute_batch(
            "INSERT INTO generation_store VALUES (1,1,zeroblob(16));
          INSERT INTO project_generation_head (project_id,active_id) VALUES (1,1);",
        )
        .unwrap();
        tx.execute(
            "INSERT INTO project_generations
          (id,project_id,layout,phase,base_revision,seal_digest,reserved_disk_bytes)
          VALUES (1,1,'legacy','active',?1,NULL,0),(2,1,'local','sealed',?1,?2,0)",
            rusqlite::params![revision, digest.as_bytes().as_slice()],
        )
        .unwrap();
        tx.pragma_update(None, "user_version", generation_catalog::SCHEMA_VERSION)
            .unwrap();
        tx.commit().unwrap();
        Self {
            cache,
            _source: source,
            revision,
            local,
        }
    }
    fn activate_local(&self) {
        let mut registry = Connection::open(self.cache.path().join(DB_FILE)).unwrap();
        registry.pragma_update(None, "foreign_keys", "ON").unwrap();
        let tx = registry.transaction().unwrap();
        tx.execute_batch(
            "UPDATE project_generations SET phase='retired' WHERE id=1;
            UPDATE project_generations SET phase='active' WHERE id=2;
            UPDATE project_generation_head SET active_id=2 WHERE project_id=1;",
        )
        .unwrap();
        tx.commit().unwrap();
    }
    fn observe(&self) -> ProjectIndexObservation {
        inspect(self.cache.path())
    }
    fn local_sql(&self, sql: &str) {
        Connection::open(self.local.join("metadata.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}

#[test]
fn observation_follows_the_head_and_reads_only_the_selected_files() {
    let pairs = Pairs::new();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Present(ObservedProjectIndex {
            project_id: 1,
            epoch: Some([0; 16]),
            generation_id: Some(1),
            content_revision: pairs.revision,
            layout: ProjectIndexLayout::Legacy,
        })
    );
    pairs.activate_local();
    let index = ObservedProjectIndex {
        project_id: 1,
        epoch: Some([0; 16]),
        generation_id: Some(2),
        content_revision: pairs.revision,
        layout: ProjectIndexLayout::Local,
    };
    assert_eq!(pairs.observe(), ProjectIndexObservation::Present(index));
    pairs.local_sql("UPDATE generation_state SET content_revision=content_revision+1");
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Present(ObservedProjectIndex {
            content_revision: pairs.revision + 1,
            ..index
        })
    );
    fs::remove_file(pairs.local.join("tantivy/meta.json")).unwrap();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::Io)
    );
}

#[test]
fn incomplete_or_busy_local_generation_never_falls_back_to_legacy() {
    let pairs = Pairs::new();
    pairs.activate_local();
    // Windows enforces the byte-range lock during file reads, including
    // reads made by the fixture. Snapshot only while the guards are dropped.
    let before = snapshot(pairs.cache.path());
    for path in [
        pairs.cache.path().join("projects/1/.writer.lock"),
        pairs.local.join(".read.lock"),
    ] {
        let mut lock = fd_lock::RwLock::new(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );
        let _guard = lock.try_write().unwrap();
        assert_eq!(
            inspect_project_index(
                pairs.cache.path(),
                1,
                &settings::standalone().unwrap(),
                limits(),
            )
            .unwrap(),
            ProjectIndexObservation::Unavailable(UnavailableReason::Busy),
            "{path:?}"
        );
    }
    assert_eq!(snapshot(pairs.cache.path()), before);
    let sidecar = pairs.local.join("metadata.db-wal");
    fs::write(&sidecar, b"unsettled metadata").unwrap();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::RecoveryRequired)
    );
    fs::remove_file(sidecar).unwrap();
    pairs.local_sql("UPDATE generation_state SET generation_id=3");
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::InvalidRegistry)
    );
    pairs.local_sql("UPDATE generation_state SET generation_id=2; UPDATE docs SET content_hash=''");
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::RecoveryRequired)
    );
    fs::write(pairs.local.join("seal.json"), b"changed seal").unwrap();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::InvalidRegistry)
    );
}

#[test]
fn observation_requires_positions_for_searchable_segments() {
    let pairs = Pairs::new();
    pairs.activate_local();
    let positions = fs::read_dir(pairs.local.join("tantivy"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "pos"))
        .expect("the indexed Markdown creates a segment with positions");
    fs::remove_file(positions).unwrap();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::Io)
    );
}

#[test]
fn observation_refuses_hardlinked_generation_files() {
    let pairs = Pairs::new();
    pairs.activate_local();
    for name in [
        ".read.lock",
        "seal.json",
        "metadata.db",
        "tantivy/meta.json",
    ] {
        let alias = pairs.local.join("hold");
        fs::hard_link(pairs.local.join(name), &alias).unwrap();
        assert_eq!(
            pairs.observe(),
            ProjectIndexObservation::Unavailable(UnavailableReason::Io),
            "{name}"
        );
        fs::remove_file(alias).unwrap();
    }
}

#[test]
fn observation_bounds_buffered_tantivy_metadata() {
    let pairs = Pairs::new();
    pairs.activate_local();
    let manifest = pairs.local.join("tantivy/meta.json");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes.resize(
        usize::try_from(crate::limits::MAX_STATE_BYTES).unwrap() + 1,
        b' ',
    );
    fs::write(&manifest, bytes).unwrap();
    assert_eq!(
        pairs.observe(),
        ProjectIndexObservation::Unavailable(UnavailableReason::LimitExceeded)
    );
    let relocated = pairs.local.join("manifest-hold");
    fs::rename(&manifest, &relocated).unwrap();
    fs::rename(relocated, manifest.with_file_name("META.JSON")).unwrap();
    if manifest.is_file() {
        assert_eq!(
            pairs.observe(),
            ProjectIndexObservation::Unavailable(UnavailableReason::LimitExceeded)
        );
    }
}

#[cfg(unix)]
#[cfg(test)]
mod unix {
    use super::*;

    #[test]
    fn observation_refuses_a_tantivy_manifest_from_another_directory() {
        let pairs = Pairs::new();
        pairs.activate_local();
        let manifest = pairs.local.join("tantivy/meta.json");
        let other = pairs.local.join("other-meta.json");
        fs::rename(&manifest, &other).unwrap();
        std::os::unix::fs::symlink(&other, &manifest).unwrap();
        assert_eq!(
            pairs.observe(),
            ProjectIndexObservation::Unavailable(UnavailableReason::Io)
        );
    }
}
