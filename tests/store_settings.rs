use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::SystemTime;

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::resolve_component;
use docsbase_memory::config::schema::{ArtifactState, Resolved, SourceId};
use docsbase_memory::error::Error;
use docsbase_memory::store::settings::{DbPatch, DbSettings, DbSpec};
use docsbase_memory::store::{DB_FILE, Db, migrations};

fn settings(patch: DbPatch) -> Resolved<DbSettings> {
    resolve_component::<DbSpec>(&DefaultsContext::standalone(), [(SourceId::Cli, patch)])
        .expect("resolve db policy")
}

fn busy_timeout(db: &Db) -> u32 {
    db.connection()
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .expect("busy timeout")
}

#[test]
fn configured_busy_timeout_reaches_connection() {
    let cache = tempfile::tempdir().expect("cache");
    let policy = settings(DbPatch {
        write_busy_timeout: Some(Duration::from_millis(17)),
        read_busy_timeout: Some(Duration::from_millis(29)),
        ..DbPatch::default()
    });
    let writer = Db::open_with_settings(cache.path(), &policy).expect("writer");
    assert_eq!(busy_timeout(&writer), 17);
    assert_eq!(writer.settings().settings(), policy.settings());
    assert_eq!(writer.settings().provenance(), policy.provenance());
    let reader = Db::open_readonly_with_settings(cache.path(), &policy).expect("reader");
    assert_eq!(busy_timeout(&reader), 29);
    assert_eq!(reader.settings().settings(), policy.settings());
    let error = reader
        .connection()
        .execute("CREATE TABLE forbidden_write (id INTEGER)", [])
        .expect_err("prepared reader must remain read-only");
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ReadOnly)
    );
}

#[test]
fn resolved_defaults_preserve_distinct_reader_and_writer_timeouts() {
    let cache = tempfile::tempdir().expect("cache");
    let policy = settings(DbPatch::default());
    let writer = Db::open_with_settings(cache.path(), &policy).expect("writer");
    assert_eq!(busy_timeout(&writer), 30_000);
    let reader = Db::open_readonly_with_settings(cache.path(), &policy).expect("reader");
    assert_eq!(busy_timeout(&reader), 5_000);
}

#[test]
fn readonly_open_never_migrates_or_creates() {
    let root = tempfile::tempdir().expect("root");
    let missing = root.path().join("missing/cache");
    let policy = settings(DbPatch::default());
    assert!(Db::open_readonly_with_settings(&missing, &policy).is_err());
    assert!(!root.path().join("missing").exists());

    let file = root.path().join(DB_FILE);
    let raw = rusqlite::Connection::open(&file).expect("old db");
    raw.execute_batch("CREATE TABLE legacy (id INTEGER);")
        .expect("legacy table");
    raw.pragma_update(None, "user_version", migrations::SCHEMA_VERSION - 1)
        .expect("old version");
    drop(raw);
    let before = fs::read(&file).expect("before");
    let error = Db::open_readonly_with_settings(root.path(), &policy).expect_err("old schema");
    assert!(matches!(error, Error::Admission { .. }));
    assert_eq!(fs::read(&file).expect("after"), before, "no migration");
}

#[test]
fn inspector_missing_does_not_create_cache() {
    let root = tempfile::tempdir().expect("root");
    let missing = root.path().join("missing/cache");
    let facts = Db::inspect_readonly(&missing, &settings(DbPatch::default())).expect("facts");
    assert_eq!(facts.artifacts.len(), 1);
    assert_eq!(facts.artifacts[0].state, ArtifactState::Missing);
    assert!(facts.artifacts[0].identity.is_none());
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
}

