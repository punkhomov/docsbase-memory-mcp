//! Retryable retirement of inactive local generations.

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
        find_abandoned_build(&db, request)
    })
}

fn find_abandoned_build(db: &super::Db, request: LexicalBuildRequest) -> Result<Option<i64>> {
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
}

pub(super) fn discard_index_attempt(
    cache: &Path,
    project: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<()> {
    let db = open_recovery_registry(cache, settings)?;
    validate_epoch(db.connection(), request.epoch)?;
    let abandoned = find_abandoned_build(&db, request)?;
    db.close()?;
    if let Some(generation) = abandoned {
        reclaim_stopped(
            cache,
            project,
            request,
            generation,
            settings,
            limits,
            Reclamation::Abandoned,
        )?;
    }
    Ok(())
}

/// Retry reclamation only for retired pairs built by the explicit index owner.
pub(super) fn reclaim_index_pairs(
    cache: &Path,
    project: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) {
    let retired = match retired_index_receipts(cache, request, settings, limits) {
        Ok(retired) => retired,
        Err(error) => {
            eprintln!("docsbase: retired generation cleanup will be retried: {error}");
            return;
        }
    };
    for (generation, receipt) in retired {
        if let Err(error) = reclaim_stopped(
            cache,
            project,
            request,
            generation,
            settings,
            limits,
            Reclamation::Retired(receipt),
        ) {
            eprintln!("docsbase: retained generation {generation} for later cleanup: {error}");
        }
    }
}

fn retired_index_receipts(
    cache: &Path,
    request: LexicalBuildRequest,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Vec<(i64, RetiredReceipt)>> {
    let db = open_recovery_registry(cache, settings)?;
    validate_epoch(db.connection(), request.epoch)?;
    let plan = blake3::Hash::from_bytes(request.plan_digest).to_hex();
    let maximum_rows = limits.max_metadata_bytes / size_of::<(i64, RetiredReceipt)>();
    let maximum_rows = i64::try_from(maximum_rows)
        .map_err(|_| admission("generation recovery exceeds the catalog inspection limit"))?;
    let retired =
        {
            let mut statement = db.connection().prepare(
            "SELECT id, base_id, base_revision, reserved_disk_bytes FROM project_generations
             WHERE project_id = ?1 AND plan_id = ?2 AND layout = 'local'
                AND phase IN ('retired', 'reclaiming') AND seal_digest IS NOT NULL
             ORDER BY id LIMIT ?3",
        ).map_err(repo::db_error)?;
            statement
                .query_map(
                    params![request.project_id, plan.as_str(), maximum_rows],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            RetiredReceipt {
                                base_id: row.get(1)?,
                                base_revision: row.get(2)?,
                                reserved_disk_bytes: row.get(3)?,
                            },
                        ))
                    },
                )
                .map_err(repo::db_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(repo::db_error)?
        };
    db.close()?;
    Ok(retired)
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
    reclaim_local_generation(
        cache,
        request,
        generation,
        settings,
        limits,
        Reclamation::Abandoned,
    )
}

