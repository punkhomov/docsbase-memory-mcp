//! Construction of an inactive lexical generation in a v5 registry.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use fd_lock::RwLock;
use rusqlite::{Connection, OpenFlags, params};

use crate::config::Config;
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::{
    job,
    tantivy_index::{IndexHandle, ReadIndex, chunk_id},
};
use crate::platform::file_lease::ExclusiveFileLease;
use crate::platform::fs as native_fs;

use super::project_index::{
    LOCAL_REVISION_SCHEMA_SQL, LOCAL_SCHEMA_SQL, LOCAL_SCHEMA_VERSION, project_directory,
};
use super::registry_inspect::{
    self, InspectionLimits, ObservedProjectIndex, ProjectIndexLayout, ProjectIndexObservation,
    inspect_project_index,
};
use super::settings::{DbSettings, DbSpec};
use super::{
    DB_FILE, Db, generation_catalog, migrations,
    models::{Project, ProjectStatus},
    repo,
};

mod catalog_access;
use catalog_access::CatalogAccess;
use catalog_access::observe_index_base;
pub(crate) use catalog_access::{observe_replacement_base, open_index_registry};

mod interrupted;
use interrupted::InterruptedBase;

mod publication;
pub use publication::publish_lexical_generation;

mod recovery;
mod retirement;
pub(super) use recovery::sync_directory;
pub use recovery::{
    discard_lexical_generation, find_lexical_build_to_discard, reclaim_retired_lexical_generation,
};

/// Accepted inputs to one lexical generation build.
#[derive(Debug, Clone, Copy)]
pub struct LexicalBuildRequest {
    /// Registered numeric project ID.
    pub project_id: i64,
    /// Accepted v5 installation identity.
    pub epoch: [u8; 16],
    /// Last observed active pair, or absence for a newly registered project.
    pub base: Option<ObservedProjectIndex>,
    /// Non-secret digest of the caller's approved change plan.
    pub plan_digest: [u8; 32],
    /// Estimated disk reservation, in bytes.
    pub reserved_disk_bytes: u64,
}

/// Identity of a completed build that is still inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedLexicalGeneration {
    /// Registered project captured from the v5 registry.
    pub project_id: i64,
    /// Allocated generation identifier.
    pub generation_id: i64,
    /// Content revision committed by the local index job.
    pub content_revision: i64,
    /// Logical bytes used by the completed files.
    pub bytes: u64,
}

/// Builds a private lexical pair from the Markdown sources in a quiescent v5 cache.
/// The reservation remains in `building` until sealing or recovery owns it.
/// The active index is never opened for writing by this operation.
///
/// # Errors
/// Returns admission, filesystem, database or incomplete indexing errors.
pub fn prepare_lexical_generation(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
) -> Result<PreparedLexicalGeneration> {
    prepare_lexical_generation_with_space(
        cache,
        request,
        settings,
        limits,
        config,
        native_fs::available_space,
    )
}

fn prepare_lexical_generation_with_space(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
    available_space: impl FnOnce(&Path) -> io::Result<u64>,
) -> Result<PreparedLexicalGeneration> {
    prepare_and_then(
        cache,
        BuildInputs {
            request,
            settings,
            limits,
            config,
        },
        available_space,
        BuildPurpose::Maintenance,
        |pair| Ok(pair.publication.staged),
    )
}

/// A staged pair under its project's preparation lease.
pub(crate) struct PreparedLexicalPublication<'a> {
    publication: publication::StagedPublication<'a>,
    outcome: crate::index::outcome::IndexOutcome,
}

impl PreparedLexicalPublication<'_> {
    pub(crate) const fn outcome(&self) -> &crate::index::outcome::IndexOutcome {
        &self.outcome
    }

    pub(crate) fn publish(
        self,
        finalize: impl FnOnce(publication::LexicalCommit<'_>) -> Result<ObservedProjectIndex>,
    ) -> Result<job::JobStats> {
        publication::publish_prepared(self.publication, finalize)?;
        Ok(self.outcome.stats)
    }
}

