//! Bounded registry observation from a quiescent metadata database. This is not
//! an operational reader and never prepares recovery, migrations or sidecars.
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::generation_catalog;
use super::settings::{DbSettings, DbSpec};
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::error::{Error, Result};

pub(super) mod lease;
mod project;
pub(super) use project::{
    LocalRevision, check_directory, check_identity_budget, check_revised_tantivy,
    check_tantivy_files, local_revision, observe_project, open_checked_file, read_checked_seal,
    read_local_revision, tantivy_manifest,
};
pub use project::{
    ObservedProjectIndex, ProjectIndexLayout, ProjectIndexObservation, inspect_project_index,
};
pub(crate) use project::{ProjectRecipeInspection, SelectedIndexEvidence, inspect_project_recipes};

/// Caller-owned limits; no runtime fallback/default is chosen by the observer.
#[derive(Debug, Clone, Copy)]
pub struct InspectionLimits {
    /// Maximum complete registry row count; zero admits an empty registry only.
    pub max_projects: usize,
    /// Retained UTF-8 cache/root bytes plus four schema bytes and eight bytes/id.
    /// Allocator overhead and `SQLite`'s internal schema heap are not RSS-bounded.
    pub max_metadata_bytes: usize,
    /// `SQLite`'s maximum individual string/blob/record value, independently bounded.
    pub max_sqlite_value_bytes: i32,
    /// Explicit soft `SQLite` page-cache target in KiB, never zero.
    pub sqlite_cache_kib: i32,
}
/// Only registry identity is captured, not indexing status or runtime readiness.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryProject {
    /// Persisted project id.
    pub id: i64,
    /// Registered absolute root; it may no longer exist on disk.
    pub root: PathBuf,
}
/// Conservative diagnostic without SQL/source values or raw error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// A native `SQLite` reader/writer prevents exclusive observation.
    Busy,
    /// WAL/SHM/journal requires a coherent observation/recovery path.
    RecoveryRequired,
    /// The database namespace changed during observation.
    Changed,
    /// The native locking/namespace contract is not recognized.
    UnsupportedVfs,
    /// Persisted metadata schema differs from this build.
    UnsupportedSchema,
    /// Registry rows have unsupported types/identities/paths.
    InvalidRegistry,
    /// Complete bounded observation does not fit the caller's limits.
    LimitExceeded,
    /// No trustworthy observation was possible.
    Io,
}
/// Missing is distinct from a captured, empty registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RegistryState {
    /// No metadata DB or recovery sidecars were observed.
    Missing,
    /// The registry must not be treated as complete/empty.
    Unavailable(UnavailableReason),
    /// Complete rows from one quiescent leased snapshot, not query readiness.
    Ready {
        /// Recognized metadata schema.
        schema_version: u32,
        /// Versioned digest bound to exact caller-selected cache context and rows.
        revision: String,
        /// Complete registry in increasing id order.
        projects: Vec<RegistryProject>,
    },
}
/// Captured registry facts, always requiring revalidation before future apply.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryObservation {
    /// Observation DTO version.
    pub observation_version: u32,
    /// Exact caller-selected absolute context.
    pub cache_root: PathBuf,
    /// Result at the observation boundary.
    pub state: RegistryState,
}

/// Observe the complete registry without creating or modifying persisted state.
/// Active/recovery-bearing stores remain unavailable; this never stops a daemon.
///
/// # Errors
/// Invalid caller settings, limits or cache path. Operational observation failures
/// are represented as `Unavailable`, never as an empty registry.
pub fn inspect_registry(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<RegistryObservation> {
    inspect_with(cache, settings, limits, || {}, |_| {})
}

fn inspect_with(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    before_lock: impl FnOnce(),
    after_lock: impl FnOnce(&Connection),
) -> Result<RegistryObservation> {
    let initial_bytes = validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    // Never open/close a raw descriptor for the DB: POSIX close would release
    // another SQLite connection's process-wide locks outside VFS bookkeeping.
    let state = match presence(cache) {
        Ok(false) => RegistryState::Missing,
        Ok(true) => match capture(cache, limits, initial_bytes, before_lock, after_lock) {
            Ok(state) => state,
            Err(reason) => RegistryState::Unavailable(reason),
        },
        Err(reason) => RegistryState::Unavailable(reason),
    };
    Ok(RegistryObservation {
        observation_version: 1,
        cache_root: cache.to_owned(),
        state,
    })
}
fn presence(cache: &Path) -> std::result::Result<bool, UnavailableReason> {
    let path = cache.join(super::DB_FILE);
    sidecars_absent(&path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            sidecars_absent(&path)?;
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                _ => Err(UnavailableReason::Changed),
            }
        }
        Ok(_) | Err(_) => Err(UnavailableReason::Io),
    }
}

