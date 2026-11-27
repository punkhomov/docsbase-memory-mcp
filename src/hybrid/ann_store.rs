//! Persisted ANN storage layer (spec ann-index T2): paths, key, metadata,
//! atomic write/load, stale/corrupt rejection and generation cleanup.
//!
//! No HNSW logic lives here: a dump is written by the caller through
//! [`AnnStore::persist_dump`] and consumed later by `hybrid::ann`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::capture::read_bounded;
use crate::error::{Error, Result};
use crate::hybrid::ann_settings::{AnnBuildSettings, LEGACY_BUILD_RECIPE};
use crate::platform::fs::{is_regular_nofollow, open_readonly_nofollow};
use crate::store::project_index::ProjectIndex;
use crate::store::repo::db_error;

/// First on-disk format version (design §5).
pub const ANN_FORMAT_VERSION: u32 = 1;

/// The only v1 `hnsw_rs` version (FR-13); a dump from any other version is
/// rejected on load and rebuilt.
pub const HNSW_RS_VERSION: &str = "0.3.5";

/// Default graph fan-out during construction (design §6).
pub const ANN_DEFAULT_M: usize = AnnBuildSettings::owner_defaults().m();

/// Default construction beam width (design §6).
pub const ANN_DEFAULT_EF_CONSTRUCTION: usize = AnnBuildSettings::owner_defaults().ef_construction();

// Version of the construction recipe in the slot name. The HNSW dump layout
// and its own on-disk format version are unchanged.
pub(crate) const ANN_RECIPE_VERSION: u32 = 1;

/// Provisional per-file size ceiling; a bigger graph/data file is treated as
/// corrupt and rejected (design §6, NFR-6).
pub const MAX_ANN_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Dump basename enforced by [`AnnStore::persist_dump`] (design §6).
const DUMP_BASENAME: &str = "index";
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Identity of one persisted index (FR-3): changing any field moves the
/// index to another on-disk key, so stale/corrupt state can never be
/// mistaken for the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnKey {
    pub project_id: i64,
    pub generation: i64,
    pub model_id: String,
    pub dim: usize,
    pub chunk_count: usize,
    pub chunk_set_digest: String,
    pub format_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AnnCatalogGeneration {
    epoch: [u8; 16],
    generation: i64,
}

/// The graph inputs and, for a catalog, the installation and physical head
/// to which the persisted dump belongs. Legacy callers keep their key format.
#[derive(Clone, Copy)]
pub(crate) struct AnnArtifact<'a> {
    pub(crate) key: &'a AnnKey,
    catalog: Option<AnnCatalogGeneration>,
}

impl<'a> AnnArtifact<'a> {
    pub(crate) fn for_index(key: &'a AnnKey, index: ProjectIndex) -> Self {
        let catalog = index
            .catalog_generation()
            .map(|(epoch, generation)| AnnCatalogGeneration { epoch, generation });
        Self { key, catalog }
    }
}

impl<'a> From<&'a AnnKey> for AnnArtifact<'a> {
    fn from(key: &'a AnnKey) -> Self {
        Self { key, catalog: None }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredAnnMeta {
    #[serde(flatten)]
    meta: AnnMeta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog: Option<AnnCatalogGeneration>,
}

/// Validated metadata of a persisted index; written at publish time and
/// checked against the requested [`AnnKey`] on load.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnMeta {
    pub format: u32,
    pub hnsw_rs_version: String,
    pub project_id: i64,
    pub generation: i64,
    pub model_id: String,
    pub dim: usize,
    pub count: usize,
    pub chunk_set_digest: String,
    pub m: usize,
    pub ef_construction: usize,
    pub created_at: i64,
}

impl AnnMeta {
    /// Metadata for `key` with the pinned defaults (FR-13).
    #[must_use]
    pub fn new(key: &AnnKey) -> Self {
        Self::for_settings(key, AnnBuildSettings::owner_defaults())
    }

    fn for_settings(key: &AnnKey, settings: AnnBuildSettings) -> Self {
        Self {
            format: key.format_version,
            hnsw_rs_version: HNSW_RS_VERSION.to_owned(),
            project_id: key.project_id,
            generation: key.generation,
            model_id: key.model_id.clone(),
            dim: key.dim,
            count: key.chunk_count,
            chunk_set_digest: key.chunk_set_digest.clone(),
            m: settings.m(),
            ef_construction: settings.ef_construction(),
            created_at: now_secs(),
        }
    }

