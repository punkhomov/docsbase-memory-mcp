//! Persisted ANN storage layer (spec ann-index T2): paths, key, metadata,
//! atomic write/load, stale/corrupt rejection and generation cleanup.
//!
//! No HNSW logic lives here: a dump is written by the caller through
//! [`AnnStore::persist_dump`] and consumed later by `hybrid::ann`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// First on-disk format version (design §5).
pub const ANN_FORMAT_VERSION: u32 = 1;

/// The only v1 `hnsw_rs` version (FR-13); a dump from any other version is
/// rejected on load and rebuilt.
pub const HNNSW_RS_VERSION: &str = "0.3.5";

/// Default graph fan-out during construction (design §6).
pub const ANN_DEFAULT_M: usize = 16;

/// Default construction beam width (design §6).
pub const ANN_DEFAULT_EF_CONSTRUCTION: usize = 200;

/// Provisional per-file size ceiling; a bigger graph/data file is treated as
/// corrupt and rejected (design §6, NFR-6).
pub const MAX_ANN_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Dump basename enforced by [`AnnStore::persist_dump`] (design §6).
const DUMP_BASENAME: &str = "index";

/// Generations kept by [`AnnStore::cleanup`] (design §9).
const ANN_KEEP_GENERATIONS: usize = 2;

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
        Self {
            format: key.format_version,
            hnsw_rs_version: HNNSW_RS_VERSION.to_owned(),
            project_id: key.project_id,
            generation: key.generation,
            model_id: key.model_id.clone(),
            dim: key.dim,
            count: key.chunk_count,
            chunk_set_digest: key.chunk_set_digest.clone(),
            m: ANN_DEFAULT_M,
            ef_construction: ANN_DEFAULT_EF_CONSTRUCTION,
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
        hasher.update(&id.to_le_bytes());
        hasher.update(hash.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// On-disk store for ANN indexes, rooted at a cache root (design §4.1).
///
/// Layout (design §5):
/// `<root>/projects/<project_id>/ann/<key>/index.hnsw.{graph,data} meta.json READY`
#[derive(Debug, Clone)]
pub struct AnnStore {
    root: PathBuf,
}

impl AnnStore {
    /// Store rooted at `root` (typically the cache root).
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
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
        self.project_ann_root(key.project_id).join(dir_name(key))
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
        let dir = self.ann_dir(key);
        if !dir.is_dir() || !dir.join("READY").is_file() {
            return Ok(None);
        }
        let meta: AnnMeta = match fs::read_to_string(dir.join("meta.json")) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(meta) => meta,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };
        if !meta.matches_key(key)
            || meta.hnsw_rs_version != HNNSW_RS_VERSION
            || !dump_sizes_ok(&dir)
        {
            return Ok(None);
        }
        Ok(Some(AnnSlot { dir, meta }))
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
        let ann_root = self.project_ann_root(key.project_id);
        let tmp = ann_root.join(format!("tmp-{}-{}", std::process::id(), nanos()));
        fs::create_dir_all(&tmp)
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
        let meta = AnnMeta::new(key);
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|err| Error::internal_with_source("serialize ann meta", err))?;
        fs::write(tmp.join("meta.json"), meta_json)
            .map_err(|err| Error::internal_with_source("write ann meta", err))?;
        sync_dump_files(&tmp)?;
        fs::write(tmp.join("READY"), "")
            .map_err(|err| Error::internal_with_source("write ann READY", err))?;
        let final_dir = self.ann_dir(key);
        if final_dir.exists() {
            fs::remove_dir_all(&final_dir)
                .map_err(|err| Error::internal_with_source("replace ann index", err))?;
        }
        fs::rename(&tmp, &final_dir)
            .map_err(|err| Error::internal_with_source("publish ann index", err))?;
        Ok(AnnSlot {
            dir: final_dir,
            meta,
        })
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

    /// Best-effort cleanup: removes stale `tmp-*` directories and keeps only
    /// the newest [`ANN_KEEP_GENERATIONS`] generations per project (design §9).
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
        let mut generations: Vec<(i64, PathBuf)> = Vec::new();
        let mut tmp_dirs: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("tmp-") {
                tmp_dirs.push(entry.path());
            } else if let Some(generation) = parse_generation(&name) {
                generations.push((generation, entry.path()));
            }
        }
        for dir in tmp_dirs {
            let _ = fs::remove_dir_all(dir);
        }
        generations.sort_by_key(|(generation, _)| *generation);
        let remove_count = generations.len().saturating_sub(ANN_KEEP_GENERATIONS);
        for (_, dir) in generations.into_iter().take(remove_count) {
            let _ = fs::remove_dir_all(dir);
        }
        Ok(())
    }
}

/// Directory name of a key: `<model_hash>-g<generation>-d<dim>-c<count>-<digest8>`
/// (design §5).
fn dir_name(key: &AnnKey) -> String {
    let model_hash: String = blake3::hash(key.model_id.as_bytes())
        .to_hex()
        .chars()
        .take(16)
        .collect();
    let digest8: String = key.chunk_set_digest.chars().take(8).collect();
    format!(
        "{model_hash}-g{}-d{}-c{}-{digest8}",
        key.generation, key.dim, key.chunk_count
    )
}

/// Parses the generation out of a key directory name.
fn parse_generation(name: &str) -> Option<i64> {
    let start = name.find("-g")? + 2;
    let rest = &name[start..];
    let end = rest.find('-').unwrap_or(rest.len());
    rest[..end].parse::<i64>().ok()
}

/// Whether every dump file exists and stays within [`MAX_ANN_FILE_BYTES`].
fn dump_sizes_ok(dir: &Path) -> bool {
    [
        format!("{DUMP_BASENAME}.hnsw.graph"),
        format!("{DUMP_BASENAME}.hnsw.data"),
    ]
    .iter()
    .all(|name| {
        fs::metadata(dir.join(name))
            .map(|meta| meta.len() <= MAX_ANN_FILE_BYTES)
            .unwrap_or(false)
    })
}

/// `sync_all` on the dump files before the `READY` marker is written
/// (NFR-5).
fn sync_dump_files(dir: &Path) -> Result<()> {
    for name in [
        format!("{DUMP_BASENAME}.hnsw.graph"),
        format!("{DUMP_BASENAME}.hnsw.data"),
        "meta.json".to_owned(),
    ] {
        let file = fs::File::open(dir.join(name))
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
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

/// Nanosecond tick for unique temporary directory names.
fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
