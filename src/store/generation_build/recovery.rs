//! Retryable discard of a private build with a matching ownership record.

use std::io::Read as _;

use rusqlite::OptionalExtension;

use crate::index::tantivy_index::{REBUILD_MARKER, TOKENIZER_VERSION_FILE};

use super::{
    Capabilities, DB_FILE, DbSettings, DbSpec, InspectionLimits, LexicalBuildRequest, Path,
    Resolved, Result, RwLock, SettingsSpec, admission, claim_path, filesystem, fs, io, native_fs,
    open_coordination_file, open_registry_file, ownership_digest, params, project_directory,
    registry_inspect, repo, validate_epoch, with_preparation_lease,
};

/// Finds a catalogue record for the supplied plan after an interrupted build.
/// SQLite may recover an abandoned journal while checking the quiescent cache.
///
/// # Errors
/// Invalid project, epoch, limits, pending work or ambiguous matching records.
pub fn find_lexical_build_to_discard(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Option<i64>> {
    if request.project_id <= 0 {
        return Err(admission(
            "generation recovery requires a positive project ID",
        ));
    }
    registry_inspect::validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    let cache = filesystem(cache, "resolve cache", fs::canonicalize(cache))?;
    let project = project_directory(&cache, request.project_id);
    if !directory_exists(&project)? {
        return Ok(None);
    }
    with_preparation_lease(&project, || {
        let db = open_recovery_registry(&cache, settings)?;
        validate_epoch(db.connection(), request.epoch)?;
        let plan = blake3::Hash::from_bytes(request.plan_digest).to_hex();
        let mut query = db
            .connection()
            .prepare(
                "SELECT id FROM project_generations WHERE project_id = ?1 AND plan_id = ?2
               AND layout = 'local' AND phase IN ('building', 'reclaiming')
               AND seal_digest IS NULL LIMIT 2",
            )
            .map_err(repo::db_error)?;
        let mut rows = query
            .query(params![request.project_id, plan.as_str()])
            .map_err(repo::db_error)?;
        let Some(row) = rows.next().map_err(repo::db_error)? else {
            return Ok(None);
        };
        let id = row.get(0).map_err(repo::db_error)?;
        if rows.next().map_err(repo::db_error)?.is_some() {
            return Err(admission(
                "the accepted plan matches multiple incomplete generations",
            ));
        }
        Ok(Some(id))
    })
}

/// Discards an inactive attempt with the exact project, generation and plan identity.
///
/// The reservation remains charged until deletion passes the filesystem's
/// namespace sync. Repeating a completed discard succeeds.
/// # Errors
/// Refuses a busy lease, unsupported namespace sync, or unverified ownership.
/// Filesystem or database failures can leave a discard pending for retry.
pub fn discard_lexical_generation(
    cache: &Path,
    request: LexicalBuildRequest,
    generation: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<()> {
    if request.project_id <= 0 || generation <= 0 {
        return Err(admission(
            "discard requires positive project and generation IDs",
        ));
    }
    registry_inspect::validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    let cache = filesystem(cache, "resolve cache", fs::canonicalize(cache))?;
    let project = project_directory(&cache, request.project_id);
    if !directory_exists(&project)? {
        return Err(admission("project preparation directory is missing"));
    }
    with_preparation_lease(&project, || {
        let generations = project.join("generations");
        let parent = if directory_exists(&generations)? {
            generations.as_path()
        } else {
            project.as_path()
        };
        let path = generations.join(generation.to_string());
        let exists = directory_exists(&path)?;
        let plan = blake3::Hash::from_bytes(request.plan_digest).to_hex();
        let mut db = open_recovery_registry(&cache, settings)?;
        validate_epoch(db.connection(), request.epoch)?;
        let state: Option<(String, String, Option<String>, bool)> = db
            .connection()
            .query_row(
                "SELECT phase, layout, plan_id, seal_digest IS NULL FROM project_generations
             WHERE project_id = ?1 AND id = ?2",
                params![request.project_id, generation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(repo::db_error)?;
        let Some((phase, layout, stored_plan, unsealed)) = state else {
            return if exists {
                Err(admission(
                    "discard found an uncatalogued generation directory",
                ))
            } else {
                Ok(())
            };
        };
        if !matches!(phase.as_str(), "building" | "reclaiming")
            || layout != "local"
            || stored_plan.as_deref() != Some(plan.as_str())
            || !unsealed
        {
            return Err(admission("generation is not the requested inactive build"));
        }

        // Confirm the required barrier before changing either durable state.
        sync_directory(parent)?;
        let digest = ownership_digest(request, generation);
        let intent = claim_path(&generations, generation);
        let intent_bytes = read_owner_record(&intent)?;
        let claimed = intent_bytes.as_deref() == Some(digest.as_bytes());
        let partial_intent = !exists
            && intent_bytes
                .as_ref()
                .is_some_and(|bytes| digest.as_bytes().starts_with(bytes));
        if intent_bytes.is_some() && !claimed && !partial_intent {
            return Err(admission(
                "generation intent does not match the accepted build",
            ));
        }
        if exists {
            verify_ownership(&path, digest, claimed, phase == "reclaiming")?;
        }
        let mut reader_lock = open_reader_lock(&path.join(".read.lock"))?;
        let _reader_guard = reader_lock
            .as_mut()
            .map(|lock| {
                lock.try_write().map_err(|error| {
                    super::Error::internal_with_source("exclude staged generation readers", error)
                })
            })
            .transpose()?;
        if exists {
            validate_owned_files(&path)?;
            sync_directory(&path)?;
        }
        claim_reclamation(&mut db, request, generation, &phase, plan.as_str())?;
        drop(db);

        if exists {
            delete_owned_files(&path)?;
        }
        sync_directory(parent)?;
        if claimed || partial_intent {
            filesystem(
                &intent,
                "remove generation intent",
                fs::remove_file(&intent),
            )?;
            sync_directory(parent)?;
        }
        release_reservation(&cache, request, generation, settings, plan.as_str())
    })
}

pub(super) fn open_recovery_registry(
    cache: &Path,
    settings: &Resolved<DbSettings>,
) -> Result<super::Db> {
    // Recovery can open these SQLite-managed sidecars. Regular no-link
    // paths are required under the quiescent maintenance owner.
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = cache.join(format!("{DB_FILE}{suffix}"));
        match fs::symlink_metadata(&path) {
            Ok(_) => filesystem(
                &path,
                "inspect recovery sidecar",
                native_fs::check_single_link(&path),
            )?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return filesystem(&path, "inspect recovery sidecar", Err(error)),
        }
    }
    open_registry_file(cache, &cache.join(DB_FILE), settings)
}

fn open_reader_lock(path: &Path) -> Result<Option<RwLock<super::File>>> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(Some(RwLock::new(open_coordination_file(path, false)?))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => filesystem(path, "inspect staged read lease", Err(error)),
    }
}

fn claim_reclamation(
    db: &mut super::Db,
    request: LexicalBuildRequest,
    generation: i64,
    phase: &str,
    plan: &str,
) -> Result<()> {
    db.connection()
        .pragma_update(None, "synchronous", "EXTRA")
        .map_err(repo::db_error)?;
    let tx = db.begin_write()?;
    validate_epoch(&tx, request.epoch)?;
    let changed = tx
        .execute(
            "UPDATE project_generations SET phase = 'reclaiming'
         WHERE project_id = ?1 AND id = ?2 AND phase = ?3
            AND layout = 'local' AND plan_id = ?4 AND seal_digest IS NULL",
            params![request.project_id, generation, phase, plan],
        )
        .map_err(repo::db_error)?;
    if changed != 1 {
        return Err(admission("inactive build changed before discard"));
    }
    tx.commit().map_err(repo::db_error)
}

fn release_reservation(
    cache: &Path,
    request: LexicalBuildRequest,
    generation: i64,
    settings: &Resolved<DbSettings>,
    plan: &str,
) -> Result<()> {
    let mut db = open_recovery_registry(cache, settings)?;
    db.connection()
        .pragma_update(None, "synchronous", "EXTRA")
        .map_err(repo::db_error)?;
    let tx = db.begin_write()?;
    validate_epoch(&tx, request.epoch)?;
    let removed = tx
        .execute(
            "DELETE FROM project_generations WHERE project_id = ?1 AND id = ?2
            AND phase = 'reclaiming' AND layout = 'local'
            AND plan_id = ?3 AND seal_digest IS NULL",
            params![request.project_id, generation, plan],
        )
        .map_err(repo::db_error)?;
    if removed != 1 {
        return Err(admission(
            "discard record changed before releasing its reservation",
        ));
    }
    tx.commit().map_err(repo::db_error)
}

pub(super) fn directory_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_dir()
                && filesystem(path, "resolve directory", fs::canonicalize(path))? == path =>
        {
            Ok(true)
        }
        Ok(_) => Err(admission("discard path is not its own directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => filesystem(path, "inspect discard directory", Err(error)),
    }
}

pub(super) fn verify_ownership(
    path: &Path,
    digest: blake3::Hash,
    claimed: bool,
    reclaiming: bool,
) -> Result<()> {
    let marker = path.join(".build-owner");
    let stored = read_owner_record(&marker)?;
    let expected = digest.as_bytes();
    if stored.as_deref() == Some(expected) {
        return Ok(());
    }
    let partial = stored
        .as_ref()
        .is_some_and(|bytes| expected.starts_with(bytes));
    let allowed = (claimed && (stored.is_none() || partial)) || (reclaiming && stored.is_none());
    if allowed {
        for entry in filesystem(
            path,
            "inspect incomplete generation owner",
            fs::read_dir(path),
        )? {
            let entry = filesystem(path, "inspect incomplete generation entry", entry)?;
            if !partial || entry.path() != marker {
                return Err(admission("incomplete generation has unverified payload"));
            }
        }
        return Ok(());
    }
    Err(admission(
        "generation ownership record does not match the accepted build",
    ))
}

pub(super) fn read_owner_record(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return filesystem(path, "inspect generation owner record", Err(error)),
    }
    let mut bytes = Vec::with_capacity(33);
    let mut file = open_coordination_file(path, false)?.take(33);
    filesystem(
        path,
        "read generation ownership record",
        file.read_to_end(&mut bytes),
    )?;
    Ok(Some(bytes))
}

pub(super) fn validate_owned_files(path: &Path) -> Result<()> {
    for entry in filesystem(path, "inspect staged directory", fs::read_dir(path))? {
        let entry = filesystem(path, "inspect staged entry", entry)?;
        let child = entry.path();
        let name = entry.file_name();
        match name.to_str() {
            Some("tantivy") if directory_exists(&child)? => {
                for file in filesystem(&child, "inspect staged Tantivy", fs::read_dir(&child))? {
                    let file = filesystem(&child, "inspect staged Tantivy component", file)?.path();
                    if !file
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(is_tantivy_file)
                    {
                        return Err(admission("discard found an unrecognized Tantivy entry"));
                    }
                    filesystem(
                        &file,
                        "verify staged Tantivy component",
                        native_fs::check_single_link(&file),
                    )?;
                }
            }
            Some(
                REBUILD_MARKER
                | ".build-owner"
                | ".read.lock"
                | "metadata.db"
                | "metadata.db-journal"
                | "metadata.db-wal"
                | "metadata.db-shm"
                | "seal.json",
            ) => {
                filesystem(
                    &child,
                    "verify owned staged file",
                    native_fs::check_single_link(&child),
                )?;
            }
            _ => return Err(admission("discard found an unrecognized generation entry")),
        }
    }
    Ok(())
}

// Tantivy's fixed names and UUID-based segment components. A failed
// managed-directory atomic write can also leave its tempfile.
fn is_tantivy_file(name: &str) -> bool {
    if matches!(
        name,
        ".managed.json"
            | "meta.json"
            | ".tantivy-meta.lock"
            | ".tantivy-writer.lock"
            | REBUILD_MARKER
            | TOKENIZER_VERSION_FILE
    ) {
        return true;
    }
    if let Some(temp) = name.strip_prefix(".tmp") {
        return temp.len() == 6 && temp.bytes().all(|byte| byte.is_ascii_alphanumeric());
    }
    let Some((id, extension)) = name.split_once('.') else {
        return false;
    };
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    match extension {
        "idx" | "pos" | "term" | "store" | "fast" | "fieldnorm" => true,
        _ => extension
            .strip_suffix(".del")
            .is_some_and(|opstamp| opstamp.parse::<u64>().is_ok()),
    }
}

fn delete_owned_files(path: &Path) -> Result<()> {
    let marker = path.join(".build-owner");
    let tantivy = path.join("tantivy");
    for entry in filesystem(path, "list generation to discard", fs::read_dir(path))? {
        let file = filesystem(path, "list generation entry", entry)?.path();
        if file == marker {
            continue;
        }
        if file == tantivy {
            if !directory_exists(&file)? {
                return Err(admission("discarded Tantivy directory disappeared"));
            }
            filesystem(&file, "remove staged Tantivy", fs::remove_dir_all(&file))?;
        } else {
            filesystem(
                &file,
                "inspect file to discard",
                native_fs::check_single_link(&file),
            )?;
            filesystem(&file, "remove staged file", fs::remove_file(&file))?;
        }
    }
    // Make the payload removal durable before unlinking the ownership
    // marker. A crash after that point can leave only an empty directory.
    sync_directory(path)?;
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return filesystem(&marker, "remove generation ownership record", Err(error)),
    }
    filesystem(path, "remove generation directory", fs::remove_dir(path))
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    let file = filesystem(
        path,
        "open generation directory for sync",
        native_fs::open_readonly_nofollow(path),
    )?;
    filesystem(path, "sync generation directory", file.sync_all())
}

#[cfg(windows)]
pub(super) fn sync_directory(_: &Path) -> Result<()> {
    Err(admission(
        "generation operation needs a supported directory durability barrier",
    ))
}