pub(crate) fn with_prepared_lexical_generation<T>(
    cache: &Path,
    build: OperationalBuild,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
    capture: &mut job::CaptureBudget<'_>,
    complete: impl FnOnce(PreparedLexicalPublication<'_>) -> Result<T>,
) -> Result<T> {
    prepare_and_then(
        cache,
        BuildInputs {
            request: build.request,
            settings,
            limits,
            config,
        },
        native_fs::available_space,
        BuildPurpose::DirectIndex {
            capture,
            interrupted: build.interrupted,
        },
        complete,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct OperationalBuild {
    pub(crate) request: LexicalBuildRequest,
    pub(crate) interrupted: Option<InterruptedBase>,
}

#[derive(Clone, Copy)]
struct BuildInputs<'a> {
    request: LexicalBuildRequest,
    settings: &'a Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &'a Config,
}

enum BuildPurpose<'a, 'b> {
    Maintenance,
    DirectIndex {
        capture: &'a mut job::CaptureBudget<'b>,
        interrupted: Option<InterruptedBase>,
    },
}

impl BuildPurpose<'_, '_> {
    const fn access(&self) -> CatalogAccess {
        match self {
            Self::Maintenance => CatalogAccess::Stopped,
            Self::DirectIndex {
                interrupted: None, ..
            } => CatalogAccess::Operational,
            Self::DirectIndex {
                interrupted: Some(base),
                ..
            } => CatalogAccess::Repair(*base),
        }
    }
}

fn prepare_and_then<T>(
    cache: &Path,
    inputs: BuildInputs<'_>,
    available_space: impl FnOnce(&Path) -> io::Result<u64>,
    purpose: BuildPurpose<'_, '_>,
    complete: impl FnOnce(PreparedLexicalPublication<'_>) -> Result<T>,
) -> Result<T> {
    let BuildInputs {
        request,
        settings,
        limits,
        config,
    } = inputs;
    let reservation = i64::try_from(request.reserved_disk_bytes)
        .ok()
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| admission("invalid generation disk reservation"))?;
    let pair_budget = request
        .reserved_disk_bytes
        .checked_sub(2 * blake3::OUT_LEN as u64)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| admission("generation disk reservation cannot cover ownership records"))?;
    if request.project_id <= 0 {
        return Err(admission("generation build requires a positive project ID"));
    }
    registry_inspect::validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    config.validate()?;
    let cache = filesystem(cache, "resolve cache", fs::canonicalize(cache))?;
    let access = purpose.access();
    let project = probe_project(&cache, request, settings, limits, access)?;
    let source = &project.canonical_root;
    if !source.is_absolute()
        || filesystem(source, "resolve registered root", fs::canonicalize(source))? != *source
        || !source.is_dir()
    {
        return Err(admission(
            "registered project root no longer names its canonical directory",
        ));
    }
    let project_dir = project_directory(&cache, project.id);
    ensure_directory(&cache.join("projects"))?;
    ensure_directory(&project_dir)?;
    with_preparation_lease(&project_dir, || {
        let direct = access != CatalogAccess::Stopped;
        let capture = match purpose {
            BuildPurpose::Maintenance => None,
            BuildPurpose::DirectIndex { capture, .. } => {
                capture.operation.check()?;
                // The preparation lease excludes an active builder. Reclaim
                // only this command's old attempt, under its own reader lease.
                recovery::discard_index_attempt(&cache, &project_dir, request, settings, limits)?;
                recovery::reclaim_index_pairs(&cache, &project_dir, request, settings, limits);
                capture.operation.check()?;
                Some(capture)
            }
        };
        // Keep the coordination path available if reservation commits
        // before the generation directory or its intent are created.
        #[cfg(unix)]
        for directory in [&project_dir, &cache.join("projects"), &cache] {
            recovery::sync_directory(directory)?;
        }
        let generations = project_dir.join("generations");
        ensure_directory(&generations)?;
        let base_lease = lifecycle::lease_path(&cache, project.id);
        let generation = lifecycle::with_read_lock(
            open_coordination_file(&base_lease, request.base.is_none())?,
            &base_lease,
            || {
                verify_base(&cache, request, settings, limits, access)?;
                reserve(
                    &cache,
                    inputs,
                    &project,
                    access,
                    reservation,
                    &generations,
                    available_space,
                )
            },
        )?;
        // The base lease ends before any index or merge work. Publication will
        // require a new freshness check under its own writer authority.

        let (staged, outcome) = build_reserved_pair(
            &project_dir,
            &generations,
            &project,
            generation,
            inputs,
            pair_budget,
            capture,
        )?;
        let result = complete(PreparedLexicalPublication {
            publication: publication::StagedPublication {
                cache: &cache,
                project_dir: &project_dir,
                request,
                staged,
                settings,
                limits,
                config,
                access,
            },
            outcome,
        })?;
        if direct {
            recovery::reclaim_index_pairs(&cache, &project_dir, request, settings, limits);
        }
        Ok(result)
    })
}

fn build_reserved_pair(
    #[cfg_attr(not(unix), allow(unused_variables))] project_dir: &Path,
    generations: &Path,
    project: &Project,
    generation: i64,
    inputs: BuildInputs<'_>,
    pair_budget: u64,
    capture: Option<&mut job::CaptureBudget<'_>>,
) -> Result<(
    PreparedLexicalGeneration,
    crate::index::outcome::IndexOutcome,
)> {
    let request = inputs.request;
    let path = generations.join(generation.to_string());
    let digest = ownership_digest(request, generation);
    let intent = claim_path(generations, generation);
    write_owner_record(&intent, digest)?;
    #[cfg(unix)]
    for directory in [generations, project_dir] {
        recovery::sync_directory(directory)?;
    }
    filesystem(
        &path,
        "create generation directory",
        native_fs::create_private_directory(&path),
    )?;
    write_owner_record(&path.join(".build-owner"), digest)?;
    #[cfg(unix)]
    recovery::sync_directory(&path)?;
    let (revision, outcome) = build_pair(&path, project, generation, inputs, pair_budget, capture)?;
    let bytes = size_of_pair(&path)?
        .checked_add(digest.as_bytes().len() as u64)
        .ok_or_else(|| admission("generation size overflow"))?;
    if bytes > request.reserved_disk_bytes {
        return Err(admission(
            "prepared generation exceeds its disk reservation",
        ));
    }
    Ok((
        PreparedLexicalGeneration {
            project_id: project.id,
            generation_id: generation,
            content_revision: revision,
            bytes,
        },
        outcome,
    ))
}

fn with_preparation_lease<T>(project_dir: &Path, run: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = project_dir.join(".prepare.lock");
    let mut lock = RwLock::new(open_coordination_file(&lock_path, true)?);
    let _guard = lock.try_write().map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            admission("another builder holds the project preparation lease")
        } else {
            Error::internal_with_source("lock project generation preparation", error)
        }
    })?;
    run()
}