    /// The key this metadata represents.
    #[must_use]
    pub fn key(&self) -> AnnKey {
        AnnKey {
            project_id: self.project_id,
            generation: self.generation,
            model_id: self.model_id.clone(),
            dim: self.dim,
            chunk_count: self.count,
            chunk_set_digest: self.chunk_set_digest.clone(),
            format_version: self.format,
        }
    }

    /// Whether this metadata exactly matches `key` (FR-3).
    #[must_use]
    pub fn matches_key(&self, key: &AnnKey) -> bool {
        self.key() == *key
    }
}

/// A load-validated persisted index.
#[derive(Debug, Clone)]
pub struct AnnSlot {
    /// Directory holding the dump, `meta.json` and `READY`.
    pub dir: PathBuf,
    /// Metadata the slot was validated against.
    pub meta: AnnMeta,
}

/// Deterministic digest of a chunk set: `blake3` over the sorted
/// `(chunk_id, chunk_hash)` pairs (FR-3). Order-independent by construction,
/// so a backfill that only reorders chunks does not invalidate an index while
/// any content change does.
#[must_use]
pub fn chunk_set_digest(chunks: &[(u64, String)]) -> String {
    let mut sorted: Vec<&(u64, String)> = chunks.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut hasher = blake3::Hasher::new();
    for (id, hash) in sorted {
        hash_chunk_key(&mut hasher, *id, hash.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_chunk_key(hasher: &mut blake3::Hasher, id: u64, hash: &[u8]) {
    hasher.update(&id.to_le_bytes());
    hasher.update(hash);
}

/// Hash a leased, single-project catalog pair in composite ID order without
/// collecting texts or allocating a corpus-wide SQL sort.
pub(crate) fn catalog_chunk_set(
    metadata: &rusqlite::Connection,
    project_id: i64,
    mut remaining_bytes: u64,
    check: impl Fn() -> Result<()>,
) -> Result<(usize, String)> {
    // The chunks (doc_id, seq) index gives the order. CROSS JOIN keeps it
    // outermost so SQLite doesn't sort the joined chunk texts.
    let mut statement = metadata
        .prepare(
            "SELECT c.doc_id, c.seq, c.text FROM chunks c CROSS JOIN docs d ON d.id = c.doc_id
             WHERE d.project_id = ?1 ORDER BY c.doc_id, c.seq",
        )
        .map_err(db_error)?;
    let mut rows = statement.query([project_id]).map_err(db_error)?;
    let mut count = 0_usize;
    let mut hash = blake3::Hasher::new();
    let row_key_bytes = (size_of::<u64>() + 2 * blake3::OUT_LEN) as u64;
    loop {
        check()?;
        let Some(row) = rows.next().map_err(db_error)? else {
            break;
        };
        let doc_id = row.get::<_, u32>(0).map_err(db_error)?;
        let seq = row.get::<_, u32>(1).map_err(db_error)?;
        let id = crate::index::tantivy_index::chunk_id(i64::from(doc_id), seq);
        let text = row
            .get_ref(2)
            .map_err(db_error)?
            .as_str()
            .map_err(|error| {
                Error::internal_with_source("read catalog chunk text for ANN observation", error)
            })?;
        // Empty texts still consume a row and its composite key and digest.
        remaining_bytes = remaining_bytes
            .checked_sub(text.len() as u64)
            .and_then(|bytes| bytes.checked_sub(row_key_bytes))
            .ok_or_else(|| Error::internal("ANN observation exceeds its input budget"))?;
        let digest = crate::vector_cache::chunk_digest(text).to_hex();
        hash_chunk_key(&mut hash, id, digest.as_bytes());
        count = count
            .checked_add(1)
            .ok_or_else(|| Error::internal("ANN chunk count overflow"))?;
    }
    Ok((count, hash.finalize().to_hex().to_string()))
}

/// On-disk store for ANN indexes, rooted at a cache root (design §4.1).
///
/// Layout (design §5):
/// `<root>/projects/<project_id>/ann/<key>/index.hnsw.{graph,data} meta.json READY`
#[derive(Debug, Clone)]
pub struct AnnStore {
    root: PathBuf,
    keep_generations: usize,
    slot_pins: Arc<Mutex<HashMap<PathBuf, SlotUse>>>,
}

#[derive(Debug)]
enum SlotUse {
    Build,
    Readers(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnSlotAccess {
    Read,
    Build,
}

/// Owns one in-process claim on a persisted slot through load or residency.
pub(crate) struct AnnSlotPin {
    path: PathBuf,
    pins: Arc<Mutex<HashMap<PathBuf, SlotUse>>>,
}

impl AnnSlotPin {
    pub(crate) fn finish_build(self) -> Self {
        self.pins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.path.clone(), SlotUse::Readers(1));
        self
    }
}

impl Drop for AnnSlotPin {
    fn drop(&mut self) {
        let mut pins = self.pins.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(SlotUse::Readers(count)) = pins.get_mut(&self.path)
            && *count > 1
        {
            *count -= 1;
        } else {
            pins.remove(&self.path);
        }
    }
}

impl AnnStore {
    /// Store rooted at `root` (typically the cache root).
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self::with_retention(root, super::dense_settings::DEFAULT_ANN_KEEP_GENERATIONS)
    }

    pub(crate) fn with_retention(root: PathBuf, keep_generations: usize) -> Self {
        Self {
            root,
            keep_generations,
            slot_pins: Arc::default(),
        }
    }

    /// Directory containing all ANN indexes of a project.
    #[must_use]
    pub fn project_ann_root(&self, project_id: i64) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id.to_string())
            .join("ann")
    }

    /// The cache root this store is rooted at.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Final directory for `key`; unique per key by construction.
    #[must_use]
    pub fn ann_dir(&self, key: &AnnKey) -> PathBuf {
        self.ann_dir_with_settings(key, AnnBuildSettings::owner_defaults())
    }

    /// Final directory for a key and checked graph construction recipe.
    #[must_use]
    pub fn ann_dir_with_settings(&self, key: &AnnKey, settings: AnnBuildSettings) -> PathBuf {
        self.artifact_dir(key.into(), settings)
    }

    fn artifact_dir(&self, artifact: AnnArtifact<'_>, settings: AnnBuildSettings) -> PathBuf {
        self.project_ann_root(artifact.key.project_id)
            .join(dir_name(artifact, settings))
    }

    pub(crate) fn pin_slot<'a>(
        &self,
        artifact: impl Into<AnnArtifact<'a>>,
        settings: AnnBuildSettings,
        access: AnnSlotAccess,
    ) -> Option<AnnSlotPin> {
        let path = self.artifact_dir(artifact.into(), settings);
        let mut pins = self
            .slot_pins
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match pins.get_mut(&path) {
            Some(SlotUse::Readers(count)) if access == AnnSlotAccess::Read => *count += 1,
            Some(_) => return None,
            None => {
                let slot_use = match access {
                    AnnSlotAccess::Read => SlotUse::Readers(1),
                    AnnSlotAccess::Build => SlotUse::Build,
                };
                pins.insert(path.clone(), slot_use);
            }
        }
        Some(AnnSlotPin {
            path,
            pins: self.slot_pins.clone(),
        })
    }

    fn remove_unpinned(&self, path: &Path) -> bool {
        // Serialize the per-slot check and delete with pin acquisition.
        // The filesystem scan and graph loading run outside this lock.
        let pins = self
            .slot_pins
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !pins.contains_key(path) && fs::remove_dir_all(path).is_ok()
    }

    /// Loads the index for `key`, if present and valid.
    ///
    /// Returns `Ok(None)` when the index is missing, incomplete (no `READY`),
    /// stale, corrupt or oversized — the caller falls back to exact search
    /// (FR-5). Only unexpected filesystem failures surface as `Err`.
    ///
    /// # Errors
    /// None for invalid/missing indexes (those are `Ok(None)`); filesystem
    /// failures while scanning the ann root are still reported.
    pub fn load(&self, key: &AnnKey) -> Result<Option<AnnSlot>> {
        self.load_with_settings(key, AnnBuildSettings::owner_defaults())
    }

    /// Loads an existing slot for the requested graph construction recipe.
    /// Invalid or incompatible slots return `Ok(None)`.
    ///
    /// # Errors
    /// Unexpected filesystem failures, as in [`Self::load`].
    pub fn load_with_settings(
        &self,
        key: &AnnKey,
        settings: AnnBuildSettings,
    ) -> Result<Option<AnnSlot>> {
        Ok(self.load_artifact(key.into(), settings))
    }

    pub(crate) fn load_artifact(
        &self,
        artifact: AnnArtifact<'_>,
        settings: AnnBuildSettings,
    ) -> Option<AnnSlot> {
        match self.read_artifact(artifact, settings) {
            Ok(slot) => slot.map(|(slot, _)| slot),
            Err(reason) => {
                reject(artifact.key, reason);
                None
            }
        }
    }

    /// Observe published provenance and lengths under the same slot pin used
    /// by a loader. The graph and vector data are never opened.
    pub(crate) fn observe_artifact(
        &self,
        artifact: AnnArtifact<'_>,
        settings: AnnBuildSettings,
    ) -> Option<(usize, u64)> {
        let _pin = self.pin_slot(artifact, settings, AnnSlotAccess::Read)?;
        let (slot, bytes) = self.read_artifact(artifact, settings).ok()??;
        Some((slot.meta.count, bytes))
    }

    pub(crate) fn has_observation_candidate(
        &self,
        index: ProjectIndex,
        model_id: &str,
        dimension: usize,
        settings: AnnBuildSettings,
        check: impl Fn() -> Result<()>,
    ) -> Result<bool> {
        check()?;
        let catalog = index
            .catalog_generation()
            .map(|(epoch, generation)| AnnCatalogGeneration { epoch, generation });
        let prefix = slot_prefix(model_id, index.content_revision(), catalog, dimension);
        let suffix = recipe_suffix(settings);
        let entries = match fs::read_dir(self.project_ann_root(index.project_id())) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::internal_with_source("read ANN slot names", error)),
        };
        for entry in entries {
            check()?;
            let entry =
                entry.map_err(|error| Error::internal_with_source("read ANN slot names", error))?;
            let name = entry.file_name();
            if let Some((count, digest)) = name
                .to_str()
                .and_then(|name| name.strip_prefix(&prefix))
                .and_then(|rest| rest.strip_suffix(&suffix))
                .and_then(|slot| slot.split_once('-'))
                && count.parse::<usize>().is_ok_and(|count| count > 0)
                && digest.len() == 8
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
                && ordinary_size(&entry.path().join("READY")).is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn read_artifact(
        &self,
        artifact: AnnArtifact<'_>,
        settings: AnnBuildSettings,
    ) -> std::result::Result<Option<(AnnSlot, u64)>, &'static str> {
        let dir = self.artifact_dir(artifact, settings);
        if !dir.is_dir() || ordinary_size(&dir.join("READY")).is_none() {
            return Ok(None);
        }
        let file = open_readonly_nofollow(&dir.join("meta.json"))
            .map_err(|_| "meta.json is unreadable")?;
        if !file.metadata().is_ok_and(|meta| is_regular_nofollow(&meta)) {
            return Err("meta.json is not an ordinary file");
        }
        let raw = read_bounded(file, crate::limits::MAX_STATE_BYTES)
            .map_err(|_| "meta.json is unreadable or oversized")?;
        let StoredAnnMeta { meta, catalog } =
            serde_json::from_slice(&raw).map_err(|_| "meta.json is not valid JSON")?;
        if catalog != artifact.catalog || !meta.matches_key(artifact.key) {
            return Err("metadata does not match the requested key");
        }
        if meta.m != settings.m() || meta.ef_construction != settings.ef_construction() {
            return Err("metadata does not match the graph construction settings");
        }
        if meta.hnsw_rs_version != HNSW_RS_VERSION {
            return Err("hnsw_rs version mismatch");
        }
        let bytes = dump_file_bytes(&dir).ok_or("dump files missing, empty or oversized")?;
        Ok(Some((AnnSlot { dir, meta }, bytes)))
    }

    /// Atomically publishes a dump written by `write` into a fresh temporary
    /// directory: files, `meta.json`, `READY`, then rename to the final key
    /// directory (design §5.1). A crash at any point leaves no valid index.
    ///
    /// `write` receives the temporary directory and must return the dump
    /// basename; anything other than [`DUMP_BASENAME`] fails the publish.
    ///
    /// # Errors
    /// Dump write failure, wrong basename, or filesystem failure during
    /// metadata/`READY` writing or rename.
    pub fn persist_dump(
        &self,
        key: &AnnKey,
        write: impl FnOnce(&Path) -> Result<String>,
    ) -> Result<AnnSlot> {
        self.persist_artifact(key.into(), AnnBuildSettings::owner_defaults(), write)
    }

    pub(crate) fn persist_artifact(
        &self,
        artifact: AnnArtifact<'_>,
        settings: AnnBuildSettings,
        write: impl FnOnce(&Path) -> Result<String>,
    ) -> Result<AnnSlot> {
        self.persist_dump_at(artifact, settings, nanos(), write)
    }

    fn persist_dump_at(
        &self,
        artifact: AnnArtifact<'_>,
        settings: AnnBuildSettings,
        clock_tick: u128,
        write: impl FnOnce(&Path) -> Result<String>,
    ) -> Result<AnnSlot> {
        let key = artifact.key;
        let ann_root = self.project_ann_root(key.project_id);
        fs::create_dir_all(&ann_root)
            .map_err(|err| Error::internal_with_source("create ann root", err))?;
        // Recipe builds may overlap under the same project root even when
        // the clock returns the same tick to both workers.
        let sequence = STAGING_SEQUENCE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Error::internal("ANN staging sequence exhausted"))?;
        let tmp = ann_root.join(format!(
            "tmp-{}-{clock_tick:x}-{sequence:x}",
            std::process::id()
        ));
        fs::create_dir(&tmp)
            .map_err(|err| Error::internal_with_source("create ann temp dir", err))?;
        let basename = match write(&tmp) {
            Ok(basename) => basename,
            Err(err) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(err);
            }
        };
        if basename != DUMP_BASENAME {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::internal(format!(
                "ann dump basename {basename:?}, expected {DUMP_BASENAME:?}"
            )));
        }
        let stored = StoredAnnMeta {
            meta: AnnMeta::for_settings(key, settings),
            catalog: artifact.catalog,
        };
        let publish = (|| -> Result<AnnSlot> {
            let meta_json = serde_json::to_string_pretty(&stored)
                .map_err(|err| Error::internal_with_source("serialize ann meta", err))?;
            fs::write(tmp.join("meta.json"), meta_json)
                .map_err(|err| Error::internal_with_source("write ann meta", err))?;
            sync_dump_files(&tmp)?;
            fs::write(tmp.join("READY"), "")
                .map_err(|err| Error::internal_with_source("write ann READY", err))?;
            let final_dir = self.artifact_dir(artifact, settings);
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir)
                    .map_err(|err| Error::internal_with_source("replace ann index", err))?;
            }
            fs::rename(&tmp, &final_dir)
                .map_err(|err| Error::internal_with_source("publish ann index", err))?;
            Ok(AnnSlot {
                dir: final_dir,
                meta: stored.meta,
            })
        })();
        if publish.is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
        publish
    }

    /// Removes every ANN index of a project (invalidation, FR-4).
    ///
    /// # Errors
    /// Filesystem failures while removing the tree.
    pub fn invalidate(&self, project_id: i64) -> Result<()> {
        let root = self.project_ann_root(project_id);
        match fs::remove_dir_all(&root) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::internal_with_source("remove ann index tree", err)),
        }
    }

    /// Best-effort global disk budget: removes the oldest index directories
    /// across all projects until their total size fits `max_bytes` (keeping at
    /// least one directory). Loaded indexes on Windows may fail to delete;
    /// those removals are skipped.
    ///
    /// # Errors
    /// Filesystem failures while scanning the cache root.
    pub fn cleanup_disk(&self, max_bytes: u64, keep_project: Option<i64>) -> Result<()> {
        let projects = self.root.join("projects");
        let entries = match fs::read_dir(&projects) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(Error::internal_with_source("read projects dir", err)),
        };
        let mut dirs: Vec<(u64, SystemTime, PathBuf)> = Vec::new();
        for project in entries {
            let Ok(project) = project else { continue };
            let project_id = project.file_name().to_string_lossy().parse::<i64>().ok();
            if project_id.is_some() && project_id == keep_project {
                continue;
            }
            let ann = project.path().join("ann");
            let Ok(entries) = fs::read_dir(&ann) else {
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else { continue };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("tmp-") || !entry.path().is_dir() {
                    continue;
                }
                let size = dir_size(&entry.path());
                let modified = fs::metadata(entry.path())
                    .and_then(|meta| meta.modified())
                    .unwrap_or(UNIX_EPOCH);
                dirs.push((size, modified, entry.path()));
            }
        }
        let mut total = dirs.iter().map(|(size, _, _)| *size).sum::<u64>();
        if total <= max_bytes || dirs.len() <= 1 {
            return Ok(());
        }
        dirs.sort_by_key(|(_, modified, path)| (*modified, path.clone()));
        let mut remaining = dirs.len();
        for (size, _, path) in dirs {
            if total <= max_bytes || remaining <= 1 {
                break;
            }
            if self.remove_unpinned(&path) {
                total = total.saturating_sub(size);
                remaining -= 1;
            }
        }
        Ok(())
    }

    /// Best-effort cleanup: removes stale `tmp-*` directories and keeps the
    /// configured number of newest distinct generations per model (design §9).
    /// Each retained generation can contain multiple recipes.
    ///
    /// # Errors
    /// Filesystem failures while scanning the ann root (individual removals
    /// are best-effort and never fail the call).
    pub fn cleanup(&self, project_id: i64) -> Result<()> {
        let root = self.project_ann_root(project_id);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(Error::internal_with_source("read ann index dir", err));
            }
        };
        let current_tmp = format!("tmp-{}-", std::process::id());
        let mut by_model = HashMap::<_, Vec<_>>::new();
        let mut tmp_dirs: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("tmp-") {
                // Never delete the temp dir of a build running in this
                // process; stale dirs from crashed processes are fair game.
                if !name.starts_with(&current_tmp) {
                    tmp_dirs.push(entry.path());
                }
            } else if let Some((model, revision)) = parse_slot_revision(&name) {
                by_model
                    .entry((model.to_owned(), revision.epoch))
                    .or_default()
                    .push((revision.order, entry.path()));
            }
        }
        for dir in tmp_dirs {
            let _ = fs::remove_dir_all(dir);
        }
        for generations in by_model.values_mut() {
            generations.sort_unstable_by_key(|(generation, _)| *generation);
            let expired = generations
                .chunk_by(|older, newer| older.0 == newer.0)
                .rev()
                .skip(self.keep_generations)
                .flatten();
            for (_, dir) in expired {
                self.remove_unpinned(dir);
            }
        }
        Ok(())
    }
}

