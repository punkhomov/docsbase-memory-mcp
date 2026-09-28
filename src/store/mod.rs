//! Metadata store: project registry and index state (FR-10, FR-16; NFR-3, NFR-7).

pub mod migrations;
pub mod models;

use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Database file name inside the cache root.
pub const DB_FILE: &str = "registry.db";

/// Handle to the metadata database. One writer per process.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating if needed) the database in `cache_dir` and migrates it.
    ///
    /// The cache directory is created with mode `0700`: the database contains
    /// document text and absolute paths (design §3 runtime layout).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on IO/SQLite failures and [`Error::Admission`]
    /// when the on-disk schema is newer than this build supports.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        ensure_private_dir(cache_dir)?;
        let path = cache_dir.join(DB_FILE);
        let conn = Connection::open(&path).map_err(|err| sql_error(&path, err))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| sql_error(&path, err))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| sql_error(&path, err))?;
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
            return Err(Error::internal(format!(
                "database not found: {}",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| sql_error(&path, err))?;
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

fn sql_error(path: &Path, err: rusqlite::Error) -> Error {
    Error::internal_with_source(format!("sqlite {}: {err}", path.display()), err)
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|err| {
        Error::internal_with_source(format!("create cache dir {}: {err}", dir.display()), err)
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, mode).map_err(|err| {
            Error::internal_with_source(format!("set 0700 on {}: {err}", dir.display()), err)
        })?;
    }
    Ok(())
}
