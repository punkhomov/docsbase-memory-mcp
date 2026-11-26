//! Validated project index binding, captured under the project read lease.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::{outcome, tantivy_index::ReadIndex};

use super::generation_catalog::{self, Layout};
use super::generation_seal::GenerationSeal;
use super::models::{Project, ProjectStatus};
use super::registry_inspect::{
    self, InspectionLimits, ObservedProjectIndex, ProjectIndexLayout, UnavailableReason,
};
use super::{Db, migrations, repo};

mod update;
pub(crate) use update::apply_lexical_batch;

pub(super) const SEALED_LOCAL_SCHEMA_VERSION: u32 = 1;
pub(super) const LOCAL_SCHEMA_VERSION: u32 = 2;
pub(super) const LOCAL_SCHEMA_SQL: &str = include_str!("registry_inspect/project_schema.sql");
pub(super) const LOCAL_REVISION_SCHEMA_SQL: &str =
    include_str!("registry_inspect/revision_schema.sql");

/// Identifies one revision of a project's existing SQLite/Tantivy pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectIndex {
    project_id: i64,
    content_revision: i64,
    generation: Option<Generation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Generation {
    epoch: [u8; 16],
    id: i64,
    seal: Option<[u8; 32]>,
}

impl ProjectIndex {
    pub(crate) fn legacy(project_id: i64, content_revision: i64) -> Result<Self> {
        if project_id <= 0 || content_revision < 0 {
            return Err(Error::Project {
                message: format!("project {project_id} has an invalid index generation"),
                instruction: Some("check the project index before retrying".to_owned()),
            });
        }
        Ok(Self {
            project_id,
            content_revision,
            generation: None,
        })
    }

    pub(crate) const fn project_id(self) -> i64 {
        self.project_id
    }

    /// Order published snapshots of the same project within one installation.
    /// An epoch change requires stopped maintenance and has no numeric order.
    pub(crate) fn is_after(self, other: Self) -> bool {
        if self.project_id != other.project_id {
            return false;
        }
        match (self.generation, other.generation) {
            (Some(current), Some(previous)) => {
                current.epoch == previous.epoch
                    && (current.id, self.content_revision) > (previous.id, other.content_revision)
            }
            (None, None) => self.content_revision > other.content_revision,
            (Some(_), None) => true,
            (None, Some(_)) => false,
        }
    }

    pub(crate) const fn content_revision(self) -> i64 {
        self.content_revision
    }

    pub(crate) fn generation_id(self) -> Option<i64> {
        self.generation.map(|generation| generation.id)
    }

    pub(crate) fn catalog_generation(self) -> Option<([u8; 16], i64)> {
        self.generation
            .map(|generation| (generation.epoch, generation.id))
    }

    pub(crate) fn chunk_binding(self) -> Option<crate::search::chunk_ref::ChunkBinding> {
        self.generation
            .map(|generation| crate::search::chunk_ref::ChunkBinding {
                epoch: generation.epoch,
                project_id: self.project_id,
                generation_id: generation.id,
                content_revision: self.content_revision,
            })
    }

    /// Resolve within the same cache as the metadata reader that supplied this binding.
    pub(crate) fn tantivy_dir(self, cache: &Path) -> PathBuf {
        match self.generation {
            Some(Generation {
                id, seal: Some(_), ..
            }) => {
                local_generation_dir(&project_directory(cache, self.project_id), id).join("tantivy")
            }
            _ => legacy_tantivy_dir(cache, self.project_id),
        }
    }
}

/// Immutable metadata and the directory bound by the same generation lease.
pub(crate) struct LocalMetadata {
    pub(crate) db: Db,
    pub(crate) index_recipe: String,
    tantivy: PathBuf,
    publication: TantivyPublication,
}

enum TantivyPublication {
    Sealed(Vec<(String, u64)>),
    Revised([u8; 32]),
}

impl LocalMetadata {
    pub(crate) fn tantivy_dir(&self) -> Result<PathBuf> {
        let proof = match &self.publication {
            TantivyPublication::Sealed(files) => {
                registry_inspect::check_tantivy_files(&self.tantivy, Some(files))
            }
            TantivyPublication::Revised(digest) => {
                registry_inspect::check_revised_tantivy(&self.tantivy, digest)
            }
        };
        proof.map_err(|reason| unavailable("Tantivy files", reason))?;
        Ok(self.tantivy.clone())
    }
}