pub(super) fn validate(cache: &Path, limits: InspectionLimits) -> Result<usize> {
    let invalid = || Error::Admission {
        message: "invalid registry inspection limits or cache context".into(),
    };
    let path = cache.to_str().ok_or_else(invalid)?;
    if !cache.is_absolute()
        || path.as_bytes().contains(&0)
        || limits.max_sqlite_value_bytes <= 0
        || limits.sqlite_cache_kib <= 0
        || limits
            .max_projects
            .checked_add(1)
            .and_then(|n| i64::try_from(n).ok())
            .is_none()
    {
        return Err(invalid());
    }
    path.len()
        .checked_add(4)
        .filter(|size| *size <= limits.max_metadata_bytes)
        .ok_or_else(invalid)
}
fn capture(
    cache: &Path,
    limits: InspectionLimits,
    initial_bytes: usize,
    before_lock: impl FnOnce(),
    after_lock: impl FnOnce(&Connection),
) -> std::result::Result<RegistryState, UnavailableReason> {
    with_registry(cache, limits, before_lock, |connection, _| {
        after_lock(connection);
        let version = schema_version(connection)?;
        if !matches!(
            version,
            super::migrations::SCHEMA_VERSION | generation_catalog::SCHEMA_VERSION
        ) {
            return Err(UnavailableReason::UnsupportedSchema);
        }
        let projects = read_rows(connection, limits, initial_bytes)?;
        let catalog = (version == generation_catalog::SCHEMA_VERSION)
            .then(|| generation_catalog::identity(connection, &projects))
            .transpose()?;
        let revision = revision(cache, version, &projects, catalog)?;
        Ok(RegistryState::Ready {
            schema_version: version,
            revision,
            projects,
        })
    })
}

pub(super) fn with_registry<T>(
    cache: &Path,
    limits: InspectionLimits,
    before_lock: impl FnOnce(),
    read: impl FnOnce(&Connection, &Path) -> std::result::Result<T, UnavailableReason>,
) -> std::result::Result<T, UnavailableReason> {
    with_registry_lease(cache, limits, before_lock, |locked, directory| {
        read(locked.connection(), directory)
    })
}

pub(super) fn with_registry_lease<T>(
    cache: &Path,
    limits: InspectionLimits,
    before_lock: impl FnOnce(),
    read: impl FnOnce(&lease::LockedConnection, &Path) -> std::result::Result<T, UnavailableReason>,
) -> std::result::Result<T, UnavailableReason> {
    // Resolve only the cache directory. SQLite NOFOLLOW must still check the DB entry.
    let directory = std::fs::canonicalize(cache).map_err(|_| UnavailableReason::Io)?;
    with_locked_db(
        &directory.join(super::DB_FILE),
        limits,
        before_lock,
        |connection| {
            let value = read(connection, &directory)?;
            if std::fs::canonicalize(cache).map_err(|_| UnavailableReason::Changed)? != directory {
                return Err(UnavailableReason::Changed);
            }
            Ok(value)
        },
    )
}

fn with_locked_db<T>(
    path: &Path,
    limits: InspectionLimits,
    before_lock: impl FnOnce(),
    read: impl FnOnce(&lease::LockedConnection) -> std::result::Result<T, UnavailableReason>,
) -> std::result::Result<T, UnavailableReason> {
    sidecars_absent(path)?;
    let locked = lease::LockedConnection::open(path, before_lock)?;
    sidecars_absent(path)?;
    configure_connection(locked.connection(), limits).map_err(sqlite_failure)?;
    let value = read(&locked)?;
    locked.check_namespace()?;
    sidecars_absent(path)?;
    Ok(value)
}