/// Reclaims a retired local pair using the request that produced it.
/// Requires exclusive access to its reader lease and the original publication
/// seal. An interrupted reclaim can resume after its partial deletion.
/// # Errors
/// An active or legacy generation, invalid ownership, a busy reader, or a
/// failure to durably remove the retired files or catalogue entry.
pub fn reclaim_retired_lexical_generation(
    cache: &Path,
    request: LexicalBuildRequest,
    generation: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<()> {
    reclaim_local_generation(
        cache,
        request,
        generation,
        settings,
        limits,
        Reclamation::Retired(RetiredReceipt {
            base_id: request.base.and_then(|base| base.generation_id),
            base_revision: request.base.map_or(0, |base| base.content_revision),
            reserved_disk_bytes: i64::try_from(request.reserved_disk_bytes)
                .map_err(|_| admission("retired reservation exceeds the SQLite integer range"))?,
        }),
    )
}

#[derive(Clone, Copy)]
enum Reclamation {
    Abandoned,
    Retired(RetiredReceipt),
}

impl Reclamation {
    const fn is_retired(self) -> bool {
        matches!(self, Self::Retired(_))
    }
}

#[derive(Clone, Copy)]
struct RetiredReceipt {
    base_id: Option<i64>,
    base_revision: i64,
    reserved_disk_bytes: i64,
}

struct RetainedGeneration {
    project: i64,
    phase: String,
    layout: String,
    plan: Option<String>,
    seal: Option<[u8; 32]>,
    base_id: Option<i64>,
    base_revision: i64,
    reservation: i64,
}

impl RetainedGeneration {
    fn load(registry: &rusqlite::Connection, generation: i64) -> Result<Option<Self>> {
        registry
            .query_row(
                "SELECT project_id, phase, layout, plan_id, seal_digest,
                        base_id, base_revision, reserved_disk_bytes
                 FROM project_generations WHERE id = ?1",
                [generation],
                |row| {
                    Ok(Self {
                        project: row.get(0)?,
                        phase: row.get(1)?,
                        layout: row.get(2)?,
                        plan: row.get(3)?,
                        seal: row.get(4)?,
                        base_id: row.get(5)?,
                        base_revision: row.get(6)?,
                        reservation: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(repo::db_error)
    }

    fn resuming(&self, kind: Reclamation, plan: &str) -> Result<bool> {
        let (initial_phase, error) = match kind {
            Reclamation::Abandoned => {
                ("building", "generation is not the requested inactive build")
            }
            Reclamation::Retired(_) => (
                "retired",
                "generation is not the requested retired local pair",
            ),
        };
        let resuming = self.phase == "reclaiming";
        if (!resuming && self.phase != initial_phase)
            || self.layout != "local"
            || self.plan.as_deref() != Some(plan)
            || self.seal.is_some() != kind.is_retired()
            || matches!(kind, Reclamation::Retired(receipt) if !self.matches_build(receipt))
        {
            return Err(admission(error));
        }
        Ok(resuming)
    }

    fn matches_build(&self, receipt: RetiredReceipt) -> bool {
        self.base_id == receipt.base_id
            && self.base_revision == receipt.base_revision
            && self.reservation == receipt.reserved_disk_bytes
    }
}

fn find_reclamation_record(
    registry: &rusqlite::Connection,
    project: i64,
    generation: i64,
    kind: Reclamation,
) -> Result<Option<RetainedGeneration>> {
    let state = RetainedGeneration::load(registry, generation)?;
    match state {
        Some(state) if state.project == project => Ok(Some(state)),
        Some(_) if kind.is_retired() => Err(admission(
            "retired generation belongs to a different project",
        )),
        _ => Ok(None),
    }
}

fn reclaim_local_generation(
    cache: &Path,
    request: LexicalBuildRequest,
    generation: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    kind: Reclamation,
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
        reclaim_stopped(
            &cache, &project, request, generation, settings, limits, kind,
        )
    })
}

fn reclaim_stopped(
    cache: &Path,
    project: &Path,
    request: LexicalBuildRequest,
    generation: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    kind: Reclamation,
) -> Result<()> {
    let generations = project.join("generations");
    let parent = if directory_exists(&generations)? {
        generations.as_path()
    } else {
        project
    };
    let path = generations.join(generation.to_string());
    let exists = directory_exists(&path)?;
    let plan = blake3::Hash::from_bytes(request.plan_digest).to_hex();
    let mut db = open_recovery_registry(cache, settings)?;
    validate_epoch(db.connection(), request.epoch)?;
    let intent = claim_path(&generations, generation);
    let Some(state) =
        find_reclamation_record(db.connection(), request.project_id, generation, kind)?
    else {
        return if exists || (kind.is_retired() && read_owner_record(&intent)?.is_some()) {
            Err(admission(
                "discard found an uncatalogued generation directory",
            ))
        } else {
            Ok(())
        };
    };
    let resuming = state.resuming(kind, plan.as_str())?;
    let seal_digest = state.seal.as_ref().map(<[u8; 32]>::as_slice);

    // Confirm the required barrier before changing either durable state.
    sync_directory(parent)?;
    let digest = ownership_digest(request, generation);
    let intent_bytes = read_owner_record(&intent)?;
    let claimed = verify_intent(
        intent_bytes.as_deref(),
        digest,
        !exists && !kind.is_retired(),
    )?;
    if exists {
        verify_ownership(&path, digest, claimed, resuming)?;
    }
    let mut reader_lock = open_reader_lock(&path.join(".read.lock"))?;
    if kind.is_retired()
        && ((exists && !claimed) || (!resuming && (!exists || reader_lock.is_none())))
    {
        return Err(admission(
            "retired generation has incomplete ownership or reader lease",
        ));
    }
    let _reader_guard = reader_lock
        .as_mut()
        .map(|lock| {
            lock.try_write().map_err(|error| {
                super::Error::internal_with_source("exclude staged generation readers", error)
            })
        })
        .transpose()?;
    if exists {
        match seal_digest {
            Some(seal) => validate_retired_files(
                &path,
                seal,
                limits,
                resuming,
                request.epoch,
                generation,
                &state,
            )?,
            None => validate_owned_files(&path)?,
        }
        sync_directory(&path)?;
    }
    claim_reclamation(
        &mut db,
        request,
        generation,
        &state.phase,
        plan.as_str(),
        seal_digest,
    )?;
    drop(db);

    remove_reclaiming_generation(
        parent,
        exists.then_some(path.as_path()),
        intent_bytes.as_ref().map(|_| intent.as_path()),
    )?;
    release_reservation(
        cache,
        request,
        generation,
        settings,
        plan.as_str(),
        seal_digest,
    )
}

fn verify_intent(bytes: Option<&[u8]>, digest: blake3::Hash, allow_partial: bool) -> Result<bool> {
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    if bytes == digest.as_bytes() {
        Ok(true)
    } else if allow_partial && digest.as_bytes().starts_with(bytes) {
        Ok(false)
    } else {
        Err(admission(
            "generation intent does not match the accepted build",
        ))
    }
}

fn remove_reclaiming_generation(
    parent: &Path,
    path: Option<&Path>,
    intent: Option<&Path>,
) -> Result<()> {
    if let Some(path) = path {
        delete_owned_files(path)?;
    }
    sync_directory(parent)?;
    if let Some(intent) = intent {
        filesystem(intent, "remove generation intent", fs::remove_file(intent))?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn validate_retired_files(
    path: &Path,
    expected: &[u8],
    limits: InspectionLimits,
    resuming: bool,
    epoch: [u8; 16],
    generation: i64,
    state: &RetainedGeneration,
) -> Result<()> {
    let seal = path.join("seal.json");
    if resuming && !super::entry_exists(&seal)? {
        // A retired seal is removed only after syncing the payload removal.
        return validate_generation_files(path, Some(&[]));
    }
    let cap = crate::limits::MAX_STATE_BYTES.min(limits.max_metadata_bytes as u64);
    let content = crate::config::capture::read_bounded(open_coordination_file(&seal, false)?, cap)
        .map_err(|error| admission(format!("retired generation seal is unavailable: {error}")))?;
    if blake3::hash(&content).as_bytes() != expected {
        return Err(admission("retired generation seal changed"));
    }
    let manifest: crate::store::generation_seal::GenerationSeal = serde_json::from_slice(&content)
        .map_err(|error| {
            super::Error::internal_with_source("decode retired generation seal", error)
        })?;
    if manifest.format != crate::store::generation_seal::SEAL_FORMAT
        || manifest.installation_epoch != epoch
        || manifest.project_id != state.project
        || manifest.generation_id != generation
        || Some(manifest.plan.as_str()) != state.plan.as_deref()
        || manifest.base_id != state.base_id
        || manifest.base_revision != state.base_revision
    {
        return Err(admission(
            "retired generation seal does not match its catalog receipt",
        ));
    }
    validate_generation_files(path, Some(&manifest.files))
}

pub(super) fn open_recovery_registry(
    cache: &Path,
    settings: &Resolved<DbSettings>,
) -> Result<super::Db> {
    // Resolve the configured directory before SQLite's NOFOLLOW open, which
    // also rejects linked ancestors such as /var on macOS. The registry and
    // its sidecars are still checked at their final names without following links.
    let cache = filesystem(cache, "resolve registry cache", fs::canonicalize(cache))?;
    // The SQLite VFS can open these entries during normal WAL access or
    // recovery. A cooperating connection may remove an absent/closing sidecar.
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = cache.join(format!("{DB_FILE}{suffix}"));
        match native_fs::check_single_link(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return filesystem(&path, "inspect registry sidecar", Err(error)),
        }
    }
    open_registry_file(&cache, &cache.join(DB_FILE), settings)
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
    seal: Option<&[u8]>,
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
            AND layout = 'local' AND plan_id = ?4 AND seal_digest IS ?5
            AND NOT EXISTS (SELECT 1 FROM project_generation_head WHERE project_id = ?1 AND active_id = ?2)",
            params![request.project_id, generation, phase, plan, seal],
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
    seal: Option<&[u8]>,
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
            AND plan_id = ?3 AND seal_digest IS ?4
            AND NOT EXISTS (SELECT 1 FROM project_generation_head WHERE project_id = ?1 AND active_id = ?2)",
            params![request.project_id, generation, plan, seal],
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
    validate_generation_files(path, None)
}

fn validate_generation_files(path: &Path, manifest: Option<&[(String, u64)]>) -> Result<()> {
    for entry in filesystem(path, "inspect staged directory", fs::read_dir(path))? {
        let entry = filesystem(path, "inspect staged entry", entry)?;
        let child = entry.path();
        let name = entry.file_name();
        match name.to_str() {
            Some("tantivy")
                if manifest.is_none_or(|files| !files.is_empty()) && directory_exists(&child)? =>
            {
                for file in filesystem(&child, "inspect staged Tantivy", fs::read_dir(&child))? {
                    let file = filesystem(&child, "inspect staged Tantivy component", file)?.path();
                    if !file
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(is_tantivy_file)
                    {
                        return Err(admission("discard found an unrecognized Tantivy entry"));
                    }
                    verify_generation_file(path, &file, manifest)?;
                }
            }
            Some(".build-owner" | "seal.json") => {
                verify_generation_file(path, &child, None)?;
            }
            Some(
                REBUILD_MARKER
                | ".read.lock"
                | "metadata.db"
                | "metadata.db-journal"
                | "metadata.db-wal"
                | "metadata.db-shm",
            ) => {
                verify_generation_file(path, &child, manifest)?;
            }
            _ => return Err(admission("discard found an unrecognized generation entry")),
        }
    }
    Ok(())
}

fn verify_generation_file(
    root: &Path,
    file: &Path,
    manifest: Option<&[(String, u64)]>,
) -> Result<()> {
    filesystem(
        file,
        "verify owned generation file",
        native_fs::check_single_link(file),
    )?;
    if let Some(manifest) = manifest {
        let relative = file
            .strip_prefix(root)
            .map(crate::index::walk::portable_rel)
            .map_err(|_| admission("generation component left its directory"))?;
        let entry = manifest
            .binary_search_by(|(name, _)| name.cmp(&relative))
            .map_err(|_| admission("retired generation contains a file absent from its seal"))?;
        let bytes = filesystem(
            file,
            "inspect retired component",
            fs::symlink_metadata(file),
        )?
        .len();
        if manifest[entry].1 != bytes {
            return Err(admission("retired generation component size changed"));
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
    let seal = path.join("seal.json");
    for entry in filesystem(path, "list generation to discard", fs::read_dir(path))? {
        let file = filesystem(path, "list generation entry", entry)?.path();
        if file == marker || file == seal {
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
    // Keep the seal available to authenticate surviving payload after a crash.
    sync_directory(path)?;
    match fs::remove_file(&seal) {
        Ok(()) => sync_directory(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return filesystem(&seal, "remove generation seal", Err(error)),
    }
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
