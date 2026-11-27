//! Metadata store: project registry and index state (FR-10, FR-16; NFR-3, NFR-7).

pub mod generation_build;
mod generation_catalog;
mod generation_seal;
mod inspect;
pub mod migrations;
pub mod models;
mod plan;
pub(crate) mod project_index;
pub mod read_pool;
pub mod registry_inspect;
/// Document and chunk repository helpers (part of the public core surface).
pub mod repo;
pub mod settings;

pub use repo::MAX_SYNC_JOBS;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::config::schema::{ArtifactFacts, Capabilities, Resolved, SettingsSpec};
use crate::error::{Error, Result};
use migrations::RegistryOpen;
use settings::{DbSettings, DbSpec};

/// Database file name inside the cache root.
pub const DB_FILE: &str = "registry.db";
pub(crate) const GENERATION_SCHEMA_VERSION: u32 = generation_catalog::SCHEMA_VERSION;

/// Handle to the metadata database. One writer per process.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
    cache_root: PathBuf,
    settings: Resolved<DbSettings>,
    generation_catalog: bool,
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
        Self::open_with_wait(
            cache_dir,
            settings,
            RegistryOpen::Legacy,
            std::thread::sleep,
        )
    }

    pub(crate) fn open_for_index(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
        create_catalog: bool,
    ) -> Result<Self> {
        let mode = if create_catalog {
            RegistryOpen::FreshCatalog
        } else {
            RegistryOpen::ExistingCatalog
        };
        Self::open_with_wait(cache_dir, settings, mode, std::thread::sleep)
    }

    fn open_with_wait(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
        mode: RegistryOpen,
        mut wait: impl FnMut(std::time::Duration),
    ) -> Result<Self> {
        DbSpec::validate(settings.settings(), &Capabilities::current())?;
        let policy = settings.settings();
        ensure_private_dir(cache_dir)?;
        let cache_root = if mode == RegistryOpen::FreshCatalog {
            cache_dir.canonicalize().map_err(|error| {
                Error::internal_with_source("resolve generation registry cache", error)
            })?
        } else {
            cache_dir.to_path_buf()
        };
        let cache_dir = cache_root.as_path();
        let path = cache_dir.join(DB_FILE);
        let mut flags = rusqlite::OpenFlags::default();
        let mode = if mode != RegistryOpen::FreshCatalog {
            mode
        } else if is_registry_alias(&path)? {
            // An existing linked legacy registry retains its former opener.
            // A dangling alias must not create a registry at another location.
            flags.remove(rusqlite::OpenFlags::SQLITE_OPEN_CREATE);
            RegistryOpen::Legacy
        } else {
            check_catalog_database_paths(cache_dir)?;
            flags.insert(rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW);
            mode
        };
        #[cfg(windows)]
        let mode = catalog_mode_on_windows(cache_dir, &path, mode, flags, policy)?;
        let conn =
            Connection::open_with_flags(&path, flags).map_err(|err| sql_error(&path, err))?;
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
        if mode == RegistryOpen::FreshCatalog && migrations::read_version(&conn)? == 0 {
            conn.pragma_update(None, "synchronous", "EXTRA")
                .map_err(|err| sql_error(&path, err))?;
        }
        let mut created_catalog = false;
        let before_creation = || {
            sync_catalog_namespace(cache_dir)?;
            created_catalog = true;
            Ok(())
        };
        let generation_catalog = migrations::migrate_registry(&conn, mode, before_creation)
            .inspect_err(|err| {
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
            })?;
        if created_catalog {
            generation_build::sync_directory(cache_dir)?;
        }
        Ok(Self {
            conn,
            cache_root,
            settings: settings.clone(),
            generation_catalog,
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
        Self::open_readonly_using(cache_dir, settings, true, false)
    }

    // Startup admission must observe SQLite's actual live WAL/schema without
    // creating conflict diagnostics before the daemon is admitted. Ordinary
    // readonly callers retain their existing diagnostics through the adapter.
    pub(crate) fn open_readonly_for_admission(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
        allow_generation_catalog: bool,
    ) -> Result<Self> {
        Self::open_readonly_using(cache_dir, settings, false, allow_generation_catalog)
    }

    pub(crate) fn open_readonly_for_search(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
    ) -> Result<Self> {
        Self::open_readonly_using(cache_dir, settings, true, true)
    }

    fn open_readonly_using(
        cache_dir: &Path,
        settings: &Resolved<DbSettings>,
        report_conflict: bool,
        allow_generation_catalog: bool,
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
        let generation_catalog =
            allow_generation_catalog && version == generation_catalog::SCHEMA_VERSION;
        if version != migrations::SCHEMA_VERSION && !generation_catalog {
            let expected = migrations::SCHEMA_VERSION.to_string();
            let actual = version.to_string();
            let newer = version > migrations::SCHEMA_VERSION;
            if report_conflict {
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
            }
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
            generation_catalog,
        })
    }

    #[must_use]
    pub(crate) const fn has_generation_catalog(&self) -> bool {
        self.generation_catalog
    }

    pub(crate) fn generation_epoch(&self) -> Result<[u8; 16]> {
        generation_catalog::read_epoch(&self.conn).map_err(|reason| Error::Admission {
            message: format!("invalid generation catalog ({reason:?})"),
        })
    }

    /// Finish SQLite-only recovery before a new daemon observes the catalog.
    /// Caller holds stopped daemon admission. Existing registry only.
    pub(crate) fn recover_catalog_for_startup(
        cache: &Path,
        settings: &Resolved<DbSettings>,
    ) -> Result<()> {
        use rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE;

        let cache = cache.canonicalize().map_err(|error| {
            Error::internal_with_source("resolve registry cache before startup recovery", error)
        })?;
        check_catalog_database_paths(&cache)?;
        let path = cache.join(DB_FILE);
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| sql_error(&path, error))?;
        // The effective schema can be in WAL. Before qualifying that schema,
        // even closing the last connection must leave its committed frames.
        conn.set_db_config(SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .map_err(|error| sql_error(&path, error))?;
        conn.busy_timeout(settings.settings().write_busy_timeout)
            .map_err(|error| sql_error(&path, error))?;
        match migrations::read_version(&conn)? {
            GENERATION_SCHEMA_VERSION => {
                generation_build::admit_catalog_cache(&cache)?;
                generation_catalog::read_epoch(&conn).map_err(|reason| Error::Admission {
                    message: format!(
                        "invalid generation catalog during startup recovery ({reason:?})"
                    ),
                })?;
            }
            // A legacy first commit can also live entirely in WAL.
            1..=migrations::SCHEMA_VERSION => {}
            _ => {
                return Err(Error::Admission {
                    message: "registry changed before startup recovery".into(),
                });
            }
        }
        let busy: u32 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|error| sql_error(&path, error))?;
        if busy != 0 {
            return Err(Error::Admission {
                message: "registry is busy during startup recovery; retry".into(),
            });
        }
        conn.set_db_config(SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)
            .map_err(|error| sql_error(&path, error))?;
        conn.close().map_err(|(_, error)| sql_error(&path, error))
    }

    pub(crate) fn close(self) -> Result<()> {
        self.conn
            .close()
            .map_err(|(_, error)| repo::db_error(error))
    }

    /// Complete caller policy retained for explicit child connection creation.
    #[must_use]
    pub fn settings(&self) -> &Resolved<DbSettings> {
        &self.settings
    }

    /// Observes metadata schema identity without SQL, recovery or persisted writes.
    /// Uses a native exclusive inspection lease; busy stores remain unknown.
    /// This does not construct a query-ready connection or row snapshot.
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

