//! Schema versioning and migrations (NFR-7).

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{Error, Result};

use super::generation_catalog;

/// Current schema version understood by this build.
pub const SCHEMA_VERSION: u32 = 4;

/// Persisted project index format stamp, independent of the registry format.
pub(crate) const PROJECT_SCHEMA_VERSION: u32 = 4;

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

const SCHEMA_V2: &str = "
ALTER TABLE projects ADD COLUMN index_generation INTEGER NOT NULL DEFAULT 0;
";

const OUTCOME_ATTEMPT_COLUMN: &str = "
ALTER TABLE projects ADD COLUMN index_attempt INTEGER NOT NULL DEFAULT 0
    CHECK (typeof(index_attempt) = 'integer' AND index_attempt >= 0);
";
const OUTCOME_RESULT_COLUMN: &str = "
ALTER TABLE projects ADD COLUMN index_outcome_json TEXT;
";

/// Migrates the database to [`SCHEMA_VERSION`].
///
/// # Errors
/// Returns [`Error::Admission`] when the database is newer than this build and
/// [`Error::Internal`] when a migration statement fails.
pub fn migrate(conn: &Connection) -> Result<()> {
    migrate_registry(conn, RegistryOpen::Legacy, || Ok(())).map(|_| ())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RegistryOpen {
    Legacy,
    ExistingCatalog,
    FreshCatalog,
}

pub(super) fn migrate_registry(
    conn: &Connection,
    mode: RegistryOpen,
    before_catalog: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    if let Some(catalog) = supported_registry(read_version(conn)?, mode)? {
        return Ok(catalog);
    }
    // `BEGIN IMMEDIATE` serializes concurrent first opens (daemon and CLI can
    // race on a fresh cache). The version is re-read inside the lock: the
    // fast path above may be stale, and the re-read keeps the loser of a race
    // from replaying an `ALTER TABLE` (`ADD COLUMN` has no `IF NOT EXISTS`).
    let tx =
        Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(migrate_error)?;
    let version = read_version(&tx)?;
    if let Some(catalog) = supported_registry(version, mode)? {
        tx.commit().map_err(migrate_error)?;
        return Ok(catalog);
    }
    let fresh_catalog = mode == RegistryOpen::FreshCatalog && version == 0 && empty_schema(&tx)?;
    if fresh_catalog {
        before_catalog()?;
    }
    migrate_legacy_tables(&tx, version)?;
    let target = if fresh_catalog {
        generation_catalog::initialize(&tx)?;
        generation_catalog::SCHEMA_VERSION
    } else {
        SCHEMA_VERSION
    };
    tx.pragma_update(None, "user_version", target)
        .map_err(migrate_error)?;
    tx.commit().map_err(migrate_error)?;
    Ok(fresh_catalog)
}

fn empty_schema(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM sqlite_schema)",
        [],
        |row| row.get(0),
    )
    .map_err(migrate_error)
}

fn supported_registry(version: u32, mode: RegistryOpen) -> Result<Option<bool>> {
    if version == generation_catalog::SCHEMA_VERSION && mode != RegistryOpen::Legacy {
        return Ok(Some(true));
    }
    if version > SCHEMA_VERSION {
        return Err(Error::Admission {
            message: format!(
                "database schema {version} is newer than supported {SCHEMA_VERSION}; run `docsbase install`"
            ),
        });
    }
    Ok((version == SCHEMA_VERSION).then_some(false))
}

fn migrate_legacy_tables(conn: &Connection, version: u32) -> Result<()> {
    let mut script = String::new();
    if version < 1 {
        script += SCHEMA_V1;
    }
    // `IF NOT EXISTS` keeps a lost version write idempotent for the tables;
    // a column probe does the same for the `ALTER TABLE` steps.
    if version < SCHEMA_VERSION && !has_column(conn, "projects", "index_generation")? {
        script += SCHEMA_V2;
    }
    if version < 3 {
        if !has_column(conn, "projects", "index_attempt")? {
            script += OUTCOME_ATTEMPT_COLUMN;
        }
        if !has_column(conn, "projects", "index_outcome_json")? {
            script += OUTCOME_RESULT_COLUMN;
        }
        // This additive bookkeeping migration does not change the lexical
        // format. Keep compatible indexes usable, without inventing a result.
        // Older stale stamps still require their existing rebuild path.
        script += "UPDATE projects SET schema_version = 3 WHERE schema_version = 2;";
    }
    if version < 4 {
        if !has_column(conn, "docs", "chunk_recipe")? {
            script += "ALTER TABLE docs ADD COLUMN chunk_recipe INTEGER;";
        }
        // A missing stamp requires rechunking on the next index run. This
        // additive migration retains the existing chunks and search index.
        script += "UPDATE projects SET schema_version = 4 WHERE schema_version = 3;";
    }
    conn.execute_batch(&script).map_err(migrate_error)
}

