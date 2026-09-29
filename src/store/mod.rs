//! Metadata store: project registry and index state (FR-10, FR-16; NFR-3, NFR-7).

pub mod migrations;
pub mod models;
pub(crate) mod repo;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Database file name inside the cache root.
pub const DB_FILE: &str = "registry.db";

/// Handle to the metadata database. One writer per process.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
    cache_root: PathBuf,
}

impl Db {
    /// Opens (creating if needed) the database in `cache_dir` and migrates it.
    ///
    /// The cache directory is created with mode `0700`: the database contains
    /// document text and absolute paths (design §3 runtime layout).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on IO/`SQLite` failures and [`Error::Admission`]
    /// when the on-disk schema is newer than this build supports.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        ensure_private_dir(cache_dir)?;
        let path = cache_dir.join(DB_FILE);
        let conn = Connection::open(&path).map_err(|err| sql_error(&path, err))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|err| sql_error(&path, err))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| sql_error(&path, err))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| sql_error(&path, err))?;
        if let Err(err) = migrations::migrate(&conn) {
            if matches!(err, Error::Admission { .. }) {
                let version = migrations::read_version(&conn).ok();
                let expected = migrations::SCHEMA_VERSION.to_string();
                let actual = version.map_or_else(|| "?".to_owned(), |v| v.to_string());
                crate::conflict::record(
                    cache_dir,
                    &crate::conflict::Conflict {
                        kind: "db_schema_newer",
                        expected: &expected,
                        actual: &actual,
                        build_id: &crate::ipc::protocol::build_id(),
                        schema_version: migrations::SCHEMA_VERSION,
                        cache_root: cache_dir,
                        pid: std::process::id(),
                        recorded_build_id: None,
                        recorded_schema_version: version,
                        holder_pid: None,
                    },
                );
            }
            return Err(err);
        }
        Ok(Self {
            conn,
            cache_root: cache_dir.to_path_buf(),
        })
    }

    /// Opens the database read-only; never migrates or creates.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the file is missing or unreadable, and
    /// [`Error::Admission`] when the schema version does not match this build.
    pub fn open_readonly(cache_dir: &Path) -> Result<Self> {
        let path = cache_dir.join(DB_FILE);
        if !path.exists() {
            return Err(Error::internal(format!(
                "database not found: {}",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| sql_error(&path, err))?;
        let version = migrations::read_version(&conn)?;
        if version != migrations::SCHEMA_VERSION {
            let expected = migrations::SCHEMA_VERSION.to_string();
            let actual = version.to_string();
            let newer = version > migrations::SCHEMA_VERSION;
            crate::conflict::record(
                cache_dir,
                &crate::conflict::Conflict {
                    kind: "db_schema_mismatch",
                    expected: &expected,
                    actual: &actual,
                    build_id: &crate::ipc::protocol::build_id(),
                    schema_version: migrations::SCHEMA_VERSION,
                    cache_root: cache_dir,
                    pid: std::process::id(),
                    recorded_build_id: None,
                    recorded_schema_version: Some(version),
                    holder_pid: None,
                },
            );
            let hint = if newer {
                "run `docsbase install` (this database was written by a newer build)"
            } else {
                "run `docsbase index` with the current build to rebuild"
            };
            return Err(Error::Admission {
                message: format!(
                    "database schema {version} != supported {}; {hint}",
                    migrations::SCHEMA_VERSION
                ),
            });
        }
        Ok(Self {
            conn,
            cache_root: cache_dir.to_path_buf(),
        })
    }

    /// Current schema version recorded in the database.
    ///
    /// # Errors
    /// Returns an error when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::read_version(&self.conn)
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub(crate) fn cache_root(&self) -> &Path {
        &self.cache_root
    }
}

fn sql_error(path: &Path, err: rusqlite::Error) -> Error {
    Error::internal_with_source(format!("sqlite {}: {err}", path.display()), err)
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    crate::platform::fs::secure_dir(dir).map_err(|err| {
        Error::internal_with_source(format!("secure cache dir {}: {err}", dir.display()), err)
    })
}