fn probe_project(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    access: CatalogAccess,
) -> Result<Project> {
    access.read(cache, settings, limits, |connection| {
        validate_catalog(connection, request)?;
        registry::project_by_id_in(connection, request.project_id)?
            .ok_or_else(|| admission("generation build project is not registered"))
    })
}

fn verify_base(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    access: CatalogAccess,
) -> Result<()> {
    let observed = match access {
        CatalogAccess::Operational => {
            observe_index_base(cache, request.project_id, request.epoch, settings, limits)?
        }
        CatalogAccess::Repair(base) => {
            if request.base != Some(base.index) {
                return Err(admission(
                    "generation rebuild base disagrees with its mutation intent",
                ));
            }
            let lock = lifecycle::lease_path(cache, request.project_id);
            lifecycle::with_read_lock(open_coordination_file(&lock, false)?, &lock, || {
                access.read(cache, settings, limits, |conn| {
                    interrupted::recheck(conn, cache, base, settings, limits)
                })
            })?;
            Some(base.index)
        }
        CatalogAccess::Stopped => {
            match inspect_project_index(cache, request.project_id, settings, limits)? {
                ProjectIndexObservation::Missing => None,
                ProjectIndexObservation::Present(observed) => Some(observed),
                ProjectIndexObservation::Unavailable(reason) => {
                    return Err(admission(format!(
                        "generation base is unavailable ({reason:?})"
                    )));
                }
            }
        }
    };
    if observed == request.base {
        Ok(())
    } else {
        Err(admission("generation base changed before preparation"))
    }
}

fn validate_epoch(connection: &Connection, expected: [u8; 16]) -> Result<()> {
    if migrations::read_version(connection)? != generation_catalog::SCHEMA_VERSION {
        return Err(admission("generation build requires registry schema v5"));
    }
    let epoch = generation_catalog::read_epoch(connection)
        .map_err(|reason| admission(format!("invalid generation catalog ({reason:?})")))?;
    if epoch != expected {
        return Err(admission("generation installation epoch changed"));
    }
    Ok(())
}