/// Recursive directory size in bytes (best-effort: unreadable entries are 0).
fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                fs::metadata(path).map_or(0, |meta| meta.len())
            }
        })
        .sum()
}

/// Logs why a present index was rejected, so a corrupted persisted index
/// degrades to exact with diagnostics (FR-9, SC-4).
fn reject(key: &AnnKey, reason: &str) {
    eprintln!(
        "warning: ann index rejected (project {}, generation {}, model {}, dim {}, count {}): {reason}; falling back to exact",
        key.project_id, key.generation, key.model_id, key.dim, key.chunk_count
    );
}

/// Default construction uses the original slot name (design §5).
fn dir_name(artifact: AnnArtifact<'_>, settings: AnnBuildSettings) -> String {
    let key = artifact.key;
    let prefix = slot_prefix(&key.model_id, key.generation, artifact.catalog, key.dim);
    let digest8: String = key.chunk_set_digest.chars().take(8).collect();
    let suffix = recipe_suffix(settings);
    format!("{prefix}{}-{digest8}{suffix}", key.chunk_count)
}

fn slot_prefix(
    model_id: &str,
    revision: i64,
    catalog: Option<AnnCatalogGeneration>,
    dimension: usize,
) -> String {
    use std::fmt::Write as _;

    let model_hash: String = blake3::hash(model_id.as_bytes())
        .to_hex()
        .chars()
        .take(16)
        .collect();
    let mut name = format!("{model_hash}-g{revision}");
    if let Some(catalog) = catalog {
        let epoch = u128::from_be_bytes(catalog.epoch);
        let _ = write!(name, "-v5.{epoch:032x}.{}", catalog.generation);
    }
    let _ = write!(name, "-d{dimension}-c");
    name
}

