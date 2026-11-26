//! Seal a prepared pair and move a stopped registry's head in one commit.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::config::Config;
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::daemon::{lifecycle, registry};
use crate::error::Result;
use crate::index::{outcome, walk};
use crate::limits::MAX_STATE_BYTES;
use crate::platform::fs as native_fs;
use crate::store::models::{Project, ProjectStatus};
use crate::store::registry_inspect::{
    self, InspectionLimits, ObservedProjectIndex, ProjectIndexLayout, UnavailableReason,
};
use crate::store::settings::{DbSettings, DbSpec};
use crate::store::{
    DB_FILE, Db,
    generation_seal::{GenerationSeal, SEAL_FORMAT},
    migrations,
    project_index::{LOCAL_SCHEMA_VERSION, project_directory},
    repo,
};

use super::recovery::{
    directory_exists, open_recovery_registry, read_owner_record, sync_directory,
    validate_owned_files, verify_ownership,
};
use super::{
    LexicalBuildRequest, PreparedLexicalGeneration, admission, claim_path, filesystem,
    open_coordination_file, open_registry, ownership_digest, validate_catalog, validate_epoch,
    verify_base, verify_chunk_pair, with_preparation_lease,
};

/// Activates a complete lexical pair in a quiescent v5 maintenance registry.
///
/// The caller supplies the accepted build request and effective lexical
/// settings. This verifies the source survey again before changing the head.
/// A repeated call for the same still-active pair returns its identity.
///
/// # Errors
/// Stale bases, changed inputs, busy leases, unavailable directory sync,
/// invalid artifacts, and filesystem or SQLite errors prevent acceptance.
/// A catalog commit or flush error can leave the result indeterminate. Retry
/// with the same receipt. A stopped retry gives SQLite a chance to recover an
/// abandoned WAL or hot journal. It refuses if any sidecars remain.
pub fn publish_lexical_generation(
    cache: &Path,
    request: LexicalBuildRequest,
    staged: PreparedLexicalGeneration,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
) -> Result<ObservedProjectIndex> {
    if request.project_id <= 0
        || staged.project_id != request.project_id
        || staged.generation_id <= 0
        || staged.content_revision < 0
    {
        return Err(admission("invalid generation publication identity"));
    }
    registry_inspect::validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    config.validate()?;
    if config.hybrid || config.hybrid_cfg.enabled {
        return Err(admission(
            "lexical generation publication requires a lexical configuration",
        ));
    }
    let cache = filesystem(cache, "resolve cache", fs::canonicalize(cache))?;
    let initial_bytes = registry_inspect::validate(&cache, limits)?;
    registry_inspect::check_identity_budget(limits, initial_bytes, true)
        .map_err(registry_unavailable)?;
    let project_dir = project_directory(&cache, request.project_id);
    if !directory_exists(&project_dir)? {
        return Err(admission("generation project directory is missing"));
    }
    with_preparation_lease(&project_dir, || {
        publish_stopped(
            &cache,
            &project_dir,
            request,
            staged,
            settings,
            limits,
            config,
        )
    })
}

