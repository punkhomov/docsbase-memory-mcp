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
use crate::platform::fs as native_fs;

use super::project_index::{LOCAL_SCHEMA_SQL, LOCAL_SCHEMA_VERSION, project_directory};
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

mod publication;
pub use publication::publish_lexical_generation;

mod recovery;
pub use recovery::{discard_lexical_generation, find_lexical_build_to_discard};

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
    let reservation = i64::try_from(request.reserved_disk_bytes)
        .ok()
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| admission("invalid generation disk reservation"))?;
    if request.project_id <= 0 {
        return Err(admission("generation build requires a positive project ID"));
    }
    registry_inspect::validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    config.validate()?;
    let cache = filesystem(cache, "resolve cache", fs::canonicalize(cache))?;
    let project = probe_project(&cache, request, limits)?;
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
        // Keep the coordination path available if reservation commits
        // before the generation directory or its intent are created.
        #[cfg(unix)]
        for directory in [&project_dir, &cache.join("projects"), &cache] {
            recovery::sync_directory(directory)?;
        }
        if filesystem(
            &cache,
            "inspect free space",
            native_fs::available_space(&cache),
        )? < request.reserved_disk_bytes
        {
            return Err(admission(
                "insufficient free space for generation reservation",
            ));
        }
        let generations = project_dir.join("generations");
        ensure_directory(&generations)?;
        let base_lease = lifecycle::lease_path(&cache, project.id);
        let generation = lifecycle::with_read_lock(
            open_coordination_file(&base_lease, request.base.is_none())?,
            &base_lease,
            || {
                verify_base(&cache, request, settings, limits)?;
                reserve(
                    &cache,
                    request,
                    &project,
                    settings,
                    reservation,
                    &generations,
                )
            },
        )?;
        // The base lease ends before any index or merge work. Publication will
        // require a new freshness check under its own writer authority.

        let path = generations.join(generation.to_string());
        let digest = ownership_digest(request, generation);
        let intent = claim_path(&generations, generation);
        write_owner_record(&intent, digest)?;
        #[cfg(unix)]
        for directory in [&generations, &project_dir] {
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
        let revision = build_pair(&path, &project, generation, settings, config, limits)?;
        let bytes = size_of_pair(&path)?
            .checked_add(digest.as_bytes().len() as u64)
            .ok_or_else(|| admission("generation size overflow"))?;
        if bytes > request.reserved_disk_bytes {
            return Err(admission(
                "prepared generation exceeds its disk reservation",
            ));
        }
        Ok(PreparedLexicalGeneration {
            project_id: project.id,
            generation_id: generation,
            content_revision: revision,
            bytes,
        })
    })
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
    limits: InspectionLimits,
) -> Result<Project> {
    // The no-write VFS lease refuses recovery before reading the registry.
    registry_inspect::with_registry(
        cache,
        limits,
        || {},
        |connection, _| {
            Ok(validate_catalog(connection, request).and_then(|()| {
                registry::project_by_id_in(connection, request.project_id)?
                    .ok_or_else(|| admission("generation build project is not registered"))
            }))
        },
    )
    .map_err(|reason| admission(format!("generation registry is unavailable ({reason:?})")))?
}

fn verify_base(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<()> {
    match (
        inspect_project_index(cache, request.project_id, settings, limits)?,
        request.base,
    ) {
        (ProjectIndexObservation::Missing, None) => Ok(()),
        (ProjectIndexObservation::Present(observed), Some(base)) if observed == base => Ok(()),
        (ProjectIndexObservation::Unavailable(reason), _) => Err(admission(format!(
            "generation base is unavailable ({reason:?})"
        ))),
        _ => Err(admission("generation base changed before preparation")),
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
    request: LexicalBuildRequest,
    project: &Project,
    settings: &Resolved<DbSettings>,
    reservation: i64,
    generations: &Path,
) -> Result<i64> {
    let mut registry = open_registry(cache, settings)?;
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
    })
}

fn build_pair(
    path: &Path,
    project: &Project,
    generation: i64,
    settings: &Resolved<DbSettings>,
    config: &Config,
    limits: InspectionLimits,
) -> Result<i64> {
    let lease = path.join(".read.lock");
    filesystem(
        &lease,
        "create generation read lease",
        File::create_new(&lease),
    )?;
    let metadata = path.join("metadata.db");
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(&metadata, flags).map_err(repo::db_error)?;
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=ON;")
        .map_err(repo::db_error)?;
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
    };
    let mut index =
        IndexHandle::open_or_create_with_settings(&path.join("tantivy"), config.index_writer)?;
    let stats = job::run_full_lexical(&mut db, &mut index, project, config)?;
    index.finish()?;
    if stats.errors != 0 || repo::has_pending_docs(db.connection(), project.id)? {
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
    tx.pragma_update(None, "user_version", LOCAL_SCHEMA_VERSION)
        .map_err(repo::db_error)?;
    tx.commit().map_err(repo::db_error)?;
    verify_chunk_pair(db.connection(), project.id, &path.join("tantivy"), limits)?;
    db.conn
        .close()
        .map_err(|(_, error)| repo::db_error(error))?;
    registry_inspect::sidecars_absent(&metadata)
        .map_err(|reason| admission(format!("prepared metadata is unavailable ({reason:?})")))?;
    Ok(revision)
}

// The caller owns the inactive pair for both reads. The comparison covers
// every live stored Tantivy chunk, including duplicate IDs at equal counts.
fn verify_chunk_pair(
    metadata: &Connection,
    project: i64,
    tantivy: &Path,
    limits: InspectionLimits,
) -> Result<()> {
    registry_inspect::check_tantivy_files(tantivy).map_err(|reason| {
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