/// Resolve a catalog head under the project read lease. Keep its reader file
/// lease until the caller has finished all SQLite and Tantivy use in this phase.
/// The registry reader's slot also bounds the optional local connection.
pub(crate) fn with_catalog_pair<R, T>(
    registry: R,
    project: &Project,
    read: impl FnOnce(R, Option<LocalMetadata>, ProjectIndex) -> Result<T>,
) -> Result<T>
where
    R: Deref<Target = Db>,
{
    with_catalog_pair_using(registry, project, PairAccess::Read(None), read)
}

/// Whether a config proposing a different recipe needs a new head.
pub(crate) fn catalog_needs_index_for<R>(
    registry: R,
    project: &Project,
    recipe: &str,
) -> Result<bool>
where
    R: Deref<Target = Db>,
{
    let head = generation_catalog::active_head(registry.connection(), project.id)
        .map_err(|reason| unavailable("active generation", reason))?;
    match head.map(|head| head.layout) {
        None => Ok(false),
        Some(Layout::Legacy) => Ok(true),
        Some(Layout::Local { .. }) => with_catalog_pair(registry, project, |_, local, _| {
            Ok(local.is_none_or(|local| local.index_recipe != recipe))
        }),
    }
}

/// A watcher can update a selected local pair. Read registration status
/// from that pair because a new v5 project has no index rows in the registry.
pub(crate) fn local_watcher_project(registry: &Db, project: &Project) -> Result<Option<Project>> {
    let head = generation_catalog::active_head(registry.connection(), project.id)
        .map_err(|reason| unavailable("active generation", reason))?;
    if !head.is_some_and(|head| matches!(head.layout, Layout::Local { .. })) {
        return Ok(None);
    }
    with_catalog_pair(registry, project, |_, local, _| match local {
        Some(local) => registry::project_by_id(&local.db, project.id),
        None => Ok(None),
    })
}

pub(crate) struct CatalogProjectInfo {
    pub(crate) project: Project,
    pub(crate) counts: repo::ProjectCounts,
    pub(crate) revision: i64,
}

pub(crate) fn catalog_project_info(
    registry: &Db,
    project: &Project,
) -> Result<Option<CatalogProjectInfo>> {
    let head = generation_catalog::active_head(registry.connection(), project.id)
        .map_err(|reason| unavailable("active generation", reason))?;
    if head.is_none() {
        return Ok(None);
    }
    with_catalog_pair(registry, project, |registry, local, index| {
        let summarize = |metadata: &Db| {
            let registered = registry::project_by_id(metadata, project.id)?
                .ok_or_else(|| invalid_index("selected generation has no project metadata"))?;
            let counts = repo::project_counts(metadata.connection(), project.id)?;
            Ok(Some(CatalogProjectInfo {
                project: registered,
                counts,
                revision: index.content_revision(),
            }))
        };
        match local.as_ref() {
            Some(local) => {
                local.tantivy_dir()?;
                summarize(&local.db).map_err(local_metadata_error)
            }
            None => summarize(registry),
        }
    })
}

#[derive(Clone, Copy)]
enum PairAccess {
    Read(Option<InspectionLimits>),
    Write,
}