fn recipe_suffix(settings: AnnBuildSettings) -> String {
    if settings == LEGACY_BUILD_RECIPE {
        String::new()
    } else {
        format!(
            "-r{ANN_RECIPE_VERSION}-m{}-ef{}",
            settings.m(),
            settings.ef_construction()
        )
    }
}

struct SlotRevision {
    epoch: Option<u128>,
    order: (i64, i64),
}

fn parse_slot_revision(name: &str) -> Option<(&str, SlotRevision)> {
    let (model, rest) = name.split_once("-g")?;
    if model.is_empty() {
        return None;
    }
    let mut fields = rest.split('-');
    let revision = fields.next()?.parse().ok()?;
    let catalog = fields.next().and_then(|field| field.strip_prefix("v5."));
    let snapshot = if let Some(catalog) = catalog {
        let (epoch, generation) = catalog.split_once('.')?;
        let generation: i64 = generation.parse().ok()?;
        if epoch.len() != 32 || generation <= 0 {
            return None;
        }
        SlotRevision {
            epoch: Some(u128::from_str_radix(epoch, 16).ok()?),
            order: (generation, revision),
        }
    } else {
        SlotRevision {
            epoch: None,
            order: (0, revision),
        }
    };
    Some((model, snapshot))
}

fn ordinary_size(path: &Path) -> Option<u64> {
    let meta = fs::symlink_metadata(path).ok()?;
    is_regular_nofollow(&meta).then_some(meta.len())
}

