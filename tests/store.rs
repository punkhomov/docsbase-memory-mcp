use std::fs;

use docsbase_memory::error::Error;
use docsbase_memory::store::{DB_FILE, Db, migrations};
use rusqlite::Connection;
use tempfile::TempDir;

fn table_names(cache: &std::path::Path) -> Vec<String> {
    let conn = Connection::open(cache.join(DB_FILE)).expect("open db");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query");
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect")
}

#[test]
fn migrate_fresh() {
    let cache = TempDir::new().expect("tempdir");
    let db = Db::open(cache.path()).expect("open");
    assert_eq!(db.schema_version().expect("version"), 1);

    let tables = table_names(cache.path());
    for expected in ["projects", "docs", "chunks", "sync_jobs"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing {expected}: {tables:?}"
        );
    }

    let conn = Connection::open(cache.path().join(DB_FILE)).expect("open raw");
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal_mode");
    assert_eq!(journal.to_lowercase(), "wal");
}

#[test]
fn cache_dir_private() {
    let cache = TempDir::new().expect("tempdir");
    let nested = cache.path().join("docsbase-memory-mcp");
    Db::open(&nested).expect("open");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&nested)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "cache dir mode: {mode:o}");
    }
}

#[test]
fn reject_newer_schema() {
    let cache = TempDir::new().expect("tempdir");
    Db::open(cache.path()).expect("open");
    {
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("open raw");
        conn.pragma_update(None, "user_version", 99).expect("bump");
    }
    let err = Db::open(cache.path()).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert_eq!(err.mcp_code(), -32010);
}

#[test]
fn readonly_open() {
    let cache = TempDir::new().expect("tempdir");
    assert!(
        Db::open_readonly(cache.path()).is_err(),
        "readonly on missing db must fail"
    );

    Db::open(cache.path()).expect("create");
    let db = Db::open_readonly(cache.path()).expect("readonly open");
    assert_eq!(db.schema_version().expect("version"), 1);
}

#[test]
fn readonly_rejects_mismatch() {
    let cache = TempDir::new().expect("tempdir");
    Db::open(cache.path()).expect("create");
    {
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("open raw");
        conn.pragma_update(None, "user_version", 99).expect("bump");
    }
    let err = Db::open_readonly(cache.path()).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
}

#[test]
fn recreate_on_missing() {
    let cache = TempDir::new().expect("tempdir");
    Db::open(cache.path()).expect("first open");
    fs::remove_file(cache.path().join(DB_FILE)).expect("remove db");

    let db = Db::open(cache.path()).expect("recreate");
    assert_eq!(
        db.schema_version().expect("version"),
        migrations::SCHEMA_VERSION
    );
}