fn is_registry_alias(path: &Path) -> Result<bool> {
    match crate::platform::is_file_alias(path) {
        Ok(linked) => Ok(linked),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::internal_with_source(
            "inspect generation registry file",
            error,
        )),
    }
}

#[cfg(windows)]
fn catalog_mode_on_windows(
    cache: &Path,
    path: &Path,
    mode: RegistryOpen,
    flags: rusqlite::OpenFlags,
    policy: &DbSettings,
) -> Result<RegistryOpen> {
    use rusqlite::OpenFlags;

    if mode == RegistryOpen::Legacy {
        return Ok(mode);
    }
    let probe_error = match generation_build::catalog_cache_is_supported(cache) {
        Ok(true) => return Ok(mode),
        Ok(false) => None,
        Err(error) => Some(error),
    };
    // A write-enabled pager could recover a journal during schema discovery.
    // On unqualified storage, establish legacy compatibility before opening it.
    let version = if path.try_exists().map_err(|error| {
        Error::internal_with_source("inspect registry before catalog admission", error)
    })? {
        let read_flags = flags
            .difference(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)
            | OpenFlags::SQLITE_OPEN_READ_ONLY;
        let reader = Connection::open_with_flags(path, read_flags)
            .map_err(|error| sql_error(path, error))?;
        reader
            .busy_timeout(policy.read_busy_timeout)
            .map_err(|error| sql_error(path, error))?;
        migrations::read_version(&reader)?
    } else {
        0
    };
    if version > migrations::SCHEMA_VERSION {
        return Err(probe_error.unwrap_or_else(|| Error::Admission {
            message: "catalog publication requires a writable local NTFS directory".to_owned(),
        }));
    }
    if version == 0
        && mode == RegistryOpen::FreshCatalog
        && let Some(error) = probe_error
    {
        return Err(error);
    }
    Ok(RegistryOpen::Legacy)
}

