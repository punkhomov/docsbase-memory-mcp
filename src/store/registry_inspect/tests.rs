use super::*;
use crate::daemon::registry::ensure_project;
use crate::store::{DB_FILE, Db, settings};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

fn limits() -> InspectionLimits {
    InspectionLimits {
        max_projects: 8,
        max_metadata_bytes: 65_536,
        max_sqlite_value_bytes: 1_048_576,
        sqlite_cache_kib: 64,
    }
}
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    (cache, project)
}
fn files(cache: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    std::fs::read_dir(cache)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            assert!(path.is_file());
            (path.clone(), std::fs::read(path).unwrap())
        })
        .collect()
}
fn update(connection: &Connection) -> rusqlite::Result<usize> {
    connection.execute("UPDATE projects SET name=name", [])
}
fn assert_busy(error: &rusqlite::Error) {
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy)
    );
}

#[test]
fn writer_appearing_after_open_refuses_capture_and_releases_partial_lock() {
    let (cache, _project) = fixture();
    let writer = RefCell::new(None);
    let after_writer = RefCell::new(BTreeMap::new());
    let report = inspect_with(
        cache.path(),
        &settings::standalone().unwrap(),
        limits(),
        || {
            let connection = Connection::open(cache.path().join(DB_FILE)).unwrap();
            connection.busy_timeout(Duration::ZERO).unwrap();
            connection
                .query_row("SELECT count(*) FROM projects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            *after_writer.borrow_mut() = files(cache.path());
            *writer.borrow_mut() = Some(connection);
        },
        |_| panic!("busy database must not reach SQL capture"),
    )
    .unwrap();
    assert_eq!(
        report.state,
        RegistryState::Unavailable(UnavailableReason::Busy)
    );
    assert_eq!(files(cache.path()), *after_writer.borrow());
    assert_eq!(update(writer.borrow().as_ref().unwrap()).unwrap(), 1);
    // Drop live SQLite handles before TempDir on Windows.
    drop(writer.into_inner());
}

#[test]
fn lease_prevents_writes_until_capture_finishes() {
    let (cache, _project) = fixture();
    let before = files(cache.path());
    let report = inspect_with(
        cache.path(),
        &settings::standalone().unwrap(),
        limits(),
        || {},
        |reader| {
            assert!(reader.is_readonly("main").unwrap());
            reader
                .query_row("SELECT count(*) FROM projects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            assert!(update(reader).is_err());
            let writer = Connection::open(cache.path().join(DB_FILE)).unwrap();
            writer.busy_timeout(Duration::ZERO).unwrap();
            assert_busy(&update(&writer).unwrap_err());
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "store::registry_inspect::tests::external_write_attempt_child",
                    "--nocapture",
                ])
                .env("DOCSBASE_TEST_REGISTRY_ATTEMPT", cache.path())
                .status()
                .unwrap();
            assert!(status.success());
        },
    )
    .unwrap();
    assert!(matches!(report.state, RegistryState::Ready { .. }));
    assert_eq!(files(cache.path()), before);
    let writer = Connection::open(cache.path().join(DB_FILE)).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    assert_eq!(update(&writer).unwrap(), 1);
}

#[test]
fn external_write_attempt_child() {
    let Some(cache) = std::env::var_os("DOCSBASE_TEST_REGISTRY_ATTEMPT") else {
        return;
    };
    let writer = Connection::open(PathBuf::from(cache).join(DB_FILE)).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    assert_busy(&update(&writer).unwrap_err());
}

#[test]
fn bootstrap_header_is_reconciled_after_pagesize_and_registry_change() {
    let (cache, _project) = fixture();
    let second = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(second.path()).unwrap();
    let after_change = RefCell::new(BTreeMap::new());
    let report = inspect_with(cache.path(), &settings::standalone().unwrap(), limits(), || {
        let writer = Connection::open(cache.path().join(DB_FILE)).unwrap();
        let size: i64 = writer.pragma_query_value(None, "page_size", |row| row.get(0)).unwrap();
        assert_ne!(size, 1024);
        writer.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=1024; PRAGMA auto_vacuum=FULL; VACUUM;").unwrap();
        writer.execute("INSERT INTO projects(canonical_root,name,status,schema_version,created_at) VALUES (?1,'second','not_indexed',?2,0)", rusqlite::params![root.to_str().unwrap(), crate::store::migrations::SCHEMA_VERSION]).unwrap();
        drop(writer);
        *after_change.borrow_mut() = files(cache.path());
    }, |_| {}).unwrap();
    let RegistryState::Ready { projects, .. } = report.state else {
        panic!("must read new leased snapshot");
    };
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[1].root, root);
    assert_eq!(files(cache.path()), *after_change.borrow());
}