fn validate_catalog(connection: &Connection, request: LexicalBuildRequest) -> Result<()> {
    let epoch = request.epoch;
    validate_epoch(connection, epoch)?;
    let head = generation_catalog::active_head(connection, request.project_id)
        .map_err(|reason| admission(format!("invalid project generation head ({reason:?})")))?;
    match (head, request.base) {
        (None, None) => Ok(()),
        (Some(head), Some(base))
            if base.epoch == Some(epoch)
                && base.generation_id == Some(head.id)
                && base.project_id == head.project_id
                && base.layout
                    == match head.layout {
                        generation_catalog::Layout::Legacy => ProjectIndexLayout::Legacy,
                        generation_catalog::Layout::Local { .. } => ProjectIndexLayout::Local,
                    } =>
        {
            if matches!(head.layout, generation_catalog::Layout::Legacy)
                && base.content_revision != repo::index_generation(connection, request.project_id)?
            {
                return Err(admission("generation base revision changed"));
            }
            Ok(())
        }
        _ => Err(admission("generation base head changed")),
    }
}

fn reserve(
    cache: &Path,
    inputs: BuildInputs<'_>,
    project: &Project,
    access: CatalogAccess,
    reservation: i64,
    generations: &Path,
    available_space: impl FnOnce(&Path) -> io::Result<u64>,
) -> Result<i64> {
    let BuildInputs {
        request, settings, ..
    } = inputs;
    let mut registry = access.writer(cache, settings)?;
    // Project registration and the head comparison are bound to the same
    // IMMEDIATE transaction as the new reservation.
    let tx = registry.begin_write()?;
    validate_catalog(&tx, request)?;
    let (root, status): (String, String) = tx
        .query_row(
            "SELECT canonical_root, status FROM projects WHERE id = ?1",
            [project.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(repo::db_error)?;
    if Path::new(&root) != project.canonical_root
        || (request.base.is_none() && status != ProjectStatus::NotIndexed.as_str())
    {
        return Err(admission(
            "project registration changed before generation preparation",
        ));
    }
    let building: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM project_generations WHERE project_id = ?1
            AND (phase IN ('building', 'sealed') OR (phase = 'reclaiming' AND layout = 'local' AND seal_digest IS NULL))
        )",
        [project.id],
        |row| row.get(0),
    ).map_err(repo::db_error)?;
    if building {
        return Err(admission(
            "a staged generation or pending discard already exists for this project",
        ));
    }
    // Compare all unfinished reservations and the new request under the
    // transaction that records the claim. Retired/active bytes are already
    // reflected in free space, while incomplete discard keeps its charge.
    let total = reserved_disk_bytes(&tx, request.reserved_disk_bytes)?;
    let free = filesystem(
        generations,
        "inspect free space",
        available_space(generations),
    )?;
    if total > free {
        return Err(admission(format!(
            "insufficient free space for all generation reservations ({total} bytes required, {free} available)"
        )));
    }
    let generation: i64 = tx
        .query_row(
            "INSERT INTO project_generations
                (project_id, layout, phase, base_id, base_revision, plan_id, reserved_disk_bytes)
             VALUES (?1, 'local', 'building', ?2, ?3, ?4, ?5)
             RETURNING id",
            params![
                project.id,
                request.base.and_then(|base| base.generation_id),
                request.base.map_or(0, |base| base.content_revision),
                blake3::Hash::from_bytes(request.plan_digest)
                    .to_hex()
                    .as_str(),
                reservation
            ],
            |row| row.get(0),
        )
        .map_err(repo::db_error)?;
    // Check both names before committing a reservation. If another entry
    // already uses the ID, commit the sequence advance without its row.
    let path = generations.join(generation.to_string());
    let intent = claim_path(generations, generation);
    if entry_exists(&path)? || entry_exists(&intent)? {
        tx.execute(
            "DELETE FROM project_generations WHERE id = ?1",
            [generation],
        )
        .map_err(repo::db_error)?;
        tx.commit().map_err(repo::db_error)?;
        return Err(admission(
            "generation path already belongs to another entry",
        ));
    }
    tx.commit().map_err(repo::db_error)?;
    Ok(generation)
}