#[cfg(unix)]
fn sync_catalog_namespace(cache: &Path) -> Result<()> {
    // Earlier attempts may have created any prefix before the directory
    // barriers completed. A fresh catalog must make the entire path durable.
    for directory in cache.ancestors() {
        generation_build::sync_directory(directory)?;
    }
    Ok(())
}

#[cfg(windows)]
fn sync_catalog_namespace(cache: &Path) -> Result<()> {
    // The closest known Windows profile directory anchors the namespace.
    // Outside these anchors an ACL error cannot prove ancestry was durable.
    let dirs = directories::BaseDirs::new();
    let anchors: Vec<_> = dirs
        .iter()
        .flat_map(|dirs| [dirs.cache_dir(), dirs.home_dir()])
        .filter_map(|directory| directory.canonicalize().ok())
        .collect();
    for directory in cache.ancestors() {
        generation_build::sync_directory(directory)?;
        if anchors.iter().any(|anchor| anchor == directory) {
            break;
        }
    }
    Ok(())
}

fn check_catalog_database_paths(cache: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = cache.join(format!("{DB_FILE}{suffix}"));
        match crate::platform::fs::check_single_link(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let context = if suffix.is_empty() {
                    "inspect generation registry file"
                } else {
                    "inspect registry sidecar"
                };
                return Err(Error::internal_with_source(context, error));
            }
        }
    }
    Ok(())
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
    fn admission_reader_wrong_schema_creates_no_conflict_state() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(DB_FILE);
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", migrations::SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&path).unwrap();
        assert!(matches!(
            Db::open_readonly_for_admission(cache.path(), &settings::standalone().unwrap(), false),
            Err(Error::Admission { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
        assert!(!cache.path().join("logs").exists());
        assert!(
            Db::open_readonly_with_settings(cache.path(), &settings::standalone().unwrap())
                .is_err()
        );
        assert!(
            cache.path().join("logs/conflicts.ndjson").exists(),
            "ordinary readonly callers retain diagnostics"
        );
    }

    #[test]
    fn admission_reader_uses_live_wal_schema_and_rows() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(DB_FILE);
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=2; CREATE TABLE admission_probe (value INTEGER); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        writer
            .pragma_update(None, "user_version", migrations::SCHEMA_VERSION)
            .unwrap();
        writer
            .execute("INSERT INTO admission_probe VALUES (7)", [])
            .unwrap();
        assert_eq!(
            &std::fs::read(&path).unwrap()[60..64],
            &2_u32.to_be_bytes(),
            "main-only inspection would see an obsolete schema"
        );
        let reader =
            Db::open_readonly_for_admission(cache.path(), &settings::standalone().unwrap(), false)
                .unwrap();
        assert_eq!(reader.schema_version().unwrap(), migrations::SCHEMA_VERSION);
        assert_eq!(
            reader
                .connection()
                .query_row("SELECT value FROM admission_probe", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            7
        );
        assert!(
            reader
                .connection()
                .execute("INSERT INTO admission_probe VALUES (8)", [])
                .is_err()
        );
        assert!(!cache.path().join("logs").exists());
    }

    #[test]
    fn startup_recovers_the_first_catalog_commit_from_wal() {
        check_first_commit_recovery(RegistryOpen::FreshCatalog, GENERATION_SCHEMA_VERSION);
    }

    #[test]
    fn startup_preserves_the_legacy_layout_when_the_first_commit_is_in_wal() {
        check_first_commit_recovery(RegistryOpen::Legacy, migrations::SCHEMA_VERSION);
    }

    fn check_first_commit_recovery(mode: RegistryOpen, expected_version: u32) {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join(DB_FILE)).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let catalog = migrations::migrate_registry(&writer, mode, || Ok(())).unwrap();
        let epoch = catalog.then(|| generation_catalog::read_epoch(&writer).unwrap());
        let stopped = copy_uncheckpointed_registry(source.path());
        assert_eq!(
            &std::fs::read(stopped.path().join(DB_FILE)).unwrap()[60..64],
            &0_u32.to_be_bytes()
        );
        assert!(registry_inspect::startup_catalog_requires_recovery(
            stopped.path()
        ));
        Db::recover_catalog_for_startup(stopped.path(), &settings::standalone().unwrap()).unwrap();
        assert!(!stopped.path().join("registry.db-wal").exists());
        let observer = Connection::open(stopped.path().join(DB_FILE)).unwrap();
        assert_eq!(
            migrations::read_version(&observer).unwrap(),
            expected_version
        );
        if let Some(epoch) = epoch {
            assert_eq!(generation_catalog::read_epoch(&observer).unwrap(), epoch);
        }
    }

    fn copy_uncheckpointed_registry(source: &Path) -> tempfile::TempDir {
        let stopped = tempfile::tempdir().unwrap();
        for name in [DB_FILE, "registry.db-wal"] {
            std::fs::copy(source.join(name), stopped.path().join(name)).unwrap();
        }
        stopped
    }

    #[test]
    fn rejected_startup_recovery_does_not_checkpoint_on_close() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join(DB_FILE)).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        writer
            .pragma_update(None, "user_version", GENERATION_SCHEMA_VERSION + 1)
            .unwrap();
        let stopped = copy_uncheckpointed_registry(source.path());
        let registry = || {
            [DB_FILE, "registry.db-wal"].map(|name| {
                std::fs::read(stopped.path().join(name))
                    .ok()
                    .map(|bytes| blake3::hash(&bytes))
            })
        };
        let before = registry();
        assert!(matches!(
            Db::recover_catalog_for_startup(stopped.path(), &settings::standalone().unwrap()),
            Err(Error::Admission { .. })
        ));
        assert_eq!(registry(), before);
    }

    #[test]
    fn admission_reader_missing_database_does_not_create_cache() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("missing");
        assert!(
            Db::open_readonly_for_admission(&absent, &settings::standalone().unwrap(), false)
                .is_err()
        );
        assert!(!absent.exists());
    }

    #[cfg(unix)]
    #[test]
    fn fresh_catalog_rejects_an_unsafe_sidecar_before_opening_sqlite() {
        let cache = tempfile::tempdir().unwrap();
        let db_path = cache.path().join(DB_FILE);
        let unrelated = cache.path().join("other.db");
        let sidecar = cache.path().join(format!("{DB_FILE}-shm"));
        std::fs::write(&unrelated, b"unrelated file").unwrap();
        std::fs::hard_link(&unrelated, &sidecar).unwrap();

        let error = Db::open_for_index(cache.path(), &settings::standalone().unwrap(), true)
            .expect_err("fresh catalog refuses a linked SQLite sidecar");
        assert!(error.to_string().contains("inspect registry sidecar"));
        assert!(!db_path.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated file");
    }

    #[cfg(windows)]
    #[test]
    fn fresh_catalog_sharing_denial_precedes_database_creation() {
        let cache = tempfile::tempdir().unwrap();
        let settings = settings::standalone().unwrap();
        let blocker = crate::platform::fs::block_directory_sync_for_test(cache.path()).unwrap();
        let error = Db::open_for_index(cache.path(), &settings, true).unwrap_err();
        assert!(error.to_string().contains("qualify catalog directory"));
        assert!(!cache.path().join(DB_FILE).exists());
        drop(blocker);

        let db = Db::open_for_index(cache.path(), &settings, true).unwrap();
        assert!(db.has_generation_catalog());
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
            let err = Db::open_with_wait(cache.path(), &settings, RegistryOpen::Legacy, |value| {
                waits.push(value);
            })
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
        let db = Db::open_with_wait(cache.path(), &settings, RegistryOpen::Legacy, |value| {
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
