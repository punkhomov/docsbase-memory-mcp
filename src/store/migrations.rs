//! Schema versioning and migrations (NFR-7).

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Current schema version understood by this build.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS projects (
    id             INTEGER PRIMARY KEY,
    canonical_root TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    status         TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    last_indexed_at INTEGER
);
CREATE TABLE IF NOT EXISTS docs (
    id             INTEGER PRIMARY KEY,
    project_id     INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    rel_path       TEXT NOT NULL,
    abs_path       TEXT NOT NULL,
    title          TEXT,
    frontmatter_json TEXT,
    size           INTEGER NOT NULL,
    mtime          INTEGER NOT NULL,
    content_hash   TEXT NOT NULL,
    indexed_at     INTEGER NOT NULL,
    UNIQUE(project_id, rel_path)
);
CREATE TABLE IF NOT EXISTS chunks (
    id           INTEGER PRIMARY KEY,
    doc_id       INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    heading_path TEXT NOT NULL,
    kind         TEXT NOT NULL,
    lang         TEXT,
    line_start   INTEGER NOT NULL,
    line_end     INTEGER NOT NULL,
    text         TEXT NOT NULL,
    UNIQUE(doc_id, seq)
);
CREATE TABLE IF NOT EXISTS sync_jobs (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    state       TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    stats_json  TEXT
);
";

/// Migrates the database to [`SCHEMA_VERSION`].
///
/// # Errors
/// Returns [`Error::Admission`] when the database is newer than this build and
/// [`Error::Internal`] when a migration statement fails.
pub fn migrate(conn: &Connection) -> Result<()> {
    let version = read_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Admission {
            message: format!(
                "database schema {version} is newer than supported {SCHEMA_VERSION}; run `docsbase install`"
            ),
        });
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    // `BEGIN IMMEDIATE` serializes concurrent first opens (daemon and CLI can
    // race on a fresh cache); `IF NOT EXISTS` keeps a lost version write from
    // turning the loser into a hard error.
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;{SCHEMA_V1}PRAGMA user_version = {SCHEMA_VERSION};COMMIT;"
    ))
    .map_err(|err| {
        Error::internal_with_source(format!("migrate schema to v{SCHEMA_VERSION}: {err}"), err)
    })
}

/// Reads `PRAGMA user_version`.
///
/// # Errors
/// Returns [`Error::Internal`] when the pragma cannot be read.
pub fn read_version(conn: &Connection) -> Result<u32> {
    let raw: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|err| Error::internal_with_source(format!("read user_version: {err}"), err))?;
    u32::try_from(raw).map_err(|_| Error::internal(format!("user_version out of range: {raw}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn open(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).expect("open");
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .expect("timeout");
        conn
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = TempDir::new().expect("dir");
        let conn = open(&dir.path().join("db.sqlite"));
        migrate(&conn).expect("first");
        migrate(&conn).expect("second");
        assert_eq!(read_version(&conn).expect("version"), SCHEMA_VERSION);
    }

    #[test]
    fn migrate_adopts_tables_without_version() {
        // A crashed or racing first open can leave the schema committed but
        // the version write lost; the next open must repair, not fail.
        let dir = TempDir::new().expect("dir");
        let conn = open(&dir.path().join("db.sqlite"));
        conn.execute_batch(SCHEMA_V1).expect("tables only");
        assert_eq!(read_version(&conn).expect("version"), 0);
        migrate(&conn).expect("repair");
        assert_eq!(read_version(&conn).expect("version"), SCHEMA_VERSION);
    }

    #[test]
    fn concurrent_first_open_is_safe() {
        let dir = TempDir::new().expect("dir");
        let path = Arc::new(dir.path().join("db.sqlite"));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let conn = open(&path);
                    migrate(&conn).expect("concurrent migrate");
                    read_version(&conn).expect("version")
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().expect("join"), SCHEMA_VERSION);
        }
    }
}