fn with_catalog_pair_using<R, T>(
    registry: R,
    project: &Project,
    access: PairAccess,
    read: impl FnOnce(R, Option<LocalMetadata>, ProjectIndex) -> Result<T>,
) -> Result<T>
where
    R: Deref<Target = Db>,
{
    let cache = canonical_cache(registry.cache_root())?;
    let directory = project_directory(&cache, project.id);
    registry_inspect::check_directory(&directory)
        .map_err(|reason| unavailable("project generation directory", reason))?;
    let writer = lifecycle::lease_path(&cache, project.id);
    let work = || {
        let current = registry::project_by_id(&registry, project.id)?
            .filter(|current| current.canonical_root == project.canonical_root)
            .ok_or_else(|| invalid_index("project binding changed before search preparation"))?;
        let connection = registry.connection();
        let epoch = generation_catalog::read_epoch(connection)
            .map_err(|reason| unavailable("registry epoch", reason))?;
        let head = generation_catalog::active_head(connection, project.id)
            .map_err(|reason| unavailable("active generation", reason))?
            .ok_or_else(|| invalid_index("project has no published generation"))?;
        let mut generation = Generation {
            epoch,
            id: head.id,
            seal: None,
        };
        match head.layout {
            Layout::Legacy => {
                if current.status != ProjectStatus::Indexed
                    || current.schema_version != migrations::PROJECT_SCHEMA_VERSION
                    || repo::has_pending_docs(connection, project.id)?
                {
                    return Err(invalid_index("registered legacy pair is incomplete"));
                }
                let mut index = ProjectIndex::legacy(
                    project.id,
                    repo::index_generation(connection, project.id)?,
                )?;
                index.generation = Some(generation);
                let path = directory.join(".legacy-generation.lock");
                with_catalog_lease(&path, "generation lease", || read(registry, None, index))
            }
            Layout::Local { seal_digest } => {
                generation.seal = Some(seal_digest);
                let generation_dir = local_generation_dir(&directory, head.id);
                registry_inspect::check_directory(&generation_dir)
                    .map_err(|reason| unavailable("generation directory", reason))?;
                let path = generation_dir.join(".read.lock");
                with_catalog_lease(&path, "generation lease", || {
                    let bytes = registry_inspect::read_checked_seal(
                        &generation_dir.join("seal.json"),
                        &seal_digest,
                    )
                    .map_err(|reason| unavailable("generation seal", reason))?;
                    let seal: GenerationSeal = serde_json::from_slice(&bytes).map_err(|_| {
                        invalid_index("published generation seal has an invalid format")
                    })?;
                    let metadata = open_catalog_metadata(&registry, &generation_dir, access)
                        .map_err(local_metadata_error)?;
                    let revision = registry_inspect::local_revision(
                        metadata.connection(),
                        project.id,
                        head.id,
                    )
                    .map_err(|reason| unavailable("generation metadata", reason))?;
                    if !revision.matches_seal(&seal, epoch, project.id, head.id) {
                        return Err(invalid_index(
                            "published generation seal does not match the selected metadata identity",
                        ));
                    }
                    verify_local_outcome(&metadata, project, revision, &seal)?;
                    let index = ProjectIndex {
                        project_id: project.id,
                        content_revision: revision.content_revision,
                        generation: Some(generation),
                    };
                    let local = LocalMetadata {
                        db: metadata,
                        index_recipe: seal.index_recipe,
                        tantivy: generation_dir.join("tantivy"),
                        publication: match revision.manifest {
                            Some(digest) => TantivyPublication::Revised(digest),
                            None => TantivyPublication::Sealed(seal.files),
                        },
                    };
                    read(registry, Some(local), index)
                })
            }
        }
    };
    let file = registry_inspect::open_checked_file(&writer)
        .map_err(|reason| unavailable("project lease", reason))?;
    match access {
        PairAccess::Read(_) => lifecycle::with_read_lock(file, &writer, work),
        PairAccess::Write => {
            let _preparation = super::generation_build::acquire_preparation_lease(&directory)?;
            lifecycle::with_writer_lock(file, &writer, work)
        }
    }
}

fn verify_local_outcome(
    metadata: &Db,
    project: &Project,
    revision: registry_inspect::LocalRevision,
    seal: &GenerationSeal,
) -> Result<()> {
    let outcome = outcome::read(metadata, project.id).map_err(local_metadata_error)?;
    if !outcome.is_some_and(|outcome| {
        // The seal proves original corpus coverage. An incremental outcome
        // also reports per-path capture errors while committing valid edits.
        (outcome.is_complete()
            || (revision.manifest.is_some() && outcome.scope == outcome::SurveyScope::Subset))
            && outcome.index_generation == revision.content_revision
            && project.canonical_root.to_str() == Some(outcome.canonical_root.as_str())
            && outcome.recipe_id == seal.index_recipe
    }) {
        return Err(invalid_index(
            "published generation seal or revision does not match a successful index outcome",
        ));
    }
    Ok(())
}

