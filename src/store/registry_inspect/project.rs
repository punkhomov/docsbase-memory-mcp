//! Quiescent read of the metadata/Tantivy pair selected by one registry head.
use std::fs;
use std::path::Path;

use rusqlite::Connection;

use super::{
    InspectionLimits, UnavailableReason as Why, presence, schema_version, sqlite_failure, validate,
    with_locked_db, with_registry,
};
use crate::config::capture::{ReadFailure, read_bounded};
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::daemon::lifecycle;
use crate::error::Result;
use crate::index::tantivy_index::{ReadIndex, TOKENIZER_VERSION_FILE};
use crate::limits::MAX_STATE_BYTES;
use crate::platform::fs::{
    check_single_link, file_identity_and_links, is_regular_nofollow, open_readonly_nofollow,
};
use crate::store::generation_catalog::{self, Head, Layout};
use crate::store::generation_seal::{GenerationSeal, SEAL_FORMAT};
use crate::store::models::ProjectStatus;
use crate::store::project_index::{
    LOCAL_SCHEMA_VERSION, ProjectIndex, SEALED_LOCAL_SCHEMA_VERSION, project_directory,
};
use crate::store::settings::{DbSettings, DbSpec};
use crate::store::{migrations, repo};

type Observed<T> = std::result::Result<T, Why>;

/// Physical layout selected by the observed project generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIndexLayout {
    /// Metadata in the registry, with the historical Tantivy directory.
    Legacy,
    /// Metadata and Tantivy inside the selected generation directory.
    Local,
}

/// Identity of the pair checked while its read leases were held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedProjectIndex {
    /// Registered numeric project ID.
    pub project_id: i64,
    /// Installation epoch assigned by a generation-aware registry.
    pub epoch: Option<[u8; 16]>,
    /// Active generation ID, when a generation catalog is installed.
    pub generation_id: Option<i64>,
    /// Current revision from the selected metadata store.
    pub content_revision: i64,
    /// Location model used by the observation.
    pub layout: ProjectIndexLayout,
}

/// Point-in-time result for one selected project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIndexObservation {
    /// No registered index generation was selected.
    Missing,
    /// Observation could not establish a readable pair.
    Unavailable(Why),
    /// The selected metadata and Tantivy components opened under one lease.
    Present(ObservedProjectIndex),
}

/// Checks the selected pair from a quiescent registry without creating files.
/// Database and index handles are released before the result is returned.
/// # Errors
/// Invalid project ID, cache context, limits or storage settings.
pub fn inspect_project_index(
    cache: &Path,
    project_id: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<ProjectIndexObservation> {
    let initial_bytes = validate(cache, limits)?;
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    if project_id <= 0 {
        return Err(crate::error::Error::Admission {
            message: "index inspection requires a positive project ID".to_owned(),
        });
    }
    let result = presence(cache).and_then(|present| {
        if !present {
            return Ok(None);
        }
        with_registry(
            cache,
            limits,
            || {},
            |conn, root| observe_project(conn, root, project_id, limits, initial_bytes),
        )
    });
    Ok(match result {
        Ok(Some(index)) => ProjectIndexObservation::Present(index),
        Ok(None) => ProjectIndexObservation::Missing,
        Err(reason) => ProjectIndexObservation::Unavailable(reason),
    })
}

pub(in crate::store) fn observe_project(
    registry: &Connection,
    cache: &Path,
    project_id: i64,
    limits: InspectionLimits,
    initial_bytes: usize,
) -> Observed<Option<ObservedProjectIndex>> {
    let version = schema_version(registry)?;
    if !matches!(
        version,
        migrations::SCHEMA_VERSION | generation_catalog::SCHEMA_VERSION
    ) {
        return Err(Why::UnsupportedSchema);
    }
    let registered: bool = registry
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )
        .map_err(sqlite_failure)?;
    if !registered {
        return Ok(None);
    }
    let catalog = if version == generation_catalog::SCHEMA_VERSION {
        let epoch = generation_catalog::read_epoch(registry)?;
        let Some(head) = generation_catalog::active_head(registry, project_id)? else {
            return Ok(None);
        };
        Some((epoch, head))
    } else {
        None
    };
    check_identity_budget(limits, initial_bytes, catalog.is_some())?;
    let project = project_directory(cache, project_id);
    if catalog.is_none() && legacy_revision(registry, project_id)?.is_none() {
        return Ok(None);
    }
    check_directory(&project)?;
    with_read_lease(&project.join(".writer.lock"), || match catalog {
        None => observe_legacy(registry, cache, project_id, None).map(Some),
        Some((epoch, head)) => {
            let index = match head.layout {
                Layout::Legacy => {
                    with_read_lease(&project.join(".legacy-generation.lock"), || {
                        observe_legacy(registry, cache, project_id, Some((epoch, head.id)))
                    })?
                }
                Layout::Local { seal_digest } => {
                    observe_local(&project, epoch, &head, seal_digest, limits)?
                }
            };
            Ok(Some(index))
        }
    })
}