pub(super) fn configure_connection(
    connection: &Connection,
    limits: InspectionLimits,
) -> rusqlite::Result<()> {
    lease::configure_connection(
        connection,
        limits.max_sqlite_value_bytes,
        limits.sqlite_cache_kib,
    )
}

fn schema_version(connection: &Connection) -> std::result::Result<u32, UnavailableReason> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sqlite_failure)?;
    u32::try_from(version).map_err(|_| UnavailableReason::UnsupportedSchema)
}

pub(super) fn sidecars_absent(path: &Path) -> std::result::Result<(), UnavailableReason> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match std::fs::symlink_metadata(sidecar) {
            Ok(_) => return Err(UnavailableReason::RecoveryRequired),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(UnavailableReason::Io),
        }
    }
    Ok(())
}

fn read_rows(
    connection: &Connection,
    limits: InspectionLimits,
    mut bytes: usize,
) -> std::result::Result<Vec<RegistryProject>, UnavailableReason> {
    let limit = limits
        .max_projects
        .checked_add(1)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or(UnavailableReason::LimitExceeded)?;
    let mut statement = connection
        .prepare("SELECT id, canonical_root FROM projects ORDER BY rowid LIMIT ?1")
        .map_err(sqlite_failure)?;
    let mut rows = statement.query([limit]).map_err(sqlite_failure)?;
    let mut projects = Vec::new();
    let mut previous = 0;
    while let Some(row) = rows.next().map_err(sqlite_failure)? {
        if projects.len() == limits.max_projects {
            return Err(UnavailableReason::LimitExceeded);
        }
        let id: i64 = row.get(0).map_err(|_| UnavailableReason::InvalidRegistry)?;
        let value = row
            .get_ref(1)
            .map_err(|_| UnavailableReason::InvalidRegistry)?;
        let root = value
            .as_str()
            .map_err(|_| UnavailableReason::InvalidRegistry)?;
        if id <= previous || root.as_bytes().contains(&0) || !Path::new(root).is_absolute() {
            return Err(UnavailableReason::InvalidRegistry);
        }
        bytes = bytes
            .checked_add(8)
            .and_then(|n| n.checked_add(root.len()))
            .filter(|size| *size <= limits.max_metadata_bytes)
            .ok_or(UnavailableReason::LimitExceeded)?;
        projects.push(RegistryProject {
            id,
            root: PathBuf::from(root),
        });
        previous = id;
    }
    Ok(projects)
}
fn revision(
    cache: &Path,
    version: u32,
    projects: &[RegistryProject],
    catalog: Option<blake3::Hash>,
) -> std::result::Result<String, UnavailableReason> {
    fn path(hash: &mut blake3::Hasher, value: &Path) -> std::result::Result<(), UnavailableReason> {
        let text = value.to_str().ok_or(UnavailableReason::InvalidRegistry)?;
        let length = u64::try_from(text.len()).map_err(|_| UnavailableReason::LimitExceeded)?;
        hash.update(&length.to_le_bytes());
        hash.update(text.as_bytes());
        Ok(())
    }
    let (context, prefix) = if catalog.is_some() {
        ("docsbase.registry.v2", "registry-v2")
    } else {
        ("docsbase.registry.v1", "registry-v1")
    };
    let mut hash = blake3::Hasher::new_derive_key(context);
    path(&mut hash, cache)?;
    hash.update(&version.to_le_bytes());
    if let Some(catalog) = catalog {
        hash.update(catalog.as_bytes());
    }
    for project in projects {
        hash.update(&project.id.to_le_bytes());
        path(&mut hash, &project.root)?;
    }
    Ok(format!("{prefix}:{}", hash.finalize().to_hex()))
}

#[allow(clippy::needless_pass_by_value)] // Owned map_err adapter discards raw SQL diagnostics.
pub(super) fn sqlite_failure(error: rusqlite::Error) -> UnavailableReason {
    if let rusqlite::Error::SqliteFailure(code, _) = error {
        match code.extended_code & 0xff {
            rusqlite::ffi::SQLITE_BUSY | rusqlite::ffi::SQLITE_LOCKED => {
                return UnavailableReason::Busy;
            }
            rusqlite::ffi::SQLITE_TOOBIG | rusqlite::ffi::SQLITE_NOMEM => {
                return UnavailableReason::LimitExceeded;
            }
            _ => {}
        }
    }
    UnavailableReason::Io
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod project_tests;
