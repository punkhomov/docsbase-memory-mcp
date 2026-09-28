//! Metadata store: project registry and index state (FR-10, FR-16; NFR-3, NFR-7).

pub mod migrations;
pub mod models;

use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Database file name inside the cache root.
pub const DB_FILE: &str = "registry.db";

/// Handle to the metadata database. One writer per process (I5).
#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating if needed) the database in `cache_dir` and migrates it.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on IO/SQLite failures and [`Error::Admission`]
    /// when the on-disk schema is newer than this build supports.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(cache_dir).map_err(|err| Error::Internal {
            message: format!("create cache dir {}: {err}", cache_dir.display()),
        })?;
        let path = cache_dir.join(DB_FILE);
        let conn = Connection::open(&path).map_err(|err| sql_error(&path, &err))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| sql_error(&path, &err))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| sql_error(&path, &err))?;
        migrations::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Opens the database read-only; never migrates or creates.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the file is missing or unreadable, and
    /// [`Error::Admission`] when the schema version does not match this build.
    pub fn open_readonly(cache_dir: &Path) -> Result<Self> {
        let path = cache_dir.join(DB_FILE);
        if !path.exists() {
            return Err(Error::Internal {
                message: format!("database not found: {}", path.display()),
            });
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| sql_error(&path, &err))?;
        let version = migrations::read_version(&conn)?;
        if version != migrations::SCHEMA_VERSION {
            return Err(Error::Admission {
                message: format!(
                    "database schema {version} != supported {}; run `docsbase index` with the current build",
                    migrations::SCHEMA_VERSION
                ),
            });
        }
        Ok(Self { conn })
    }

    /// Current schema version recorded in the database.
    ///
    /// # Errors
    /// Returns an error when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::read_version(&self.conn)
    }
}

fn sql_error(path: &Path, err: &rusqlite::Error) -> Error {
    Error::Internal {
        message: format!("sqlite {}: {err}", path.display()),
    }
}
