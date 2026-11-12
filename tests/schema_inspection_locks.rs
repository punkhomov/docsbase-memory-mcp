//! Native lock integrity for the public schema observation API.
use std::path::{Path, PathBuf};
use std::time::Duration;

use docsbase_memory::config::schema::ArtifactState;
use docsbase_memory::daemon::registry::ensure_project;
use docsbase_memory::store::{DB_FILE, Db, settings};

fn external_writer(cache: &Path, busy: bool) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "schema_writer_child", "--nocapture"])
        .env("DOCSBASE_TEST_SCHEMA_WRITER", cache)
        .env("DOCSBASE_TEST_SCHEMA_BUSY", if busy { "1" } else { "0" })
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn schema_writer_child() {
    let Some(cache) = std::env::var_os("DOCSBASE_TEST_SCHEMA_WRITER") else {
        return;
    };
    let connection = rusqlite::Connection::open(PathBuf::from(cache).join(DB_FILE)).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    let result = connection.execute("UPDATE projects SET name=name", []);
    if std::env::var("DOCSBASE_TEST_SCHEMA_BUSY").unwrap() == "1" {
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DatabaseBusy),
            "write must remain blocked: {result:?}"
        );
    } else {
        assert_eq!(result.unwrap(), 1);
    }
}

#[test]
fn schema_observation_preserves_existing_rollback_reader_lock() {
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    let reader = rusqlite::Connection::open(cache.path().join(DB_FILE)).unwrap();
    reader
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN; SELECT * FROM projects;")
        .unwrap();
    external_writer(cache.path(), true);
    let observation = Db::inspect_readonly(cache.path(), &settings::standalone().unwrap()).unwrap();
    // No raw File/snapshot reads while the SQLite reader is alive: their close
    // would itself drop POSIX process locks and invalidate this regression.
    external_writer(cache.path(), true);
    assert_eq!(observation.artifacts[0].state, ArtifactState::Unknown);
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    external_writer(cache.path(), false);
}