fn migrate_error(err: rusqlite::Error) -> Error {
    Error::internal_with_source(format!("migrate schema to v{SCHEMA_VERSION}: {err}"), err)
}

/// Whether `table` has the named column (idempotent migrations).
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(migrate_error)?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(migrate_error)?;
    for name in names {
        if name.map_err(migrate_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
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
    fn failed_catalog_bootstrap_leaves_no_partial_schema() {
        let dir = TempDir::new().unwrap();
        let legacy = open(&dir.path().join("legacy.sqlite"));
        migrate(&legacy).unwrap();
        let legacy_pages: u32 = legacy
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        let conn = open(&dir.path().join("db.sqlite"));
        conn.pragma_update(None, "max_page_count", legacy_pages + 1)
            .unwrap();
        assert!(migrate_registry(&conn, RegistryOpen::FreshCatalog, || Ok(())).is_err());
        assert!(conn.is_autocommit());
        assert_eq!(read_version(&conn).unwrap(), 0);
        let definitions: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
            .unwrap();
        assert_eq!(definitions, 0);
        conn.pragma_update(None, "max_page_count", 128).unwrap();
        assert!(migrate_registry(&conn, RegistryOpen::FreshCatalog, || Ok(())).unwrap());
        assert_eq!(
            read_version(&conn).unwrap(),
            generation_catalog::SCHEMA_VERSION
        );
        assert_eq!(generation_catalog::read_epoch(&conn).unwrap().len(), 16);
    }

    #[test]
    fn concurrent_catalog_bootstrap_reads_the_same_committed_epoch() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("db.sqlite");
        let together = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let open_catalog = || {
                let conn = open(&path);
                together.wait();
                assert!(migrate_registry(&conn, RegistryOpen::FreshCatalog, || Ok(())).unwrap());
                generation_catalog::read_epoch(&conn).unwrap()
            };
            let first = scope.spawn(open_catalog);
            let second = scope.spawn(open_catalog);
            assert_eq!(first.join().unwrap(), second.join().unwrap());
        });
    }

    #[test]
    fn catalog_bootstrap_preserves_an_existing_unversioned_schema() {
        let dir = TempDir::new().unwrap();
        let conn = open(&dir.path().join("db.sqlite"));
        conn.execute_batch(
            "CREATE TABLE label (value TEXT); INSERT INTO label VALUES ('previous state');",
        )
        .unwrap();
        let reject = || panic!("existing schema must keep its namespace policy");
        assert!(!migrate_registry(&conn, RegistryOpen::FreshCatalog, reject).unwrap());
        assert!(!migrate_registry(&conn, RegistryOpen::FreshCatalog, reject).unwrap());
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let label: String = conn
            .query_row("SELECT value FROM label", [], |row| row.get(0))
            .unwrap();
        assert_eq!(label, "previous state");
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
    fn migrate_upgrades_v1_and_is_idempotent() {
        let dir = TempDir::new().expect("dir");
        let conn = open(&dir.path().join("db.sqlite"));
        conn.execute_batch(SCHEMA_V1).expect("v1 tables");
        conn.execute_batch("PRAGMA user_version = 1;")
            .expect("v1 version");
        migrate(&conn).expect("v1 -> v2");
        assert_eq!(read_version(&conn).expect("version"), SCHEMA_VERSION);
        assert!(has_column(&conn, "projects", "index_generation").expect("column"));
        migrate(&conn).expect("idempotent");
    }

    #[test]
    fn migrate_repairs_a_lost_v2_version_write() {
        let dir = TempDir::new().expect("dir");
        let conn = open(&dir.path().join("db.sqlite"));
        conn.execute_batch(SCHEMA_V1).expect("tables");
        conn.execute_batch(SCHEMA_V2).expect("crashed v2 column");
        assert_eq!(read_version(&conn).expect("version"), 0);
        migrate(&conn).expect("repair");
        assert_eq!(read_version(&conn).expect("version"), SCHEMA_VERSION);
        assert!(has_column(&conn, "projects", "index_generation").expect("column"));
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