fn reserved_disk_bytes(registry: &rusqlite::Transaction<'_>, requested: u64) -> Result<u64> {
    let mut statement = registry
        .prepare(
            "SELECT reserved_disk_bytes FROM project_generations
             WHERE layout = 'local' AND phase IN ('building', 'sealed', 'reclaiming')",
        )
        .map_err(repo::db_error)?;
    let mut reserved = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(repo::db_error)?;
    reserved.try_fold(requested, |total, bytes| {
        u64::try_from(bytes.map_err(repo::db_error)?)
            .ok()
            .and_then(|bytes| total.checked_add(bytes))
            .ok_or_else(|| admission("generation reservations exceed the size range"))
    })
}

fn entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => filesystem(path, "inspect reserved generation name", Err(error)),
    }
}

fn open_registry(cache: &Path, settings: &Resolved<DbSettings>) -> Result<Db> {
    let path = cache.join(DB_FILE);
    registry_inspect::sidecars_absent(&path)
        .map_err(|reason| admission(format!("generation registry is unavailable ({reason:?})")))?;
    open_registry_file(cache, &path, settings)
}

fn open_registry_file(cache: &Path, path: &Path, settings: &Resolved<DbSettings>) -> Result<Db> {
    filesystem(
        path,
        "inspect generation registry file",
        native_fs::check_single_link(path),
    )?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(path, flags).map_err(repo::db_error)?;
    conn.busy_timeout(settings.settings().write_busy_timeout)
        .map_err(repo::db_error)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(repo::db_error)?;
    Ok(Db {
        conn,
        cache_root: cache.to_owned(),
        settings: settings.clone(),
        generation_catalog: true,
    })
}

fn build_pair(
    path: &Path,
    project: &Project,
    generation: i64,
    inputs: BuildInputs<'_>,
    bytes: u64,
    capture: Option<&mut job::CaptureBudget<'_>>,
) -> Result<(i64, crate::index::outcome::IndexOutcome)> {
    let BuildInputs {
        settings,
        limits,
        config,
        ..
    } = inputs;
    let lease_path = path.join(".read.lock");
    let lease_file = filesystem(
        &lease_path,
        "create generation read lease",
        File::create_new(&lease_path),
    )?;
    let lease = filesystem(
        &lease_path,
        "lock staged generation",
        ExclusiveFileLease::try_acquire(lease_file),
    )?;
    let metadata = path.join("metadata.db");
    let (revision, prepared) =
        build_metadata(path, project, generation, settings, config, bytes, capture)?;
    let metadata_bytes = filesystem(
        &metadata,
        "measure staged metadata",
        fs::metadata(&metadata),
    )?
    .len();
    let tantivy_bytes = bytes
        .checked_sub(metadata_bytes)
        .ok_or_else(|| admission("staged metadata exceeds its disk reservation"))?;
    let tantivy = path.join("tantivy");
    IndexHandle::build_staged(
        &tantivy,
        config.index_writer,
        tantivy_bytes,
        lease,
        |writer| prepared.write_lexical_chunks(writer),
    )?;
    drop(prepared);
    let db = publication::open_local_metadata(path, settings, limits)?;
    verify_chunk_pair(db.connection(), project.id, &tantivy, limits)?;
    let outcome = crate::index::outcome::read(&db, project.id)?
        .filter(|outcome| outcome.is_complete() && outcome.index_generation == revision)
        .ok_or_else(|| admission("staged lexical outcome is incomplete"))?;
    db.conn
        .close()
        .map_err(|(_, error)| repo::db_error(error))?;
    Ok((revision, outcome))
}