#[derive(Debug, PartialEq, Eq)]
struct SnapshotEntry {
    directory: bool,
    bytes: Vec<u8>,
    readonly: bool,
    modified: SystemTime,
    #[cfg(unix)]
    mode: u32,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let metadata = fs::symlink_metadata(path).expect("snapshot metadata");
        entries.insert(
            path.strip_prefix(root).expect("relative").to_path_buf(),
            SnapshotEntry {
                directory: metadata.is_dir(),
                bytes: if metadata.is_file() {
                    fs::read(path).expect("snapshot bytes")
                } else {
                    Vec::new()
                },
                readonly: metadata.permissions().readonly(),
                modified: metadata.modified().expect("mtime"),
                #[cfg(unix)]
                mode: {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                },
            },
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("snapshot directory") {
                visit(root, &entry.expect("entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn inspector_schema_mismatch_leaves_filesystem_unchanged() {
    for version in [
        migrations::SCHEMA_VERSION - 1,
        migrations::SCHEMA_VERSION + 1,
    ] {
        let cache = tempfile::tempdir().expect("cache");
        let conn = rusqlite::Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute_batch("CREATE TABLE legacy (id INTEGER);")
            .expect("table");
        conn.pragma_update(None, "user_version", version)
            .expect("schema");
        let cookie: u32 = conn
            .pragma_query_value(None, "schema_version", |row| row.get(0))
            .expect("schema cookie");
        drop(conn);
        let before = snapshot(cache.path());
        let facts =
            Db::inspect_readonly(cache.path(), &settings(DbPatch::default())).expect("facts");
        assert_eq!(
            snapshot(cache.path()),
            before,
            "no files/logs/sidecars/mode changes"
        );
        assert_eq!(facts.artifacts.len(), 1);
        let fact = &facts.artifacts[0];
        assert_eq!(fact.id.component, "metadata_db_schema");
        assert_eq!(
            fact.state,
            ArtifactState::Ready,
            "schema identity, not row readiness"
        );
        assert_eq!(
            fact.identity.as_deref(),
            Some(format!("sqlite-schema-v1:{version}:{cookie}").as_str())
        );
    }
}

#[test]
fn inspector_orphaned_sidecar_is_not_missing() {
    let cache = tempfile::tempdir().expect("cache");
    fs::write(
        cache.path().join(format!("{DB_FILE}-wal")),
        b"retained recovery data",
    )
    .expect("sidecar");
    let before = snapshot(cache.path());
    let facts = Db::inspect_readonly(cache.path(), &settings(DbPatch::default())).expect("facts");
    assert_eq!(facts.artifacts[0].state, ArtifactState::NeedsRecovery);
    assert!(facts.artifacts[0].identity.is_none());
    assert_eq!(snapshot(cache.path()), before);
}

#[test]
fn inspector_live_wal_is_unknown_without_writes() {
    let cache = tempfile::tempdir().expect("cache");
    let conn = rusqlite::Connection::open(cache.path().join(DB_FILE)).expect("writer");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                        CREATE TABLE retained (value INTEGER); INSERT INTO retained VALUES (73);",
    )
    .expect("committed WAL");
    conn.pragma_update(None, "user_version", migrations::SCHEMA_VERSION + 9)
        .expect("WAL version");
    assert!(cache.path().join(format!("{DB_FILE}-wal")).exists());
    let before = snapshot(cache.path());
    let facts = Db::inspect_readonly(cache.path(), &settings(DbPatch::default())).expect("facts");
    assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
    assert!(
        facts.artifacts[0].identity.is_none(),
        "must not trust stale main header"
    );
    assert_eq!(snapshot(cache.path()), before);
}

#[test]
fn wal_writer_child() {
    use std::io::{BufRead, Write};
    let Some(cache) = std::env::var_os("DOCSBASE_TEST_WAL_DB") else {
        return;
    };
    let conn = rusqlite::Connection::open(PathBuf::from(cache).join(DB_FILE)).expect("child db");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                        CREATE TABLE retained (value INTEGER); INSERT INTO retained VALUES (73);",
    )
    .expect("child WAL");
    conn.pragma_update(None, "user_version", migrations::SCHEMA_VERSION + 9)
        .expect("child version");
    println!("wal-ready");
    std::io::stdout().flush().expect("flush readiness");
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut input)
        .expect("wait for parent");
    drop(conn);
}

#[test]
fn inspector_wal_with_missing_sidecar_does_not_create_or_ignore_wal() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let cache = tempfile::tempdir().expect("cache");
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "wal_writer_child", "--nocapture"])
        .env("DOCSBASE_TEST_WAL_DB", cache.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("crash fixture child");
    let mut output = BufReader::new(child.stdout.take().expect("child stdout"));
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).expect("read readiness"),
            0,
            "child exited before ready"
        );
        if line.trim() == "wal-ready" {
            break;
        }
    }
    child.kill().expect("crash writer");
    child.wait().expect("writer terminal");
    fs::remove_file(cache.path().join(format!("{DB_FILE}-shm"))).expect("missing SHM fixture");
    let before = snapshot(cache.path());
    let facts = Db::inspect_readonly(cache.path(), &settings(DbPatch::default())).expect("facts");
    assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
    assert!(facts.artifacts[0].identity.is_none());
    assert_eq!(
        snapshot(cache.path()),
        before,
        "inspector cannot recreate SHM or checkpoint"
    );

    // Only after the no-write assertion, operational SQLite verifies that
    // committed data/version lives in WAL rather than the stale main file.
    let conn = rusqlite::Connection::open_with_flags(
        cache.path().join(DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("verify committed WAL");
    let value: i64 = conn
        .query_row("SELECT value FROM retained", [], |row| row.get(0))
        .expect("WAL row");
    assert_eq!(value, 73);
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("WAL version");
    assert_eq!(version, migrations::SCHEMA_VERSION + 9);
}

#[test]
fn inspector_invalid_file_has_no_identity_or_writes() {
    for bytes in [Vec::new(), b"not a SQLite database".to_vec()] {
        let cache = tempfile::tempdir().expect("cache");
        fs::write(cache.path().join(DB_FILE), bytes).expect("malformed DB");
        let before = snapshot(cache.path());
        let facts =
            Db::inspect_readonly(cache.path(), &settings(DbPatch::default())).expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
        assert_eq!(snapshot(cache.path()), before);
    }
}