fn publish_stopped(
    cache: &Path,
    project_dir: &Path,
    request: LexicalBuildRequest,
    staged: PreparedLexicalGeneration,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
) -> Result<ObservedProjectIndex> {
    let dir = project_dir
        .join("generations")
        .join(staged.generation_id.to_string());
    if !directory_exists(&dir)? {
        return Err(admission("prepared generation directory is missing"));
    }
    sync_directory(&dir)?;
    let registry_path = cache.join(DB_FILE);
    filesystem(
        &registry_path,
        "inspect generation registry file",
        native_fs::check_single_link(&registry_path),
    )?;
    if registry_inspect::sidecars_absent(&registry_path) == Err(UnavailableReason::RecoveryRequired)
    {
        let registry = open_recovery_registry(cache, settings)?;
        configure_publication_connection(registry.connection(), limits)?;
        validate_epoch(registry.connection(), request.epoch)?;
        registry
            .conn
            .close()
            .map_err(|(_, err)| repo::db_error(err))?;
    }
    let (already_active, project) = registry_inspect::with_registry(
        cache,
        limits,
        || {},
        |conn, _| {
            Ok(
                candidate_is_active(conn, request, staged).and_then(|active| {
                    let project = registry::project_by_id_in(conn, request.project_id)?
                        .ok_or_else(|| admission("generation project is no longer registered"))?;
                    Ok((active, project))
                }),
            )
        },
    )
    .map_err(registry_unavailable)??;

    let current = ObservedProjectIndex {
        project_id: staged.project_id,
        epoch: Some(request.epoch),
        generation_id: Some(staged.generation_id),
        content_revision: staged.content_revision,
        layout: ProjectIndexLayout::Local,
    };
    if already_active {
        let metadata = open_local_metadata(&dir, settings, limits)?;
        verified_recipe(&metadata, &project, staged.content_revision, config)?;
        metadata
            .conn
            .close()
            .map_err(|(_, err)| repo::db_error(err))?;
        confirm_publication(cache, current, limits)?;
        return Ok(current);
    }

    verify_base(cache, request, settings, limits)?;
    let read_lock = dir.join(".read.lock");
    let seal = lifecycle::with_read_lock(
        open_coordination_file(&read_lock, false)?,
        &read_lock,
        || verify_and_seal(&dir, request, staged, &project, settings, limits, config),
    )?;
    let writer_lock = lifecycle::lease_path(cache, project.id);
    lifecycle::with_writer_lock(
        open_coordination_file(&writer_lock, false)?,
        &writer_lock,
        || commit_head(cache, request, staged, settings, limits, &project, &seal),
    )?;
    Ok(current)
}

fn confirm_publication(
    cache: &Path,
    expected: ObservedProjectIndex,
    limits: InspectionLimits,
) -> Result<()> {
    let initial_bytes = registry_inspect::validate(cache, limits)?;
    registry_inspect::with_registry_lease(
        cache,
        limits,
        || {},
        |locked, directory| {
            let observed = registry_inspect::observe_project(
                locked.connection(),
                directory,
                expected.project_id,
                limits,
                initial_bytes,
            )?;
            if observed != Some(expected) {
                return Err(UnavailableReason::Changed);
            }
            // Visibility after a failed commit does not prove durability. Flush
            // the leased file without closing a second descriptor to its inode.
            locked.sync_main()?;
            Ok(sync_directory(directory))
        },
    )
    .map_err(registry_unavailable)?
}

fn registry_unavailable(reason: UnavailableReason) -> crate::error::Error {
    admission(format!("generation registry is unavailable ({reason:?})"))
}

struct CandidateRow {
    phase: String,
    layout: String,
    base_id: Option<i64>,
    revision: i64,
    plan: Option<String>,
    reservation: i64,
    sealed: bool,
}

fn candidate_is_active(
    registry: &Connection,
    request: LexicalBuildRequest,
    staged: PreparedLexicalGeneration,
) -> Result<bool> {
    validate_epoch(registry, request.epoch)?;
    let record = registry.query_row(
        "SELECT phase, layout, base_id, base_revision, plan_id, reserved_disk_bytes, seal_digest IS NOT NULL
         FROM project_generations WHERE project_id = ?1 AND id = ?2",
        params![request.project_id, staged.generation_id],
        |r| Ok(CandidateRow {
            phase: r.get(0)?,
            layout: r.get(1)?,
            base_id: r.get(2)?,
            revision: r.get(3)?,
            plan: r.get(4)?,
            reservation: r.get(5)?,
            sealed: r.get(6)?,
        }),
    ).optional().map_err(repo::db_error)?
        .ok_or_else(|| admission("prepared generation is no longer reserved"))?;
    let plan = blake3::Hash::from_bytes(request.plan_digest).to_hex();
    if record.layout != "local"
        || record.base_id != request.base.and_then(|base| base.generation_id)
        || record.revision != request.base.map_or(0, |base| base.content_revision)
        || record.plan.as_deref() != Some(plan.as_str())
        || u64::try_from(record.reservation).ok() != Some(request.reserved_disk_bytes)
    {
        return Err(admission(
            "prepared generation does not match the accepted plan",
        ));
    }
    match (record.phase.as_str(), record.sealed) {
        ("active", true) => Ok(true),
        ("building", false) => Ok(false),
        _ => Err(admission("generation has no publishable reservation")),
    }
}