fn build_metadata(
    path: &Path,
    project: &Project,
    generation: i64,
    settings: &Resolved<DbSettings>,
    config: &Config,
    bytes: u64,
    capture: Option<&mut job::CaptureBudget<'_>>,
) -> Result<(i64, job::PreparedPlan)> {
    let metadata = path.join("metadata.db");
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(&metadata, flags).map_err(repo::db_error)?;
    bound_metadata_writes(&conn, bytes)?;
    migrations::migrate(&conn)?;
    conn.execute(
        "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
         VALUES (?1, ?2, ?3, 'not_indexed', ?4, ?5)",
        params![
            project.id,
            project.canonical_root.to_str(),
            project.name,
            migrations::PROJECT_SCHEMA_VERSION,
            project.created_at
        ],
    )
    .map_err(repo::db_error)?;
    let mut db = Db {
        conn,
        cache_root: path.to_owned(),
        settings: settings.clone(),
        generation_catalog: false,
    };
    let prepared = job::prepare_staged_metadata(&mut db, project, config, capture)?;
    if prepared.stats.errors != 0 || repo::has_pending_docs(db.connection(), project.id)? {
        return Err(admission(
            "lexical generation has an incomplete source survey",
        ));
    }
    let tx = db.begin_write()?;
    let revision = repo::index_generation(&tx, project.id)?;
    tx.execute_batch(LOCAL_SCHEMA_SQL).map_err(repo::db_error)?;
    tx.execute(
        "INSERT INTO generation_state VALUES (1, ?1, ?2, ?3)",
        params![project.id, generation, revision],
    )
    .map_err(repo::db_error)?;
    tx.execute_batch(LOCAL_REVISION_SCHEMA_SQL)
        .map_err(repo::db_error)?;
    tx.pragma_update(None, "user_version", LOCAL_SCHEMA_VERSION)
        .map_err(repo::db_error)?;
    tx.commit().map_err(repo::db_error)?;
    db.conn
        .close()
        .map_err(|(_, error)| repo::db_error(error))?;
    registry_inspect::sidecars_absent(&metadata)
        .map_err(|reason| admission(format!("prepared metadata is unavailable ({reason:?})")))?;
    Ok((revision, prepared))
}

fn bound_metadata_writes(connection: &Connection, bytes: u64) -> Result<()> {
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(repo::db_error)?;
    let pages = i64::try_from(bytes)
        .ok()
        .and_then(|bytes| bytes.checked_div(page_size))
        .filter(|pages| *pages > 0)
        .ok_or_else(|| admission("generation reservation cannot cover a SQLite page"))?;
    let mut applied = None;
    connection
        .pragma(None, "max_page_count", pages, |row| {
            applied = Some(row.get::<_, i64>(0)?);
            Ok(())
        })
        .map_err(repo::db_error)?;
    if !applied.is_some_and(|actual| (1..=pages).contains(&actual)) {
        return Err(admission("SQLite could not enforce the staged page limit"));
    }
    // This database has no reader or prior state to recover. On any failure
    // the whole generation is discarded. Keep journals and sort workspace
    // off disk so the main-file page limit covers its writes.
    let journal: String = connection
        .query_row("PRAGMA journal_mode=OFF", [], |row| row.get(0))
        .map_err(repo::db_error)?;
    if !journal.eq_ignore_ascii_case("off") {
        return Err(admission(
            "SQLite could not disable journaling for private staging",
        ));
    }
    use_memory_temp_store(connection)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(repo::db_error)
}

fn use_memory_temp_store(connection: &Connection) -> Result<()> {
    let forces_disk: bool = connection
        .query_row(
            "SELECT sqlite_compileoption_used('TEMP_STORE=0')",
            [],
            |row| row.get(0),
        )
        .map_err(repo::db_error)?;
    if forces_disk {
        return Err(admission(
            "SQLite build requires disk-backed temporary files",
        ));
    }
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(repo::db_error)
}

// The caller owns the inactive pair for both reads. The comparison covers
// every live stored Tantivy chunk, including duplicate IDs at equal counts.
fn verify_chunk_pair(
    metadata: &Connection,
    project: i64,
    tantivy: &Path,
    limits: InspectionLimits,
) -> Result<()> {
    registry_inspect::check_tantivy_files(tantivy, None).map_err(|reason| {
        admission(format!("generation Tantivy files unavailable ({reason:?})"))
    })?;
    let chunk_count: i64 = metadata
        .query_row(
            "SELECT count(*) FROM chunks c JOIN docs d ON d.id = c.doc_id WHERE d.project_id = ?1",
            [project],
            |row| row.get(0),
        )
        .map_err(repo::db_error)?;
    // Both buffers stay live during comparison. Reserve only the rows that
    // exist, under the caller's metadata limit.
    let count = usize::try_from(chunk_count)
        .ok()
        .filter(|count| {
            *count <= limits.max_metadata_bytes / (size_of::<u64>() + size_of::<bool>())
        })
        .ok_or_else(|| admission("staged chunk ID verification exceeds the metadata budget"))?;
    let mut expected = Vec::new();
    expected
        .try_reserve_exact(count)
        .map_err(|error| Error::internal_with_source("allocate staged chunk IDs", error))?;
    let mut query = metadata
        .prepare(
            "SELECT c.doc_id, c.seq FROM chunks c JOIN docs d ON d.id = c.doc_id
         WHERE d.project_id = ?1 ORDER BY c.doc_id, c.seq",
        )
        .map_err(repo::db_error)?;
    let mut rows = query.query([project]).map_err(repo::db_error)?;
    while let Some(row) = rows.next().map_err(repo::db_error)? {
        let doc: u32 = row.get(0).map_err(repo::db_error)?;
        let seq: u32 = row.get(1).map_err(repo::db_error)?;
        if doc == 0 || expected.len() == count {
            return Err(admission("invalid staged SQLite chunk IDs"));
        }
        expected.push(chunk_id(i64::from(doc), seq));
    }
    if expected.len() != count {
        return Err(admission(
            "staged SQLite chunk IDs changed during verification",
        ));
    }
    ReadIndex::verify_chunk_ids(tantivy, &expected)
}