/// Combined size of the required ordinary dump files.
fn dump_file_bytes(dir: &Path) -> Option<u64> {
    [
        format!("{DUMP_BASENAME}.hnsw.graph"),
        format!("{DUMP_BASENAME}.hnsw.data"),
    ]
    .into_iter()
    .map(|name| {
        ordinary_size(&dir.join(name)).filter(|size| (1..=MAX_ANN_FILE_BYTES).contains(size))
    })
    .sum()
}

/// `sync_all` on the dump files before the `READY` marker is written
/// (NFR-5).
fn sync_dump_files(dir: &Path) -> Result<()> {
    for name in [
        format!("{DUMP_BASENAME}.hnsw.graph"),
        format!("{DUMP_BASENAME}.hnsw.data"),
        "meta.json".to_owned(),
    ] {
        // Windows `FlushFileBuffers` (used by `sync_all`) requires a
        // GENERIC_WRITE handle; a read-only `File::open` fails there.
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(name))
            .map_err(|err| Error::internal_with_source("open ann dump file", err))?;
        file.sync_all()
            .map_err(|err| Error::internal_with_source("sync ann dump file", err))?;
    }
    Ok(())
}

/// Unix seconds, or 0 when the clock is unreliable.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

/// Clock component of a temporary directory name, in nanoseconds.
fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_key(revision: i64) -> AnnKey {
        AnnKey {
            project_id: 1,
            generation: revision,
            model_id: "model@d2".into(),
            dim: 2,
            chunk_count: 2,
            chunk_set_digest: "digest".into(),
            format_version: ANN_FORMAT_VERSION,
        }
    }

    fn catalog_artifact(key: &AnnKey, epoch: u8, generation: i64) -> AnnArtifact<'_> {
        AnnArtifact {
            key,
            catalog: Some(AnnCatalogGeneration {
                epoch: [epoch; 16],
                generation,
            }),
        }
    }

    fn graph() -> crate::hybrid::ann::AnnIndex {
        crate::hybrid::ann::AnnIndex::build(
            &[(11, Arc::from([1.0, 0.0])), (12, Arc::from([0.0, 1.0]))],
            2,
        )
        .unwrap()
    }

    #[test]
    fn catalog_digest_charges_all_rows_and_checks_cancellation() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, project_id INTEGER);
             CREATE TABLE chunks(doc_id INTEGER, seq INTEGER, text TEXT, UNIQUE(doc_id, seq));
             INSERT INTO docs VALUES(1, 7), (2, 7), (3, 8);
             INSERT INTO chunks VALUES(2, 0, 'b'), (1, 0, 'a'), (3, 0, 'other');",
        )
        .unwrap();
        let hashes = crate::store::repo::chunk_hashes(&db, 7).unwrap();
        assert_eq!(
            catalog_chunk_set(&db, 7, 146, || Ok(())).unwrap(),
            (2, chunk_set_digest(&hashes))
        );
        assert!(catalog_chunk_set(&db, 7, 145, || Ok(())).is_err());
        let calls = std::cell::Cell::new(0);
        let result = catalog_chunk_set(&db, 7, 146, || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(Error::internal("inspection cancelled"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err().inner_message(), "inspection cancelled");
    }

    #[test]
    fn persisted_observation_respects_build_pin_and_required_dump_files() {
        let root = tempfile::tempdir().unwrap();
        let store = AnnStore::new(root.path().to_path_buf());
        let graph = graph();
        let key = graph_key(8);
        let artifact = catalog_artifact(&key, 7, 4);
        let settings = graph.build_settings();
        let slot = graph.dump_artifact(&store, artifact).unwrap();
        let builder = store
            .pin_slot(artifact, settings, AnnSlotAccess::Build)
            .unwrap();
        assert!(store.observe_artifact(artifact, settings).is_none());
        drop(builder);
        let Some((2, 1..)) = store.observe_artifact(artifact, settings) else {
            panic!("expected both persisted dump files");
        };
        let data = slot.dir.join("index.hnsw.data");
        fs::remove_file(&data).unwrap();
        assert!(store.observe_artifact(artifact, settings).is_none());
        assert!(!data.exists());
    }

    #[test]
    fn catalog_retention_orders_by_head_before_local_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = AnnStore::with_retention(root.path().to_path_buf(), 2);
        let graph = graph();
        let old_key = graph_key(99);
        let old = catalog_artifact(&old_key, 7, 1);
        let old_dump = graph.dump_artifact(&store, old).unwrap();
        let previous_key = graph_key(1);
        let previous = graph
            .dump_artifact(&store, catalog_artifact(&previous_key, 7, 2))
            .unwrap();
        let retained_key = graph_key(2);
        let retained = graph
            .dump_artifact(&store, catalog_artifact(&retained_key, 7, 2))
            .unwrap();
        let current_key = graph_key(3);
        let current = graph
            .dump_artifact(&store, catalog_artifact(&current_key, 7, 2))
            .unwrap();
        let legacy = graph.dump(&store, &old_key).unwrap();
        let pin = store
            .pin_slot(old, graph.build_settings(), AnnSlotAccess::Read)
            .unwrap();
        let mapped = crate::hybrid::ann::AnnIndex::load(&old_dump)
            .unwrap()
            .with_disk_pin(pin);

        store.cleanup(1).unwrap();
        assert!(old_dump.dir.is_dir(), "a mapped graph retains its slot");
        drop(mapped);
        store.cleanup(1).unwrap();

        assert!(
            !old_dump.dir.exists(),
            "a high local revision cannot keep a retired head"
        );
        assert!(
            !previous.dir.exists(),
            "only the two newest local revisions survive"
        );
        assert!(retained.dir.is_dir());
        assert!(current.dir.is_dir());
        assert!(legacy.dir.is_dir());
    }

    #[test]
    fn catalog_dumps_validate_installation_and_head_at_the_same_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = AnnStore::new(root.path().to_path_buf());
        let graph = graph();
        let key = graph_key(1);
        let previous = catalog_artifact(&key, 7, 1);
        let current = catalog_artifact(&key, 7, 2);
        let restored = catalog_artifact(&key, 9, 2);
        let settings = graph.build_settings();
        let old = graph.dump_artifact(&store, previous).unwrap();
        let new = graph.dump_artifact(&store, current).unwrap();
        assert_ne!(old.dir, new.dir);
        assert!(store.load(&key).unwrap().is_none());
        assert!(store.load_artifact(restored, settings).is_none());

        let loaded =
            crate::hybrid::ann::AnnIndex::load(&store.load_artifact(current, settings).unwrap())
                .unwrap();
        assert_eq!(loaded.search(&[1.0, 0.0], 1).unwrap()[0].0, 11);
        drop(loaded);
        fs::copy(old.dir.join("meta.json"), new.dir.join("meta.json")).unwrap();
        assert!(store.load_artifact(current, settings).is_none());
        assert!(store.load_artifact(previous, settings).is_some());
    }

    #[test]
    fn overlapping_recipes_with_the_same_clock_tick_have_private_staging() {
        let root = tempfile::tempdir().unwrap();
        let store = AnnStore::new(root.path().to_path_buf());
        let key = AnnKey {
            project_id: 1,
            generation: 1,
            model_id: "model@d8".into(),
            dim: 8,
            chunk_count: 1,
            chunk_set_digest: "digest".into(),
            format_version: ANN_FORMAT_VERSION,
        };
        let first = AnnBuildSettings::new(12, 64).unwrap();
        let second = AnnBuildSettings::new(16, 98).unwrap();
        let write = |dir: &Path, payload| {
            fs::write(dir.join("index.hnsw.graph"), payload).unwrap();
            fs::write(dir.join("index.hnsw.data"), payload).unwrap();
            Ok(DUMP_BASENAME.to_owned())
        };
        let published_first = store
            .persist_dump_at((&key).into(), first, 123, |staging_first| {
                let basename = write(staging_first, b"recipe a")?;
                let published_second =
                    store.persist_dump_at((&key).into(), second, 123, |staging_second| {
                        assert_ne!(staging_first, staging_second);
                        write(staging_second, b"recipe b")
                    })?;
                assert_eq!(
                    fs::read(published_second.dir.join("index.hnsw.graph")).unwrap(),
                    b"recipe b"
                );
                Ok(basename)
            })
            .unwrap();
        assert_eq!(
            fs::read(published_first.dir.join("index.hnsw.graph")).unwrap(),
            b"recipe a"
        );
        assert!(store.load_with_settings(&key, first).unwrap().is_some());
        assert!(store.load_with_settings(&key, second).unwrap().is_some());
    }
}