pub(in crate::store) fn check_identity_budget(
    limits: InspectionLimits,
    initial_bytes: usize,
    has_catalog: bool,
) -> Observed<()> {
    // Retained numeric identity, tag and optional epoch and generation ID.
    let bytes = 2 * size_of::<i64>() + 1 + usize::from(has_catalog) * (16 + size_of::<i64>());
    if limits.max_projects == 0
        || initial_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > limits.max_metadata_bytes)
    {
        return Err(Why::LimitExceeded);
    }
    Ok(())
}

fn observe_legacy(
    registry: &Connection,
    cache: &Path,
    project_id: i64,
    catalog: Option<([u8; 16], i64)>,
) -> Observed<ObservedProjectIndex> {
    let revision = legacy_revision(registry, project_id)?.ok_or(Why::InvalidRegistry)?;
    let index = ProjectIndex::legacy(project_id, revision).map_err(|_| Why::InvalidRegistry)?;
    check_pair(registry, project_id, &index.tantivy_dir(cache), |dir| {
        check_tantivy_files(dir, None)
    })?;
    Ok(ObservedProjectIndex {
        project_id,
        epoch: catalog.map(|(epoch, _)| epoch),
        generation_id: catalog.map(|(_, id)| id),
        content_revision: index.content_revision(),
        layout: ProjectIndexLayout::Legacy,
    })
}