pub(super) fn verify_and_seal(
    dir: &Path,
    request: LexicalBuildRequest,
    staged: PreparedLexicalGeneration,
    project: &Project,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    config: &Config,
) -> Result<blake3::Hash> {
    let parent = dir
        .parent()
        .ok_or_else(|| admission("invalid generation directory"))?;
    let digest = ownership_digest(request, staged.generation_id);
    let intent = claim_path(parent, staged.generation_id);
    if read_owner_record(&intent)?.as_deref() != Some(digest.as_bytes()) {
        return Err(admission(
            "generation intent no longer matches the published build",
        ));
    }
    verify_ownership(dir, digest, false, false)?;
    validate_owned_files(dir)?;
    let metadata = open_local_metadata(dir, settings, limits)?;
    check_revision(
        &metadata,
        staged.project_id,
        staged.generation_id,
        staged.content_revision,
    )?;
    let recipe = verified_recipe(&metadata, project, staged.content_revision, config)?;
    verify_sources(&metadata, project, config)?;
    verify_chunk_pair(
        metadata.connection(),
        project.id,
        &dir.join("tantivy"),
        limits,
    )?;
    metadata
        .conn
        .close()
        .map_err(|(_, err)| repo::db_error(err))?;

    let (files, bytes) = flush_generation_files(dir, limits)?;
    if bytes.checked_add(digest.as_bytes().len() as u64) != Some(staged.bytes) {
        return Err(admission("prepared file sizes changed before publication"));
    }
    let seal = GenerationSeal {
        format: SEAL_FORMAT,
        installation_epoch: request.epoch,
        project_id: project.id,
        generation_id: staged.generation_id,
        content_revision: staged.content_revision,
        metadata_schema: LOCAL_SCHEMA_VERSION,
        base_id: request.base.and_then(|base| base.generation_id),
        base_revision: request.base.map_or(0, |base| base.content_revision),
        plan: blake3::Hash::from_bytes(request.plan_digest)
            .to_hex()
            .to_string(),
        index_recipe: recipe,
        files,
    };
    let encoded = serde_json::to_vec(&seal)
        .map_err(|err| crate::error::Error::internal_with_source("encode generation seal", err))?;
    if encoded.len() as u64 > MAX_STATE_BYTES
        || encoded.len() > limits.max_metadata_bytes
        || staged
            .bytes
            .checked_add(encoded.len() as u64)
            .is_none_or(|total| total > request.reserved_disk_bytes)
    {
        return Err(admission(
            "generation seal exceeds the accepted reservation",
        ));
    }
    let seal_path = dir.join("seal.json");
    write_seal(&seal_path, &encoded)?;
    sync_directory(dir)?;
    sync_directory(parent)?;
    Ok(blake3::hash(&encoded))
}

fn verified_recipe(
    metadata: &Db,
    project: &Project,
    revision: i64,
    config: &Config,
) -> Result<String> {
    let indexed = outcome::read(metadata, project.id)?
        .filter(outcome::IndexOutcome::is_complete)
        .ok_or_else(|| admission("prepared metadata has no complete source survey"))?;
    if indexed.canonical_root.as_str() != project.canonical_root.to_str().unwrap_or_default()
        || indexed.recipe_id != outcome::recipe_id(config)
        || indexed.index_generation != revision
    {
        return Err(admission("prepared generation uses different index inputs"));
    }
    Ok(indexed.recipe_id)
}