fn claim_path(parent: &Path, generation: i64) -> std::path::PathBuf {
    parent.join(format!(".build-{generation}.intent"))
}

fn write_owner_record(marker: &Path, digest: blake3::Hash) -> Result<()> {
    let mut file = filesystem(
        marker,
        "create generation ownership record",
        File::create_new(marker),
    )?;
    filesystem(
        marker,
        "write generation ownership record",
        file.write_all(digest.as_bytes()),
    )?;
    filesystem(marker, "flush generation ownership record", file.sync_all())
}

fn ownership_digest(request: LexicalBuildRequest, generation: i64) -> blake3::Hash {
    let mut hash = blake3::Hasher::new_derive_key("docsbase.index.generation.owner.v1");
    hash.update(&request.epoch);
    hash.update(&request.project_id.to_le_bytes());
    hash.update(&generation.to_le_bytes());
    hash.update(&request.plan_digest);
    hash.finalize()
}

fn ensure_directory(path: &Path) -> Result<()> {
    match native_fs::create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let physical = filesystem(path, "resolve generation parent", fs::canonicalize(path))?;
            if physical != path || !path.is_dir() {
                return Err(admission("generation parent is not an owned directory"));
            }
            Ok(())
        }
        Err(error) => filesystem(path, "create generation parent", Err(error)),
    }
}

fn open_coordination_file(path: &Path, create: bool) -> Result<File> {
    #[cfg(unix)]
    match fs::symlink_metadata(path) {
        Ok(_) => filesystem(
            path,
            "inspect coordination file",
            native_fs::check_single_link(path),
        )?,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return filesystem(path, "inspect coordination file", Err(error)),
    }
    let opened = if create {
        native_fs::open_private_lock(path)
    } else {
        native_fs::open_readonly_nofollow(path)
    };
    let file = filesystem(path, "open generation coordination file", opened)?;
    let metadata = filesystem(path, "inspect coordination file", file.metadata())?;
    if !native_fs::is_regular_nofollow(&metadata)
        || filesystem(
            path,
            "inspect coordination file link count",
            native_fs::file_identity_and_links(&file, &metadata),
        )?
        .1 != 1
    {
        return Err(admission(
            "generation coordination file is not an ordinary single-link file",
        ));
    }
    Ok(file)
}

fn size_of_pair(path: &Path) -> Result<u64> {
    let mut size = 0u64;
    let tantivy = path.join("tantivy");
    for directory in [path, &tantivy] {
        for entry in filesystem(directory, "list staged files", fs::read_dir(directory))? {
            let entry = filesystem(directory, "list staged file", entry)?;
            let path = entry.path();
            if path == tantivy {
                continue;
            }
            filesystem(
                &path,
                "inspect staged file",
                native_fs::check_single_link(&path),
            )?;
            let metadata = filesystem(&path, "measure staged file", entry.metadata())?;
            size = size
                .checked_add(metadata.len())
                .ok_or_else(|| admission("generation size overflow"))?;
        }
    }
    Ok(size)
}

fn filesystem<T>(path: &Path, action: &str, result: io::Result<T>) -> Result<T> {
    result
        .map_err(|error| Error::internal_with_source(format!("{action} {}", path.display()), error))
}

fn admission(message: impl Into<String>) -> Error {
    Error::Admission {
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "generation_build_tests.rs"]
mod tests;