fn legacy_revision(registry: &Connection, project_id: i64) -> Observed<Option<i64>> {
    let (revision, status, version): (i64, String, u32) = registry
        .query_row(
            "SELECT index_generation, status, schema_version FROM projects WHERE id = ?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sqlite_failure)?;
    match status.parse().map_err(|_| Why::InvalidRegistry)? {
        ProjectStatus::NotIndexed => Ok(None),
        ProjectStatus::Indexed if version == migrations::PROJECT_SCHEMA_VERSION => {
            Ok(Some(revision))
        }
        ProjectStatus::Indexed => Err(Why::UnsupportedSchema),
        ProjectStatus::Error | ProjectStatus::Indexing => Err(Why::RecoveryRequired),
    }
}

fn observe_local(
    project: &Path,
    epoch: [u8; 16],
    head: &Head,
    seal_digest: [u8; 32],
    limits: InspectionLimits,
) -> Observed<ObservedProjectIndex> {
    let generation = project.join("generations").join(head.id.to_string());
    check_directory(&generation)?;
    with_read_lease(&generation.join(".read.lock"), || {
        let seal = read_checked_seal(&generation.join("seal.json"), &seal_digest)?;
        let metadata_path = generation.join("metadata.db");
        check_single_link(&metadata_path).map_err(|_| Why::Io)?;
        with_locked_db(
            &metadata_path,
            limits,
            || {},
            |locked| {
                let metadata = locked.connection();
                let revision = local_revision(metadata, head.project_id, head.id)?;
                let project_id = head.project_id;
                let generation_id = head.id;
                check_pair(metadata, project_id, &generation.join("tantivy"), |dir| {
                    if revision.schema == SEALED_LOCAL_SCHEMA_VERSION {
                        return check_tantivy_files(dir, None);
                    }
                    let seal: GenerationSeal =
                        serde_json::from_slice(&seal).map_err(|_| Why::InvalidRegistry)?;
                    if !revision.matches_seal(&seal, epoch, project_id, generation_id) {
                        return Err(Why::InvalidRegistry);
                    }
                    if legacy_revision(metadata, project_id)? != Some(revision.content_revision) {
                        return Err(Why::InvalidRegistry);
                    }
                    match revision.manifest {
                        Some(digest) => check_revised_tantivy(dir, &digest),
                        None => check_tantivy_files(dir, Some(&seal.files)),
                    }
                })?;
                Ok(ObservedProjectIndex {
                    project_id,
                    epoch: Some(epoch),
                    generation_id: Some(generation_id),
                    content_revision: revision.content_revision,
                    layout: ProjectIndexLayout::Local,
                })
            },
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::store) struct LocalRevision {
    pub(in crate::store) schema: u32,
    pub(in crate::store) content_revision: i64,
    pub(in crate::store) manifest: Option<[u8; 32]>,
    pub(in crate::store) pending: bool,
}

impl LocalRevision {
    pub(in crate::store) fn matches_seal(
        self,
        seal: &GenerationSeal,
        epoch: [u8; 16],
        project: i64,
        generation: i64,
    ) -> bool {
        seal.format == SEAL_FORMAT
            && seal.installation_epoch == epoch
            && seal.project_id == project
            && seal.generation_id == generation
            && (seal.metadata_schema == self.schema
                || (seal.metadata_schema == SEALED_LOCAL_SCHEMA_VERSION
                    && self.schema == LOCAL_SCHEMA_VERSION))
            && seal.content_revision >= 0
            && if self.manifest.is_some() {
                self.content_revision > seal.content_revision
            } else {
                self.content_revision == seal.content_revision
            }
    }
}

pub(in crate::store) fn local_revision(
    metadata: &Connection,
    project: i64,
    generation: i64,
) -> Observed<LocalRevision> {
    let revision = read_local_revision(metadata, project, generation)?;
    if revision.pending {
        return Err(Why::RecoveryRequired);
    }
    Ok(revision)
}

pub(in crate::store) fn read_local_revision(
    metadata: &Connection,
    project: i64,
    generation: i64,
) -> Observed<LocalRevision> {
    let schema = schema_version(metadata)?;
    let sql = match schema {
        SEALED_LOCAL_SCHEMA_VERSION => "SELECT singleton, project_id, generation_id, content_revision, 0, NULL FROM generation_state LIMIT 2",
        LOCAL_SCHEMA_VERSION => "SELECT singleton, project_id, generation_id, content_revision, mutation_pending,
            CASE WHEN tantivy_manifest IS NULL THEN NULL
                 WHEN typeof(tantivy_manifest)='blob' AND length(tantivy_manifest)=32 THEN tantivy_manifest
                 ELSE X'' END FROM generation_state LIMIT 2",
        _ => return Err(Why::UnsupportedSchema),
    };
    let mut query = metadata.prepare(sql).map_err(sqlite_failure)?;
    let mut rows = query.query([]).map_err(sqlite_failure)?;
    let row = rows
        .next()
        .map_err(sqlite_failure)?
        .ok_or(Why::InvalidRegistry)?;
    let integer = |column| row.get::<_, i64>(column).map_err(|_| Why::InvalidRegistry);
    let (singleton, project_id, generation_id, revision) =
        (integer(0)?, integer(1)?, integer(2)?, integer(3)?);
    let pending = integer(4)?;
    let bytes: Option<Vec<u8>> = row.get(5).map_err(|_| Why::InvalidRegistry)?;
    let manifest = bytes
        .map(|bytes| bytes.try_into().map_err(|_| Why::InvalidRegistry))
        .transpose()?;
    if singleton != 1
        || project_id != project
        || generation_id != generation
        || revision < 0
        || rows.next().map_err(sqlite_failure)?.is_some()
    {
        return Err(Why::InvalidRegistry);
    }
    let pending = match pending {
        0 => false,
        1 => true,
        _ => return Err(Why::InvalidRegistry),
    };
    Ok(LocalRevision {
        schema,
        content_revision: revision,
        manifest,
        pending,
    })
}

pub(in crate::store) fn read_checked_seal(path: &Path, expected: &[u8; 32]) -> Observed<Vec<u8>> {
    let file = open_checked_file(path)?;
    let bytes = read_bounded(file, MAX_STATE_BYTES).map_err(|error| match error {
        ReadFailure::LimitExceeded => Why::LimitExceeded,
        _ => Why::Io,
    })?;
    if blake3::hash(&bytes).as_bytes() != expected {
        return Err(Why::InvalidRegistry);
    }
    Ok(bytes)
}

fn check_pair(
    metadata: &Connection,
    project_id: i64,
    tantivy: &Path,
    artifact: impl FnOnce(&Path) -> Observed<()>,
) -> Observed<()> {
    if repo::has_pending_docs(metadata, project_id).map_err(|_| Why::Io)? {
        return Err(Why::RecoveryRequired);
    }
    artifact(tantivy)?;
    ReadIndex::check_health_with_project_lease(tantivy).map_err(|_| Why::Io)
}

pub(in crate::store) fn check_revised_tantivy(tantivy: &Path, expected: &[u8; 32]) -> Observed<()> {
    check_tantivy_files(tantivy, None)?;
    if &tantivy_manifest(tantivy)? != expected {
        return Err(Why::Changed);
    }
    Ok(())
}

pub(in crate::store) fn tantivy_manifest(tantivy: &Path) -> Observed<[u8; 32]> {
    let file = open_checked_file(&tantivy.join("meta.json"))?;
    let bytes = read_bounded(file, MAX_STATE_BYTES).map_err(|error| match error {
        ReadFailure::LimitExceeded => Why::LimitExceeded,
        _ => Why::Io,
    })?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

pub(in crate::store) fn check_tantivy_files(
    tantivy: &Path,
    sealed: Option<&[(String, u64)]>,
) -> Observed<()> {
    check_directory(tantivy)?;
    // Tantivy owns the component opens. Check the flat directory first under
    // the cooperating writer lease, including files read through its mmap backend.
    let reader_lock = &tantivy::directory::META_LOCK.filepath;
    let mut matched = 0;
    for entry in fs::read_dir(tantivy).map_err(|_| Why::Io)? {
        let entry = entry.map_err(|_| Why::Io)?;
        let path = entry.path();
        check_single_link(&path).map_err(|_| Why::Io)?;
        if let Some(files) = sealed {
            let name = entry.file_name();
            // Tantivy's read-time coordination file is outside the segment
            // payload and can be created by a reader of an immutable index.
            if Path::new(&name) == reader_lock {
                continue;
            }
            let relative = format!("tantivy/{}", name.to_str().ok_or(Why::InvalidRegistry)?);
            let position = files
                .binary_search_by(|(file, _)| file.cmp(&relative))
                .map_err(|_| Why::Changed)?;
            if fs::symlink_metadata(&path).map_err(|_| Why::Io)?.len() != files[position].1 {
                return Err(Why::Changed);
            }
            matched += 1;
        }
    }
    if let Some(files) = sealed {
        let expected = files
            .iter()
            .filter_map(|(file, _)| file.strip_prefix("tantivy/"))
            .filter(|name| Path::new(name) != reader_lock)
            .count();
        if matched != expected {
            return Err(Why::Changed);
        }
    }
    // Use Tantivy's lookup paths so the bound also applies on filesystems
    // that resolve a different spelling of the same name.
    for name in ["meta.json", ".managed.json", TOKENIZER_VERSION_FILE] {
        match fs::symlink_metadata(tantivy.join(name)) {
            Ok(metadata) if metadata.len() > MAX_STATE_BYTES => {
                return Err(Why::LimitExceeded);
            }
            Ok(_) => {}
            Err(error)
                if name == ".managed.json" && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Why::Io),
        }
    }
    Ok(())
}

pub(in crate::store) fn check_directory(path: &Path) -> Observed<()> {
    if !path.is_dir() || fs::canonicalize(path).map_err(|_| Why::Io)? != path {
        return Err(Why::Io);
    }
    Ok(())
}

pub(in crate::store) fn open_checked_file(path: &Path) -> Observed<fs::File> {
    #[cfg(unix)]
    check_single_link(path).map_err(|_| Why::Io)?;
    let file = open_readonly_nofollow(path).map_err(|_| Why::Io)?;
    let metadata = file.metadata().map_err(|_| Why::Io)?;
    if !is_regular_nofollow(&metadata)
        || file_identity_and_links(&file, &metadata)
            .map_err(|_| Why::Io)?
            .1
            != 1
    {
        return Err(Why::Io);
    }
    Ok(file)
}

fn with_read_lease<T>(path: &Path, read: impl FnOnce() -> Observed<T>) -> Observed<T> {
    let file = open_checked_file(path)?;
    lifecycle::with_read_lock(file, path, || Ok(read())).map_err(|error| {
        if lifecycle::is_lease_conflict(&error) {
            Why::Busy
        } else {
            Why::Io
        }
    })?
}