/// Observe a registered base through the same leases and artifact proofs as
/// search. A registration without a head is safe to inspect before its first
/// lock file exists; callers still compare that absence before reserving.
pub(super) fn observe_catalog_index(
    db: &Db,
    project: &Project,
    limits: InspectionLimits,
) -> Result<Option<ObservedProjectIndex>> {
    if generation_catalog::active_head(db.connection(), project.id)
        .map_err(|reason| unavailable("active generation", reason))?
        .is_none()
    {
        return if project.status == ProjectStatus::NotIndexed {
            Ok(None)
        } else {
            Err(invalid_index(
                "registered project has no published generation",
            ))
        };
    }
    with_catalog_pair_using(
        db,
        project,
        PairAccess::Read(Some(limits)),
        |registry, local, index| {
            let generation = index
                .generation
                .ok_or_else(|| invalid_index("selected pair has no catalog identity"))?;
            let dir = if let Some(local) = local {
                local.tantivy_dir()?
            } else {
                let dir = index.tantivy_dir(registry.cache_root());
                registry_inspect::check_tantivy_files(&dir, None)
                    .map_err(|reason| unavailable("Tantivy files", reason))?;
                dir
            };
            ReadIndex::check_health_with_project_lease(&dir)?;
            Ok(Some(ObservedProjectIndex {
                project_id: project.id,
                epoch: Some(generation.epoch),
                generation_id: Some(generation.id),
                content_revision: index.content_revision,
                layout: if generation.seal.is_some() {
                    ProjectIndexLayout::Local
                } else {
                    ProjectIndexLayout::Legacy
                },
            }))
        },
    )
}

fn open_catalog_metadata(registry: &Db, directory: &Path, access: PairAccess) -> Result<Db> {
    let path = directory.join("metadata.db");
    registry_inspect::sidecars_absent(&path)
        .map_err(|reason| unavailable("generation metadata sidecars", reason))?;
    crate::platform::fs::check_single_link(&path)
        .map_err(|err| Error::internal_with_source("inspect generation metadata file", err))?;
    let settings = registry.settings().settings();
    let (flags, timeout) = match access {
        PairAccess::Read(_) => (OpenFlags::SQLITE_OPEN_READ_ONLY, settings.read_busy_timeout),
        PairAccess::Write => (
            OpenFlags::SQLITE_OPEN_READ_WRITE,
            settings.write_busy_timeout,
        ),
    };
    let conn = Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(repo::db_error)?;
    conn.busy_timeout(timeout).map_err(repo::db_error)?;
    if let PairAccess::Read(Some(limits)) = access {
        registry_inspect::configure_connection(&conn, limits).map_err(repo::db_error)?;
    }
    Ok(Db {
        conn,
        cache_root: registry.cache_root().to_path_buf(),
        settings: registry.settings().clone(),
        generation_catalog: false,
    })
}

fn with_catalog_lease<T>(path: &Path, label: &str, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let file =
        registry_inspect::open_checked_file(path).map_err(|reason| unavailable(label, reason))?;
    lifecycle::with_read_lock(file, path, work)
}

fn canonical_cache(cache: &Path) -> Result<PathBuf> {
    cache
        .canonicalize()
        .map_err(|error| Error::internal_with_source("resolve catalog cache root", error))
}

fn unavailable(object: &str, reason: UnavailableReason) -> Error {
    invalid_index(format!("{object} is unavailable ({reason:?})"))
}

fn invalid_index(message: impl Into<String>) -> Error {
    Error::Project {
        message: message.into(),
        instruction: Some("check the published project index before retrying".into()),
    }
}

fn local_metadata_error(error: Error) -> Error {
    match error {
        Error::Project { .. } => error,
        _ => invalid_index(format!("cannot read generation-local metadata: {error}")),
    }
}

fn local_generation_dir(project: &Path, generation: i64) -> PathBuf {
    project.join("generations").join(generation.to_string())
}

pub(crate) fn legacy_tantivy_dir(cache: &Path, project_id: i64) -> PathBuf {
    project_directory(cache, project_id).join("tantivy")
}

pub(super) fn project_directory(cache: &Path, project_id: i64) -> PathBuf {
    cache.join("projects").join(project_id.to_string())
}