pub(super) fn open_local_metadata(
    dir: &Path,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Db> {
    let path = dir.join("metadata.db");
    registry_inspect::sidecars_absent(&path)
        .map_err(|reason| admission(format!("generation metadata unavailable ({reason:?})")))?;
    filesystem(
        &path,
        "inspect local generation metadata",
        native_fs::check_single_link(&path),
    )?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(path, flags).map_err(repo::db_error)?;
    configure_publication_connection(&conn, limits)?;
    super::use_memory_temp_store(&conn)?;
    Ok(Db {
        conn,
        cache_root: dir.to_path_buf(),
        settings: settings.clone(),
        generation_catalog: false,
    })
}

fn configure_publication_connection(
    connection: &Connection,
    limits: InspectionLimits,
) -> Result<()> {
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(repo::db_error)?;
    registry_inspect::configure_connection(connection, limits).map_err(repo::db_error)
}

fn check_revision(metadata: &Db, project: i64, generation: i64, expected: i64) -> Result<()> {
    let revision = registry_inspect::local_revision(metadata.connection(), project, generation)
        .map_err(|reason| {
            admission(format!(
                "local generation identity is unavailable ({reason:?})"
            ))
        })?;
    if revision != expected || repo::has_pending_docs(metadata.connection(), project)? {
        return Err(admission(
            "local generation revision or pending state changed",
        ));
    }
    Ok(())
}

fn verify_sources(metadata: &Db, project: &Project, config: &Config) -> Result<()> {
    let conn = metadata.connection();
    let root = &project.canonical_root;
    if filesystem(
        root,
        "resolve generation source root",
        fs::canonicalize(root),
    )? != *root
        || !root.is_dir()
    {
        return Err(admission("generation source root changed"));
    }
    let expected: i64 = conn
        .query_row(
            "SELECT count(*) FROM docs WHERE project_id = ?1",
            [project.id],
            |row| row.get(0),
        )
        .map_err(repo::db_error)?;
    let expected = usize::try_from(expected)
        .map_err(|_| admission("prepared source survey has an invalid document count"))?;
    if expected > config.max_docs_per_project {
        return Err(admission(
            "prepared source survey exceeds the current document limit",
        ));
    }
    let mut lookup = conn
        .prepare(
            "SELECT content_hash FROM docs
             WHERE project_id = ?1 AND rel_path = ?2 AND abs_path = ?3
                  AND length(CAST(content_hash AS BLOB)) = 64",
        )
        .map_err(repo::db_error)?;
    let mut observed = 0usize;
    for item in walk::walk(root, config)? {
        let path = item?;
        if observed == expected {
            return Err(admission(
                "source survey acquired new documents before publication",
            ));
        }
        let canonical = filesystem(&path, "resolve surveyed document", fs::canonicalize(&path))?;
        let relative = canonical
            .strip_prefix(root)
            .map(walk::portable_rel)
            .map_err(|_| admission("surveyed document left its registered root"))?;
        let absolute = path
            .to_str()
            .ok_or_else(|| admission("surveyed document path is not lossless UTF-8"))?;
        let hash: String = lookup
            .query_row(params![project.id, relative, absolute], |row| row.get(0))
            .optional()
            .map_err(repo::db_error)?
            .ok_or_else(|| admission("source path changed before generation publication"))?;
        let source = filesystem(
            &path,
            "open surveyed document",
            native_fs::open_readonly_nofollow(&path),
        )?;
        let attributes = filesystem(&path, "inspect surveyed file", source.metadata())?;
        if !native_fs::is_regular_nofollow(&attributes) || attributes.len() > config.max_file_size {
            return Err(admission("surveyed document exceeds its file policy"));
        }
        let mut hasher = blake3::Hasher::new();
        let mut bounded = source.take(config.max_file_size.saturating_add(1));
        let bytes = filesystem(
            &path,
            "hash surveyed document",
            io::copy(&mut bounded, &mut hasher),
        )?;
        if bytes > config.max_file_size || hash != hasher.finalize().to_hex().as_str() {
            return Err(admission(
                "source content changed before generation publication",
            ));
        }
        observed += 1;
    }
    if observed != expected {
        return Err(admission("source survey lost documents before publication"));
    }
    Ok(())
}

fn flush_generation_files(
    dir: &Path,
    limits: InspectionLimits,
) -> Result<(Vec<(String, u64)>, u64)> {
    let tantivy = dir.join("tantivy");
    let seal = dir.join("seal.json");
    let mut files = Vec::new();
    let mut tracked_bytes = 0usize;
    let mut bytes = 0u64;
    for folder in [dir, &tantivy] {
        for entry in filesystem(
            folder,
            "list generation files to seal",
            fs::read_dir(folder),
        )? {
            let path = filesystem(folder, "inspect generation component", entry)?.path();
            if path == tantivy || path == seal {
                continue;
            }
            let relative = path
                .strip_prefix(dir)
                .map(walk::portable_rel)
                .map_err(|_| admission("generation component left its staging directory"))?;
            tracked_bytes = tracked_bytes
                .checked_add(relative.len() + size_of::<(String, u64)>())
                .filter(|total| *total <= limits.max_metadata_bytes)
                .ok_or_else(|| admission("generation manifest exceeds the metadata limit"))?;
            let file = open_coordination_file(&path, false)?;
            let len = filesystem(&path, "inspect staged component length", file.metadata())?.len();
            filesystem(&path, "sync generation component", file.sync_all())?;
            bytes = bytes
                .checked_add(len)
                .ok_or_else(|| admission("generation file size overflow"))?;
            files.push((relative, len));
        }
    }
    files.sort_unstable();
    sync_directory(&tantivy)?;
    Ok((files, bytes))
}

fn write_seal(path: &Path, bytes: &[u8]) -> Result<()> {
    let file = match File::create_new(path) {
        Ok(mut file) => {
            filesystem(path, "write generation seal", file.write_all(bytes))?;
            file
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let mut found = Vec::new();
            let file = open_coordination_file(path, false)?;
            filesystem(
                path,
                "read existing generation seal",
                (&file).take(bytes.len() as u64 + 1).read_to_end(&mut found),
            )?;
            if found != bytes {
                return Err(admission(
                    "existing generation seal differs from the verified pair",
                ));
            }
            file
        }
        Err(error) => return filesystem(path, "create generation seal", Err(error)),
    };
    filesystem(path, "sync generation seal", file.sync_all())
}

fn commit_head(
    cache: &Path,
    request: LexicalBuildRequest,
    staged: PreparedLexicalGeneration,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    project: &Project,
    seal: &blake3::Hash,
) -> Result<()> {
    let mut registry = open_registry(cache, settings)?;
    let connection = registry.connection();
    configure_publication_connection(connection, limits)?;
    registry
        .connection()
        .pragma_update(None, "synchronous", "EXTRA")
        .map_err(repo::db_error)?;
    let tx = registry.begin_write()?;
    validate_catalog(&tx, request)?;
    let registered = registry::project_by_id_in(&tx, project.id)?
        .filter(|registered| registered.canonical_root == project.canonical_root)
        .ok_or_else(|| admission("project registration changed before generation publication"))?;
    match request.base.map(|base| base.layout) {
        None if registered.status != ProjectStatus::NotIndexed => {
            return Err(admission("project no longer accepts its first generation"));
        }
        Some(ProjectIndexLayout::Legacy)
            if registered.status != ProjectStatus::Indexed
                || registered.schema_version != migrations::PROJECT_SCHEMA_VERSION
                || repo::has_pending_docs(&tx, project.id)? =>
        {
            return Err(admission("legacy generation base is no longer complete"));
        }
        _ => {}
    }
    if candidate_is_active(&tx, request, staged)? {
        return Err(admission("prepared generation changed while publishing"));
    }
    if let Some(base) = request.base {
        let generation = base
            .generation_id
            .ok_or_else(|| admission("active pair has no generation ID"))?;
        if base.layout == ProjectIndexLayout::Local {
            let active = project_directory(cache, request.project_id)
                .join("generations")
                .join(generation.to_string());
            let metadata = open_local_metadata(&active, settings, limits)?;
            check_revision(
                &metadata,
                request.project_id,
                generation,
                base.content_revision,
            )?;
        }
        let retired = tx
            .execute(
                "UPDATE project_generations SET phase='retired'
             WHERE project_id=?1 AND id=?2 AND phase='active'",
                params![request.project_id, generation],
            )
            .map_err(repo::db_error)?;
        if retired != 1 {
            return Err(admission("base generation changed during publication"));
        }
    }
    let activated = tx
        .execute(
            "UPDATE project_generations SET phase='active', seal_digest=?3
         WHERE project_id=?1 AND id=?2 AND phase='building'",
            params![
                request.project_id,
                staged.generation_id,
                seal.as_bytes().as_slice()
            ],
        )
        .map_err(repo::db_error)?;
    if activated != 1 {
        return Err(admission("prepared generation is no longer publishable"));
    }
    tx.execute(
        "INSERT INTO project_generation_head (project_id, active_id)
         VALUES (?1, ?2) ON CONFLICT(project_id) DO UPDATE SET active_id=excluded.active_id",
        params![request.project_id, staged.generation_id],
    )
    .map_err(repo::db_error)?;
    tx.commit().map_err(|error| {
        crate::error::Error::internal_with_source(
            "commit generation catalog (publication result may be indeterminate)",
            error,
        )
    })?;
    registry
        .conn
        .close()
        .map_err(|(_, err)| repo::db_error(err))?;
    sync_directory(cache)
}
