//! Schema versioning and migrations (NFR-7).

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Current schema version understood by this build.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE projects (
    id             INTEGER PRIMARY KEY,
    canonical_root TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    status         TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    last_indexed_at INTEGER
);
CREATE TABLE docs (
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
CREATE TABLE chunks (
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
CREATE TABLE sync_jobs (
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
    conn.execute_batch(&format!(
        "BEGIN;{SCHEMA_V1}PRAGMA user_version = {SCHEMA_VERSION};COMMIT;"
    ))
    .map_err(|err| Error::Internal {
        message: format!("migrate schema to v{SCHEMA_VERSION}: {err}"),
    })
}

/// Reads `PRAGMA user_version`.
///
/// # Errors
/// Returns [`Error::Internal`] when the pragma cannot be read.
pub fn read_version(conn: &Connection) -> Result<u32> {
    let raw: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|err| Error::Internal {
            message: format!("read user_version: {err}"),
        })?;
    u32::try_from(raw).map_err(|_| Error::Internal {
        message: format!("user_version out of range: {raw}"),
    })
}