#[test]
fn error_capture_releases_the_exclusive_lease() {
    let (cache, _project) = fixture();
    let mut cap = limits();
    cap.max_projects = 0;
    assert_eq!(
        inspect_registry(cache.path(), &settings::standalone().unwrap(), cap)
            .unwrap()
            .state,
        RegistryState::Unavailable(UnavailableReason::LimitExceeded)
    );
    let writer = Connection::open(cache.path().join(DB_FILE)).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    assert_eq!(update(&writer).unwrap(), 1);
}

#[cfg(unix)]
#[test]
fn replacement_between_open_and_lock_is_not_a_ready_registry() {
    let (cache, _project) = fixture();
    let after_replace = RefCell::new(BTreeMap::new());
    let report = inspect_with(
        cache.path(),
        &settings::standalone().unwrap(),
        limits(),
        || {
            std::fs::rename(cache.path().join(DB_FILE), cache.path().join("moved.db")).unwrap();
            drop(Db::open(cache.path()).unwrap());
            *after_replace.borrow_mut() = files(cache.path());
        },
        |_| panic!("moved file must not reach capture"),
    )
    .unwrap();
    assert_eq!(
        report.state,
        RegistryState::Unavailable(UnavailableReason::Changed)
    );
    assert_eq!(files(cache.path()), *after_replace.borrow());
}

#[cfg(windows)]
#[test]
fn native_windows_file_and_parent_rename_are_denied_during_observation() {
    let parent = tempfile::tempdir().unwrap();
    let cache = parent.path().join("cache");
    drop(Db::open(&cache).unwrap());
    let before = files(&cache);
    let report = inspect_with(
        &cache,
        &settings::standalone().unwrap(),
        limits(),
        || {
            assert!(std::fs::rename(cache.join(DB_FILE), cache.join("moved.db")).is_err());
            assert!(std::fs::rename(&cache, parent.path().join("moved-cache")).is_err());
        },
        |_| {},
    )
    .unwrap();
    assert!(matches!(report.state, RegistryState::Ready { .. }));
    assert_eq!(files(&cache), before);
}

#[cfg(unix)]
#[test]
fn observation_preserves_existing_rollback_reader_os_lock() {
    let (cache, _project) = fixture();
    let reader = Connection::open(cache.path().join(DB_FILE)).unwrap();
    reader
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN; SELECT * FROM projects;")
        .unwrap();
    let attempt = || {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::registry_inspect::tests::external_write_attempt_child",
                "--nocapture",
            ])
            .env("DOCSBASE_TEST_REGISTRY_ATTEMPT", cache.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    attempt();
    assert_eq!(
        inspect_registry(cache.path(), &settings::standalone().unwrap(), limits())
            .unwrap()
            .state,
        RegistryState::Unavailable(UnavailableReason::Busy)
    );
    attempt();
    reader.execute_batch("ROLLBACK").unwrap();
}

// Keep OS-specific fixtures under the explicit test-module boundary used by
// the repository's platform-seam source scanner.
#[cfg(unix)]
#[cfg(test)]
mod namespace {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cache_alias_retargeted_after_open_is_not_ready() {
        let parent = tempfile::tempdir().unwrap();
        let first = parent.path().join("first");
        let second = parent.path().join("second");
        drop(Db::open(&first).unwrap());
        drop(Db::open(&second).unwrap());
        let alias = parent.path().join("alias");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let before_first = files(&first);
        let before_second = files(&second);
        let observation = inspect_with(
            &alias,
            &settings::standalone().unwrap(),
            limits(),
            || {
                std::fs::remove_file(&alias).unwrap();
                std::os::unix::fs::symlink(&second, &alias).unwrap();
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(
            observation.state,
            RegistryState::Unavailable(UnavailableReason::Changed)
        );
        assert_eq!(files(&first), before_first);
        assert_eq!(files(&second), before_second);
    }
}
