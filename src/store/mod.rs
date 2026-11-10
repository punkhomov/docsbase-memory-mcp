//! Metadata store: project registry and index state (FR-10, FR-16; NFR-3, NFR-7).

mod inspect;
pub mod migrations;
pub mod models;
/// Document and chunk repository helpers (part of the public core surface).
pub mod repo;
pub mod settings;

pub use repo::MAX_SYNC_JOBS;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::config::schema::{ArtifactFacts, Capabilities, Resolved, SettingsSpec};
use crate::error::{Error, Result};
use settings::{DbSettings, DbSpec};

/// Database file name inside the cache root.
pub const DB_FILE: &str = "registry.db";

/// Handle to the metadata database. One writer per process.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
    cache_root: PathBuf,
    settings: Resolved<DbSettings>,
}

impl Db {
    /// Opens (creating if needed) the database in `cache_dir` and migrates it.
    /// Compatibility adapter resolving owner defaults; runtime composition uses
    /// [`Self::open_with_settings`] with an explicit policy.
    ///
    /// The cache directory is created with mode `0700`: the database contains
    /// document text and absolute paths (design §3 runtime layout).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on IO/`SQLite` failures and [`Error::Admission`]
    /// when the on-disk schema is newer than this build supports.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        Self::open_with_settings(cache_dir, &settings::standalone()?)
    }

    /// Opens and migrates a writer using a complete, validated caller policy.
    /// Does not read configuration or choose defaults.
    ///
    /// # Errors
    /// Invalid policy is rejected before filesystem access. Other errors match
    /// [`Self::open`].
    pub fn open_with_settings(cache_dir: &Path, settings: &Resolved<DbSettings>) -> Result<Self> {
        Self::open_with_wait(cache_dir, settings, std::thread::sleep)
    }

    fn open_with_wait(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
        mut wait: impl FnMut(std::time::Duration),
    ) -> Result<Self> {
        DbSpec::validate(settings.settings(), &Capabilities::current())?;
        let policy = settings.settings();
        ensure_private_dir(cache_dir)?;
        let path = cache_dir.join(DB_FILE);
        let conn = Connection::open(&path).map_err(|err| sql_error(&path, err))?;
        conn.busy_timeout(policy.write_busy_timeout)
            .map_err(|err| sql_error(&path, err))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| sql_error(&path, err))?;
        // A concurrent first open may hold the transition lock: journal-mode
        // changes can fail with an immediate SQLITE_BUSY that the busy handler
        // does not cover (flake 3: the path-prefixed error came from this
        // `Db::open` statement), so retry briefly - and only for BUSY.
        let mut attempts = 0;
        loop {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(()) => break,
                Err(err) => {
                    let busy = matches!(
                        &err,
                        rusqlite::Error::SqliteFailure(code, _)
                            if code.code == rusqlite::ErrorCode::DatabaseBusy
                    );
                    attempts += 1;
                    if !busy || attempts >= policy.wal_attempts {
                        return Err(sql_error(&path, err));
                    }
                    wait(policy.wal_retry_delay);
                }
            }
        }
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
            settings: settings.clone(),
        })
    }

    /// Opens the database read-only; never migrates or creates.
    /// Compatibility adapter resolving owner defaults. Runtime composition uses
    /// [`Self::open_readonly_with_settings`]; pure planning uses the inspector.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the file is missing or unreadable, and
    /// [`Error::Admission`] when the schema version does not match this build.
    pub fn open_readonly(cache_dir: &Path) -> Result<Self> {
        Self::open_readonly_with_settings(cache_dir, &settings::standalone()?)
    }

    /// Opens a prepared operational reader with its explicit read timeout.
    /// Never migrates or negotiates WAL. `SQLite` sidecars and schema-conflict
    /// diagnostics remain operational behavior, not pure inspection.
    ///
    /// # Errors
    /// Invalid policy is rejected before filesystem access. Other errors match
    /// [`Self::open_readonly`].
    pub fn open_readonly_with_settings(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
    ) -> Result<Self> {
        DbSpec::validate(settings.settings(), &Capabilities::current())?;
        let path = cache_dir.join(DB_FILE);
        if !path.exists() {
            return Err(Error::internal(format!(
                "database not found: {}",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| sql_error(&path, err))?;
        conn.busy_timeout(settings.settings().read_busy_timeout)
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
            settings: settings.clone(),
        })
    }

    /// Complete caller policy retained for explicit child connection creation.
    #[must_use]
    pub fn settings(&self) -> &Resolved<DbSettings> {
        &self.settings
    }

    /// Observes metadata schema identity without opening `SQLite` or writing
    /// state. This does not construct a query-ready connection or row snapshot.
    /// Unknown/unavailable state is reported conservatively in the facts.
    ///
    /// # Errors
    /// Returns admission errors for invalid settings before filesystem access.
    pub fn inspect_readonly(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
    ) -> Result<ArtifactFacts> {
        inspect::schema(cache_dir, settings)
    }

    /// Current schema version recorded in the database.
    ///
    /// # Errors
    /// Returns an error when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::read_version(&self.conn)
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Begins an IMMEDIATE write transaction: the write lock is taken at
    /// `BEGIN`, so a read-then-write sequence cannot fail with
    /// `SQLITE_BUSY_SNAPSHOT` — the busy handler does not cover deferred
    /// upgrades once another connection commits (flake 3 root cause,
    /// 2026-10-14; regression test in `index::job`).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when `BEGIN IMMEDIATE` fails.
    pub fn begin_write(&mut self) -> Result<Transaction<'_>> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::bootstrap::DefaultsContext;
    use crate::config::resolve::resolve_component;
    use crate::config::schema::{Provenance, SourceId};
    use crate::store::settings::DbPatch;
    use std::time::Duration;

    fn policy(patch: DbPatch) -> Resolved<DbSettings> {
        resolve_component::<DbSpec>(&DefaultsContext::standalone(), [(SourceId::Cli, patch)])
            .expect("policy")
    }

    fn reader_lock() -> (tempfile::TempDir, Connection) {
        let cache = tempfile::tempdir().expect("cache");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("lock db");
        conn.execute_batch(
            "CREATE TABLE lock_seed (id INTEGER); INSERT INTO lock_seed VALUES (1);
                            BEGIN; SELECT * FROM lock_seed;",
        )
        .expect("hold rollback-mode shared lock");
        (cache, conn)
    }

    #[test]
    fn wal_retry_policy_is_explicit() {
        let delay = Duration::from_millis(37);
        for attempts in [1, 2, 4] {
            let (cache, _lock) = reader_lock();
            let settings = policy(DbPatch {
                write_busy_timeout: Some(Duration::ZERO),
                wal_attempts: Some(attempts),
                wal_retry_delay: Some(delay),
                ..DbPatch::default()
            });
            let mut waits = Vec::new();
            let err = Db::open_with_wait(cache.path(), &settings, |value| waits.push(value))
                .expect_err("WAL transition is blocked");
            assert!(matches!(err, Error::Internal { .. }));
            assert_eq!(
                waits,
                vec![delay; (attempts - 1) as usize],
                "bounded attempts and configured delay"
            );
        }
    }

    #[test]
    fn wal_retry_can_release_a_real_reader_lock() {
        let (cache, lock) = reader_lock();
        let delay = Duration::from_millis(43);
        let settings = policy(DbPatch {
            write_busy_timeout: Some(Duration::ZERO),
            wal_attempts: Some(2),
            wal_retry_delay: Some(delay),
            ..DbPatch::default()
        });
        let mut waits = Vec::new();
        let db = Db::open_with_wait(cache.path(), &settings, |value| {
            waits.push(value);
            lock.execute_batch("COMMIT;")
                .expect("release reader on retry");
        })
        .expect("second WAL attempt succeeds");
        assert_eq!(waits, vec![delay]);
        assert_eq!(
            db.schema_version().expect("migrated"),
            migrations::SCHEMA_VERSION
        );
        let mode: String = db
            .connection()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("mode");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn invalid_db_settings_leave_files_unchanged() {
        type Open = fn(&Path, &Resolved<DbSettings>) -> Result<()>;
        let opens: [Open; 3] = [
            |path, settings| Db::open_with_settings(path, settings).map(drop),
            |path, settings| Db::open_readonly_with_settings(path, settings).map(drop),
            |path, settings| Db::inspect_readonly(path, settings).map(drop),
        ];
        let valid = settings::standalone().expect("valid policy");
        let mut zero_attempts = valid.settings().clone();
        zero_attempts.wal_attempts = 0;
        let mut write_overflow = valid.settings().clone();
        write_overflow.write_busy_timeout = Duration::from_millis(i32::MAX as u64 + 1);
        let mut read_overflow = valid.settings().clone();
        read_overflow.read_busy_timeout = Duration::from_millis(i32::MAX as u64 + 1);
        for raw in [zero_attempts, write_overflow, read_overflow] {
            // Imported Resolved<T> need not have been validated by DbSpec.
            let imported = Resolved::new(
                raw,
                Provenance::initial(&DbSpec::fields(), &SourceId::ProvidedSnapshot),
            );
            for open in opens {
                let root = tempfile::tempdir().expect("root");
                let missing = root.path().join("nested/cache");
                assert!(matches!(
                    open(&missing, &imported),
                    Err(Error::Admission { .. })
                ));
                assert!(!root.path().join("nested").exists());
                std::fs::write(root.path().join("keep"), b"original").expect("sentinel");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))
                        .expect("initial mode");
                }
                assert!(matches!(
                    open(root.path(), &imported),
                    Err(Error::Admission { .. })
                ));
                assert_eq!(std::fs::read_dir(root.path()).expect("entries").count(), 1);
                assert_eq!(
                    std::fs::read(root.path().join("keep")).expect("bytes"),
                    b"original"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        std::fs::metadata(root.path())
                            .expect("mode")
                            .permissions()
                            .mode()
                            & 0o777,
                        0o755
                    );
                }
            }
        }
    }
}
