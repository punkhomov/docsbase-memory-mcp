//! Local `fastembed` embedder/reranker (ADR-17, spec hybrid-local).
//!
//! The embedder (T4) loads the fetched registry model through the BYO path
//! (`UserDefinedEmbeddingModel` + `TextEmbedding::try_new_from_user_defined`),
//! deduplicates instances per cache key, holds a shared `.lock` guard on the
//! model cache for its lifetime (so `models remove` refuses while loaded),
//! and applies role-aware registry prefixes (`embed_query` vs `embed`).
//! The reranker (T5) uses the same cache-key/lock discipline with
//! `UserDefinedRerankingModel` + `OnnxSource::Memory` (verified load-time
//! bytes, round-4 C4).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fd_lock::RwLockReadGuard;
use sha2::Digest;

use crate::error::{Error, Result};
use crate::hybrid::models::{self, ModelEntry, Pooling, Task};
use crate::vector_cache::Embedder;

use super::Reranker;
use super::config::{EmbeddingCfg, ProvidersCfg, RerankerCfg};

/// Default embedding max length (NFR-4).
pub const DEFAULT_MAX_LENGTH: usize = 512;

/// Batch size passed to fastembed inference (NFR-4).
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// `intra_threads` cap (NFR-4): `min(4, cores)`.
fn capped_intra_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    cores.min(4)
}

/// One model instance per cache key (NFR-4/NFR-7): canonical model id +
/// `max_length` + `intra_threads` + runtime path. The process-wide map
/// lives in [`load_embedder`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelCacheKey {
    /// Canonical model id (`embedding_model_id`).
    pub model_id: String,
    /// Tokenizer max length.
    pub max_length: usize,
    /// Capped intra-op threads.
    pub intra_threads: Option<usize>,
    /// Resolved runtime library path.
    pub runtime: PathBuf,
}

/// Builds the local embedding stage (FR-9, FR-12): provisioning is checked
/// first (runtime → connect hint, model → fetch hint), then the model is
/// loaded once per cache key.
///
/// # Errors
/// Returns [`Error::Admission`] for missing runtime/model (with exactly one
/// corrective action) and [`Error::Internal`] for inference/session load
/// failures after a successful load.
pub fn embedder(cfg: &EmbeddingCfg, providers: &ProvidersCfg) -> Result<Arc<dyn Embedder>> {
    let entry = models::entry(&cfg.model)?;
    if entry.task != Task::Embedding {
        return Err(Error::Admission {
            message: format!(
                "hybrid.embedding: \"{}\" is a rerank model, not an embedding model; pick an embedding model from `docsbase models list`",
                cfg.model
            ),
        });
    }
    // Provisioning (FR-12): runtime first, model second.
    let runtime = super::runtime::resolve(providers)?;
    if !models::cached(entry)? {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" is not fetched; run `docsbase models fetch {}`",
                cfg.model, cfg.model
            ),
        });
    }
    let key = ModelCacheKey {
        model_id: super::config::embedding_model_id(cfg),
        max_length: DEFAULT_MAX_LENGTH,
        intra_threads: Some(capped_intra_threads()),
        runtime,
    };
    load_embedder(entry, &key)
}

/// Process-wide embedder cache keyed by [`ModelCacheKey`] (one instance per
/// key; never per config/call, NFR-4/NFR-7). The lock is held only for the
/// lookup and the final insert — model construction happens outside it so
/// concurrent loads of the same key serialize on the work, not on the map.
fn load_embedder(entry: &ModelEntry<'_>, key: &ModelCacheKey) -> Result<Arc<dyn Embedder>> {
    get_or_load_embedder(key.clone(), || {
        FastEmbedEmbedder::load(entry, key).map(|embedder| Arc::new(embedder) as Arc<dyn Embedder>)
    })
}

/// Serializes model construction (cold path only, C11): without it, two
/// cold same-key callers could both run `load()` (two ORT sessions, two
/// shared-lock attempts) and the re-check would only discard the loser.
/// The lock is never held while the instance maps are locked, so there is
/// no lock-order cycle with `embedder_instances`/`reranker_instances`.
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Core process-wide per-key instance cache: returns the existing instance
/// for `key`, or constructs one with `load` and registers it. The single
/// generic implementation behind both the embedder and reranker maps (one
/// instance per key, NFR-4/NFR-7); the map itself is provided per-kind.
fn get_or_load_by_key<T: ?Sized>(
    key: ModelCacheKey,
    instances: &'static Mutex<std::collections::HashMap<ModelCacheKey, Arc<T>>>,
    load: impl FnOnce() -> Result<Arc<T>>,
) -> Result<Arc<T>> {
    {
        let map = instances
            .lock()
            .map_err(|poison| Error::internal(format!("instance cache lock poisoned: {poison}")))?;
        if let Some(existing) = map.get(&key) {
            return Ok(Arc::clone(existing));
        }
    }
    // Serialize construction; re-check after acquiring so a waiter reuses
    // the instance the winner registered. `LOAD_LOCK` guards no protected
    // data, so a poisoned lock (a panicking loader) is meaningless — reuse
    // it rather than failing every later construction (round-2 D4).
    let _construction_guard = LOAD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let map = instances
            .lock()
            .map_err(|poison| Error::internal(format!("instance cache lock poisoned: {poison}")))?;
        if let Some(existing) = map.get(&key) {
            return Ok(Arc::clone(existing));
        }
    }
    let shared = load()?;
    let mut map = instances
        .lock()
        .map_err(|poison| Error::internal(format!("instance cache lock poisoned: {poison}")))?;
    map.insert(key, Arc::clone(&shared));
    Ok(shared)
}

/// Embedder instance map (process-wide).
fn embedder_instances()
-> &'static Mutex<std::collections::HashMap<ModelCacheKey, Arc<dyn Embedder>>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static INSTANCES: OnceLock<Mutex<HashMap<ModelCacheKey, Arc<dyn Embedder>>>> = OnceLock::new();
    INSTANCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Public test seam for the embedder dedup contract (drives the production
/// map; see `instant_cache_deduplicates_per_key`).
pub(crate) fn get_or_load_embedder(
    key: ModelCacheKey,
    load: impl FnOnce() -> Result<Arc<dyn Embedder>>,
) -> Result<Arc<dyn Embedder>> {
    get_or_load_by_key(key, embedder_instances(), load)
}

/// Local `fastembed` embedder (T4). Holds the loaded model under a `Mutex`
/// (fastembed's `embed` takes `&mut self`) and a shared model-cache `.lock`
/// guard for its lifetime.
pub struct FastEmbedEmbedder {
    entry: ModelEntry<'static>,
    model_id: String,
    model: Mutex<fastembed::TextEmbedding>,
    /// Shared `.lock` guard on `<cache>/<name>/.lock`, held for the
    /// instance lifetime (FR-7, T9 remove refuses while loaded).
    _lock_guard: RwLockReadGuard<'static, std::fs::File>,
}

impl FastEmbedEmbedder {
    /// Loads the model from the verified cache. The fastembed model is a
    /// static-typed registry row, so the onnx/tokenizer files are read from
    /// the cache directory by `files[]` paths.
    ///
    /// # Errors
    /// Admission before load (runtime/model); `Internal` after a successful
    /// load when the session/tokenizer construction fails.
    fn load(entry: &ModelEntry<'_>, key: &ModelCacheKey) -> Result<Self> {
        // The runtime must be initialized before any fastembed constructor
        // (FR-11). `init` is idempotent per process.
        super::runtime::init(&key.runtime)?;

        let dir = models::cache_dir(entry.name, entry.revision);
        let lock_guard = take_shared_lock(&dir)?;
        let cache_dir = models::cache_dir(entry.name, entry.revision);

        let onnx_path = entry
            .files
            .iter()
            .find(|spec| spec.path.starts_with("onnx/"))
            .map(|spec| cache_dir.join(spec.path))
            .ok_or_else(|| {
                Error::internal(format!("registry row {} has no onnx file", entry.name))
            })?;
        let onnx = read_cache_file(&cache_dir, &onnx_path, entry)?;
        let tokenizer_files = tokenizer_files(&cache_dir, entry)?;

        let pooling = match entry.pooling {
            Some(Pooling::Cls) => Some(fastembed::Pooling::Cls),
            Some(Pooling::Mean) => Some(fastembed::Pooling::Mean),
            None => None,
        };
        let mut model = fastembed::UserDefinedEmbeddingModel::new(onnx, tokenizer_files);
        if let Some(pooling) = pooling {
            model = model.with_pooling(pooling);
        }
        // int8 rows use Static quantization mode.
        let quantization = if entry.quantization == Some("int8") {
            fastembed::QuantizationMode::Static
        } else {
            fastembed::QuantizationMode::None
        };
        model = model.with_quantization(quantization);
        // `output_key` is a plain field (registry rows carry it for exotic
        // intermediate heads); leave the default precedence otherwise. The
        // enum needs `&'static str` — leak one copy (process-long).
        if let Some(output_key) = entry.output_key {
            let static_key: &'static str = Box::leak(output_key.to_owned().into_boxed_str());
            model.output_key = Some(fastembed::OutputKey::ByName(static_key));
        }

        let options = fastembed::InitOptionsUserDefined::new()
            .with_max_length(key.max_length)
            .with_intra_threads(key.intra_threads.unwrap_or(1));

        let model = fastembed::TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|err| Error::internal(format!("failed to load the fastembed model: {err}")))?;

        let model_id = key.model_id.clone();
        let entry = clone_entry(entry);
        Ok(Self {
            entry,
            model_id,
            model: Mutex::new(model),
            _lock_guard: lock_guard,
        })
    }

    /// Query embedding with the registry `query_prefix` (FR-10).
    fn embed_text(&self, text: &str, is_query: bool) -> Result<Vec<f32>> {
        let prefixed = apply_role_prefix(text, &self.entry, is_query);
        let mut model = self
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("embedder lock poisoned: {poison}")))?;
        let mut vectors = model
            .embed([prefixed], Some(DEFAULT_BATCH_SIZE))
            .map_err(|err| Error::internal(format!("fastembed inference failed: {err}")))?;
        vectors
            .pop()
            .ok_or_else(|| Error::internal("fastembed returned no vector for one input"))
    }
}

impl Embedder for FastEmbedEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Batch (panel C10): one mutex acquisition and one fastembed session
        // run per `DEFAULT_BATCH_SIZE` chunk instead of one per text.
        let prefixed: Vec<String> = texts
            .iter()
            .map(|text| apply_role_prefix(text, &self.entry, false))
            .collect();
        let mut model = self
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("embedder lock poisoned: {poison}")))?;
        let mut out = Vec::with_capacity(prefixed.len());
        for range in batch_chunks(prefixed.len()) {
            let chunk = &prefixed[range];
            let vectors = model
                .embed(chunk, Some(DEFAULT_BATCH_SIZE))
                .map_err(|err| Error::internal(format!("fastembed inference failed: {err}")))?;
            if vectors.len() != chunk.len() {
                return Err(Error::internal(format!(
                    "fastembed returned {} vectors for {} inputs",
                    vectors.len(),
                    chunk.len()
                )));
            }
            out.extend(vectors);
        }
        Ok(out)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_text(text, true)
    }
}

/// Builds the local rerank stage (T5, FR-5/9/12): provisioning first
/// (runtime → connect hint, model → fetch hint), then one instance per cache
/// key. A rerank model requires an explicit registry row — there is no
/// implicit/default reranker (FR-5).
///
/// # Errors
/// Returns [`Error::Admission`] for missing runtime/model (with exactly one
/// corrective action) and [`Error::Internal`] for post-load session failures.
pub fn reranker(cfg: &RerankerCfg, providers: &ProvidersCfg) -> Result<Arc<dyn Reranker>> {
    let entry = models::entry(&cfg.model)?;
    if entry.task != Task::Rerank {
        return Err(Error::Admission {
            message: format!(
                "hybrid.reranker: \"{}\" is an embedding model, not a rerank model; pick a rerank model from `docsbase models list`",
                cfg.model
            ),
        });
    }
    let runtime = super::runtime::resolve(providers)?;
    if !models::cached(entry)? {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" is not fetched; run `docsbase models fetch {}`",
                cfg.model, cfg.model
            ),
        });
    }
    // The reranker key mirrors the embedder's canonical shape minus the
    // vector-affecting components (scores are never cached; the revision is
    // build-static, so `@r<rev8>` adds nothing today). Kept as the canonical
    // `model_id` for the dedup contract (NFR-4).
    let key = ModelCacheKey {
        model_id: format!("fastembed/{}", cfg.model),
        max_length: DEFAULT_MAX_LENGTH,
        intra_threads: Some(capped_intra_threads()),
        runtime,
    };
    get_or_load_reranker(key.clone(), || {
        FastEmbedReranker::load(entry, &key).map(|reranker| Arc::new(reranker) as Arc<dyn Reranker>)
    })
}

/// Reranker instance map (process-wide).
fn reranker_instances()
-> &'static Mutex<std::collections::HashMap<ModelCacheKey, Arc<dyn Reranker>>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static INSTANCES: OnceLock<Mutex<HashMap<ModelCacheKey, Arc<dyn Reranker>>>> = OnceLock::new();
    INSTANCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Public test seam for the reranker dedup contract.
pub(crate) fn get_or_load_reranker(
    key: ModelCacheKey,
    load: impl FnOnce() -> Result<Arc<dyn Reranker>>,
) -> Result<Arc<dyn Reranker>> {
    get_or_load_by_key(key, reranker_instances(), load)
}

/// Local `fastembed` cross-encoder reranker (T5). Uses the BYO path with
/// `OnnxSource::Memory` (load-time verified bytes, round-4 C4) and holds
/// the shared model-cache `.lock` guard for its lifetime.
pub struct FastEmbedReranker {
    model: Mutex<fastembed::TextRerank>,
    _lock_guard: RwLockReadGuard<'static, std::fs::File>,
}

impl FastEmbedReranker {
    /// Loads the rerank model from the verified cache.
    ///
    /// # Errors
    /// Admission before load (runtime/model); `Internal` after a successful
    /// load when session/tokenizer construction fails.
    fn load(entry: &ModelEntry<'_>, key: &ModelCacheKey) -> Result<Self> {
        super::runtime::init(&key.runtime)?;
        let cache_dir = models::cache_dir(entry.name, entry.revision);
        let lock_guard = take_shared_lock(&cache_dir)?;

        let onnx_path = entry
            .files
            .iter()
            .find(|spec| spec.path.starts_with("onnx/"))
            .map(|spec| cache_dir.join(spec.path))
            .ok_or_else(|| {
                Error::internal(format!("registry row {} has no onnx file", entry.name))
            })?;
        // Verify the exact byte by fed to ORT (round-4 C4): a `File`-backed
        // source would let fastembed re-open the path unverified. Memory
        // source mirrors the embedder path and costs one extra buffer while
        // the session builds (the buffer drops after construction).
        let onnx = read_cache_file(&cache_dir, &onnx_path, entry)?;
        let tokenizer_files = tokenizer_files(&cache_dir, entry)?;
        let model = fastembed::UserDefinedRerankingModel::new(
            fastembed::OnnxSource::Memory(onnx),
            tokenizer_files,
        );
        let options = fastembed::RerankInitOptionsUserDefined::new()
            .with_max_length(key.max_length)
            .with_intra_threads(key.intra_threads.unwrap_or(1));
        let model =
            fastembed::TextRerank::try_new_from_user_defined(model, options).map_err(|err| {
                Error::internal(format!("failed to load the fastembed reranker: {err}"))
            })?;
        Ok(Self {
            model: Mutex::new(model),
            _lock_guard: lock_guard,
        })
    }
}

impl Reranker for FastEmbedReranker {
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        let mut model = self
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("reranker lock poisoned: {poison}")))?;
        let docs_ref: Vec<&str> = docs.iter().map(String::as_str).collect();
        let results = model
            .rerank(query, &docs_ref, false, Some(DEFAULT_BATCH_SIZE))
            .map_err(|err| Error::internal(format!("fastembed rerank inference failed: {err}")))?;
        Ok(rerank_scores(&results, top_n))
    }
}

/// Maps fastembed results to the `(index, score)` contract, best first, and
/// clamps to `top_n`. Pure and unit-tested (live inference is T8).
fn rerank_scores(results: &[fastembed::RerankResult], top_n: usize) -> Vec<(usize, f32)> {
    let mut scores: Vec<(usize, f32)> = results
        .iter()
        .map(|result| (result.index, result.score))
        .collect();
    // fastembed already sorts descending, but the mapping contract is ours:
    // sort defensively so an upstream reorder can never invert rankings.
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    scores.truncate(top_n);
    scores
}

/// Reads the four tokenizer files from the cache directory into fastembed's
/// [`fastembed::TokenizerFiles`].
fn tokenizer_files(cache_dir: &Path, entry: &ModelEntry<'_>) -> Result<fastembed::TokenizerFiles> {
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = cache_dir.join(name);
        read_cache_file(cache_dir, &path, entry)
    };
    Ok(fastembed::TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    })
}

/// Splits `len` items into `DEFAULT_BATCH_SIZE`-sized index ranges (panel
/// C10). Pure and unit-tested; `FastEmbedEmbedder::embed` runs one session
/// call per range.
fn batch_chunks(len: usize) -> Vec<std::ops::Range<usize>> {
    (0..len)
        .step_by(DEFAULT_BATCH_SIZE)
        .map(|start| start..(start + DEFAULT_BATCH_SIZE).min(len))
        .collect()
}

/// Applies the registry role prefix (FR-10): queries get `query_prefix`,
/// passages get `passage_prefix`. Pure and unit-tested (SC-9) — inference
/// itself is live-verified in T8.
fn apply_role_prefix(text: &str, entry: &ModelEntry<'_>, is_query: bool) -> String {
    let prefix = if is_query {
        entry.query_prefix
    } else {
        entry.passage_prefix
    };
    if prefix.is_empty() {
        text.to_owned()
    } else {
        format!("{prefix}{text}")
    }
}

/// Reads one model cache file and verifies it against the registry spec
/// (exact size + sha256, round-4 C4). The `cached()` gate and its
/// size+mtime memo are advisory for availability; the bytes actually fed
/// to ORT/tokenizer must be the pinned ones — a same-size tamper (or a
/// check→read TOCTOU) can otherwise load unverified content. Mismatch is
/// a corrupted-cache condition → `Admission` with the fetch action
/// (FR-12), never a silent internal error.
fn read_cache_file(cache_dir: &Path, path: &Path, entry: &ModelEntry<'_>) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|err| Error::Admission {
        message: format!(
            "fastembed model \"{}\" cache file {} is unreadable ({}); re-run `docsbase models fetch {}`",
            entry.name,
            path.display(),
            err,
            entry.name
        ),
    })?;
    let spec = entry
        .files
        .iter()
        .find(|spec| cache_dir.join(spec.path) == path)
        .ok_or_else(|| {
            Error::internal(format!(
                "registry row {} has no file spec for {}",
                entry.name,
                path.display()
            ))
        })?;
    if bytes.len() as u64 != spec.size {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" cache file {} has size {} (registry says {}); re-run `docsbase models fetch {}`",
                entry.name,
                path.display(),
                bytes.len(),
                spec.size,
                entry.name
            ),
        });
    }
    let digest = models::hex(&sha2::Sha256::digest(&bytes));
    if digest != spec.sha256 {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" cache file {} is corrupted (sha256 mismatch); re-run `docsbase models fetch {} --force`",
                entry.name,
                path.display(),
                entry.name
            ),
        });
    }
    Ok(bytes)
}

/// Takes a shared read lock on the model root's `.lock` (fd-lock
/// `try_read`); the guard is retained by the instance for its lifetime
/// (and is what `models remove` probes exclusively, FR-7). The lock lives
/// at the model root, NOT inside the revision dir, so it survives a fetch
/// promote (round-2 D2).
pub(crate) fn take_shared_lock(dir: &Path) -> Result<RwLockReadGuard<'static, std::fs::File>> {
    let root = dir
        .parent()
        .ok_or_else(|| Error::internal(format!("cache path has no parent: {}", dir.display())))?;
    let path = root.join(".lock");
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => file,
        // The model root vanished between the provisioning check and here
        // (a concurrent `models remove`, or never fetched): surface the
        // fetch hint instead of an Internal error (round-2 M5).
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let name = root.file_name().map_or_else(
                || dir.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            return Err(Error::Admission {
                message: format!(
                    "fastembed model \"{name}\" is not fetched; run `docsbase models fetch {name}`"
                ),
            });
        }
        Err(err) => {
            return Err(Error::internal_with_source(
                format!("open {}", path.display()),
                err,
            ));
        }
    };
    let lock: &'static mut fd_lock::RwLock<std::fs::File> =
        Box::leak(Box::new(fd_lock::RwLock::new(file)));
    match lock.try_read() {
        Ok(guard) => Ok(guard),
        Err(_) => {
            // The exclusive holder may be a concurrent `models remove` or an
            // in-flight `models fetch` (round-2 D2/D3); neither means the
            // model can be loaded safely right now.
            Err(Error::Admission {
                message: format!(
                    "the model cache {} is busy (fetch in progress or removal); retry",
                    dir.display()
                ),
            })
        }
    }
}

/// Leaks a `'static` copy of a registry row (model instances are process-long
/// by cache-key design).
fn clone_entry(entry: &ModelEntry<'_>) -> ModelEntry<'static> {
    let files = Box::leak(entry.files.to_vec().into_boxed_slice());
    ModelEntry {
        name: Box::leak(entry.name.to_owned().into_boxed_str()),
        task: entry.task,
        repo: Box::leak(entry.repo.to_owned().into_boxed_str()),
        revision: Box::leak(entry.revision.to_owned().into_boxed_str()),
        dim: entry.dim,
        pooling: entry.pooling,
        quantization: entry
            .quantization
            .map(|q| -> &'static str { Box::leak(q.to_owned().into_boxed_str()) }),
        output_key: entry
            .output_key
            .map(|k| -> &'static str { Box::leak(k.to_owned().into_boxed_str()) }),
        license: Box::leak(entry.license.to_owned().into_boxed_str()),
        query_prefix: Box::leak(entry.query_prefix.to_owned().into_boxed_str()),
        passage_prefix: Box::leak(entry.passage_prefix.to_owned().into_boxed_str()),
        prompt_version: entry.prompt_version,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::models::test_support::{FakeSource, fake_cache_dir, fake_entry};

    #[test]
    fn read_cache_file_accepts_verified_bytes() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let entry = fake_entry();
        let dir = fake_cache_dir(tmp.path());
        let source = FakeSource::new(&entry);
        crate::hybrid::models::fetch_into(&entry, &source, false, &dir).expect("fetch");
        let onnx = dir.join("onnx/model.onnx");
        let bytes = read_cache_file(&dir, &onnx, &entry).expect("verified read");
        assert_eq!(bytes.len() as u64, 4096);
    }

    #[test]
    fn read_cache_file_rejects_tampered_same_size_bytes() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let entry = fake_entry();
        let dir = fake_cache_dir(tmp.path());
        let source = FakeSource::new(&entry);
        crate::hybrid::models::fetch_into(&entry, &source, false, &dir).expect("fetch");
        let onnx = dir.join("onnx/model.onnx");
        // Same-size content tamper: the memo/size gate cannot see it, the
        // load-time sha256 must (round-4 C4).
        let mut tampered = std::fs::read(&onnx).expect("read");
        tampered[0] ^= 0xff;
        std::fs::write(&onnx, &tampered).expect("tamper");
        let err = read_cache_file(&dir, &onnx, &entry).expect_err("tamper");
        assert!(matches!(err, Error::Admission { .. }), "{err:?}");
        assert!(err.to_string().contains("--force"), "{err}");
    }

    #[test]
    fn batch_chunks_covers_exact_and_partial_tails() {
        assert_eq!(batch_chunks(0), Vec::<std::ops::Range<usize>>::new());
        let one = batch_chunks(1);
        assert_eq!(one, vec![0..1]);
        let full = batch_chunks(DEFAULT_BATCH_SIZE);
        assert_eq!(full, vec![0..DEFAULT_BATCH_SIZE]);
        let mixed = batch_chunks(DEFAULT_BATCH_SIZE + 7);
        assert_eq!(
            mixed,
            vec![
                0..DEFAULT_BATCH_SIZE,
                DEFAULT_BATCH_SIZE..DEFAULT_BATCH_SIZE + 7
            ]
        );
        // Coverage: concatenated ranges cover [0, len) exactly once.
        let len = 3 * DEFAULT_BATCH_SIZE + 3;
        let ranges = batch_chunks(len);
        let covered: usize = ranges.iter().map(std::iter::ExactSizeIterator::len).sum();
        assert_eq!(covered, len);
        assert!(ranges.iter().all(|range| range.len() <= DEFAULT_BATCH_SIZE));
    }

    #[test]
    fn embedder_without_runtime_fails_with_connect_hint() {
        let cfg = EmbeddingCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let providers = ProvidersCfg::default();
        let Err(err) = embedder(&cfg, &providers) else {
            panic!("expected admission without a runtime");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("connect fastembed"), "{err}");
    }

    /// Isolates the process env (cache + config dirs) for tests that assume
    /// an empty model cache; serialized with every other env-touching test
    /// through the shared CLI lock.
    #[allow(unsafe_code)] // env mutation is confined to the locked section
    fn with_isolated_empty_cache<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::cli::test_env::ENV_LOCK.lock().expect("env lock");
        let cache = tempfile::TempDir::new().expect("tmp");
        let config = tempfile::TempDir::new().expect("tmp");
        let prev_cache = std::env::var_os("DOCSBASE_CACHE_DIR");
        let prev_config = std::env::var_os("DOCSBASE_CONFIG_DIR");
        // SAFETY: single-threaded under the shared env lock.
        unsafe {
            std::env::set_var("DOCSBASE_CACHE_DIR", cache.path());
            std::env::set_var("DOCSBASE_CONFIG_DIR", config.path());
        }
        let out = f();
        // SAFETY: same critical section.
        unsafe {
            match prev_cache {
                Some(value) => std::env::set_var("DOCSBASE_CACHE_DIR", value),
                None => std::env::remove_var("DOCSBASE_CACHE_DIR"),
            }
            match prev_config {
                Some(value) => std::env::set_var("DOCSBASE_CONFIG_DIR", value),
                None => std::env::remove_var("DOCSBASE_CONFIG_DIR"),
            }
        }
        out
    }

    #[test]
    fn embedder_without_model_fails_with_fetch_hint() {
        let cfg = EmbeddingCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let dir = tempfile::TempDir::new().expect("tmp");
        let runtime_path = dir.path().join("libonnxruntime.so");
        std::fs::write(&runtime_path, b"fake").expect("touch");
        let providers = ProvidersCfg {
            fastembed: Some(super::super::config::FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(runtime_path),
            }),
            openrouter: None,
        };
        let Err(err) = with_isolated_empty_cache(|| embedder(&cfg, &providers)) else {
            panic!("expected admission without a fetched model");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("models fetch"), "{err}");
    }

    #[test]
    fn embed_query_applies_registry_query_prefix() {
        let e5 = models::entry("multilingual-e5-small").expect("e5");
        assert_eq!(apply_role_prefix("hello", e5, true), "query: hello");
        assert_eq!(apply_role_prefix("hello", e5, false), "passage: hello");
        let bge = models::entry("bge-small-en-v1.5").expect("bge");
        assert_eq!(
            apply_role_prefix("search me", bge, true),
            "Represent this sentence for searching relevant passages: search me"
        );
        assert_eq!(apply_role_prefix("search me", bge, false), "search me");
        // Prefix-less rows (rerank models) leave both roles untouched.
        let jina = models::entry("jina-reranker-v1-turbo-en").expect("jina");
        assert_eq!(apply_role_prefix("x", jina, true), "x");
        assert_eq!(apply_role_prefix("x", jina, false), "x");
    }

    #[test]
    fn loaded_model_holds_shared_lock() {
        // The guard semantics: while a shared guard from take_shared_lock is
        // alive, the exclusive lock is refused; after dropping it, exclusive
        // acquisition succeeds. This is the holder side of the FR-7/T9
        // contract (models remove refuses while a model is loaded).
        let tmp = tempfile::TempDir::new().expect("tmp");
        // Realistic layout: the lock lives at the model root, not inside
        // the revision dir (round-2 D2).
        let root = tmp.path().join("models").join("fake-model");
        std::fs::create_dir_all(&root).expect("root");
        let rev = root.join("rev");
        std::fs::create_dir_all(&rev).expect("rev");
        let lock_path = root.join(".lock");
        let _ = std::fs::write(&lock_path, b"");
        let guard = take_shared_lock(&rev).expect("shared lock");
        let exclusive_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("open");
        let mut exclusive = fd_lock::RwLock::new(exclusive_file);
        assert!(
            exclusive.try_write().is_err(),
            "exclusive lock refused while a shared holder lives"
        );
        drop(guard);
        assert!(
            exclusive.try_write().is_ok(),
            "exclusive lock available after the shared guard drops"
        );
    }

    #[test]
    fn take_shared_lock_on_vanished_root_is_fetch_hint() {
        // Round-2 M5: the cached() check and the shared-lock acquisition
        // are not atomic; if the model root vanished in between, ta
        // shared lock must fail with the fetch hint, never Internal.
        let dir = tempfile::TempDir::new().expect("tmp");
        let root = dir.path().join("models").join("bge-small-en-v1.5");
        let rev = root.join("rev");
        // No model directory exists at all.
        let err = take_shared_lock(&rev).expect_err("vanished root");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(
            err.to_string().contains("models fetch bge-small-en-v1.5"),
            "{err}"
        );
        // Existing root: succeeds.
        std::fs::create_dir_all(&root).expect("root");
        let _guard = take_shared_lock(&rev).expect("lock");
    }

    #[test]
    fn instant_cache_deduplicates_per_key() {
        // Drive the production map (`get_or_load_embedder` + `INSTANCES`)
        // with fake embedders — no real weights needed (NFR-4/NFR-7).
        let key_a = ModelCacheKey {
            model_id: "fastembed/x@d384@l2@raaaaaaaa@p1".to_owned(),
            max_length: 512,
            intra_threads: Some(4),
            runtime: PathBuf::from("/tmp/libonnxruntime.so"),
        };
        let key_b = ModelCacheKey {
            model_id: "fastembed/y@d384@l2@raaaaaaaa@p1".to_owned(),
            ..key_a.clone()
        };
        let make = |id: String| -> Arc<dyn Embedder> {
            struct Fake(String);
            impl Embedder for Fake {
                fn model_id(&self) -> &str {
                    &self.0
                }
                fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
                    Err(Error::internal("not used"))
                }
            }
            Arc::new(Fake(id))
        };
        let first =
            get_or_load_embedder(key_a.clone(), || Ok(make(key_a.model_id.clone()))).expect("load");
        // Same key must not invoke the loader again (panic-guard).
        let second = get_or_load_embedder(key_a.clone(), || panic!("same key must not load twice"))
            .expect("cached");
        assert!(Arc::ptr_eq(&first, &second), "same key shares the instance");
        let other =
            get_or_load_embedder(key_b.clone(), || Ok(make(key_b.model_id.clone()))).expect("load");
        assert!(
            !Arc::ptr_eq(&first, &other),
            "different key is a different instance"
        );
    }

    #[test]
    fn concurrent_same_key_cold_loads_construct_once() {
        // C11: cold same-key callers must not construct duplicates; exactly
        // one loader invocation across 8 overlapping threads, all sharing
        // the same instance.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::sync::{Arc as StdArc, Barrier};
        use std::time::Duration;
        static LOADS: AtomicUsize = AtomicUsize::new(0);
        let key = ModelCacheKey {
            model_id: "fastembed/cc@d384@l2@raaaaaaaa@p1".to_owned(),
            max_length: 512,
            intra_threads: Some(4),
            runtime: PathBuf::from("/tmp/libonnxruntime.so"),
        };
        let make = |id: String| -> Arc<dyn Embedder> {
            struct Fake(String);
            impl Embedder for Fake {
                fn model_id(&self) -> &str {
                    &self.0
                }
                fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
                    Err(Error::internal("not used"))
                }
            }
            Arc::new(Fake(id))
        };
        let start = StdArc::new(Barrier::new(8));
        let (tx, rx) = mpsc::channel();
        for _ in 0..8 {
            let key = key.clone();
            let start = StdArc::clone(&start);
            let tx = tx.clone();
            std::thread::spawn(move || {
                start.wait();
                let inst = get_or_load_embedder(key.clone(), || {
                    LOADS.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(20));
                    Ok(make(key.model_id.clone()))
                })
                .expect("load");
                let _ = tx.send(inst);
            });
        }
        drop(tx);
        let instances: Vec<_> = rx.iter().collect();
        assert_eq!(instances.len(), 8, "all threads got an instance");
        assert_eq!(
            LOADS.load(Ordering::Relaxed),
            1,
            "exactly one construction for concurrent same-key loads"
        );
        for inst in &instances {
            assert!(Arc::ptr_eq(&instances[0], inst), "shared instance");
        }
    }

    #[test]
    fn poisoned_load_lock_recovers_for_later_keys() {
        // Round-2 D4: a panicking loader poisons LOAD_LOCK; later
        // constructions must still succeed (poison is meaningless for a
        // lock that guards no data).
        let key_p = ModelCacheKey {
            model_id: "fastembed/p@d384@l2@raaaaaaaa@p1".to_owned(),
            max_length: 512,
            intra_threads: Some(4),
            runtime: PathBuf::from("/tmp/libonnxruntime.so"),
        };
        let _ = std::panic::catch_unwind(|| {
            let _ = get_or_load_embedder(key_p.clone(), || -> Result<Arc<dyn Embedder>> {
                panic!("loader boom (test)");
            });
        });
        let key_q = ModelCacheKey {
            model_id: "fastembed/q@d384@l2@raaaaaaaa@p1".to_owned(),
            ..key_p.clone()
        };
        let make = |id: String| -> Arc<dyn Embedder> {
            struct Fake(String);
            impl Embedder for Fake {
                fn model_id(&self) -> &str {
                    &self.0
                }
                fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
                    Err(Error::internal("not used"))
                }
            }
            Arc::new(Fake(id))
        };
        let inst = get_or_load_embedder(key_q.clone(), || Ok(make(key_q.model_id.clone())))
            .expect("later load works after poison");
        let again =
            get_or_load_embedder(key_q.clone(), || panic!("must be cached")).expect("cached");
        assert!(Arc::ptr_eq(&inst, &again));
    }

    #[test]
    fn reranker_without_runtime_fails_with_connect_hint() {
        let cfg = RerankerCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "jina-reranker-v1-turbo-en".to_owned(),
            window: 50,
        };
        let providers = ProvidersCfg::default();
        let Err(err) = reranker(&cfg, &providers) else {
            panic!("expected admission without a runtime");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("connect fastembed"), "{err}");
    }

    #[test]
    fn reranker_without_model_fails_with_fetch_hint() {
        let cfg = RerankerCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "jina-reranker-v1-turbo-en".to_owned(),
            window: 50,
        };
        let dir = tempfile::TempDir::new().expect("tmp");
        let runtime_path = dir.path().join("libonnxruntime.so");
        std::fs::write(&runtime_path, b"fake").expect("touch");
        let providers = ProvidersCfg {
            fastembed: Some(super::super::config::FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(runtime_path),
            }),
            openrouter: None,
        };
        let Err(err) = with_isolated_empty_cache(|| reranker(&cfg, &providers)) else {
            panic!("expected admission without a fetched model");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("models fetch"), "{err}");
    }

    #[test]
    fn reranker_rejects_embedding_model_rows() {
        let cfg = RerankerCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            window: 50,
        };
        let providers = ProvidersCfg::default();
        let Err(err) = reranker(&cfg, &providers) else {
            panic!("expected admission for an embedding model selected for rerank");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("rerank model"), "{err}");
    }

    #[test]
    fn rerank_contract_returns_index_and_score_for_every_doc() {
        use fastembed::RerankResult;
        let make = |index: usize, score: f32| RerankResult {
            document: None,
            score,
            index,
        };
        // Deliberately unsorted: the contract, not fastembed's order, must
        // determine the output.
        let results = vec![
            make(2, 1.0),
            make(3, 9.0),
            make(1, 4.0),
            make(0, 7.5),
            make(4, 6.0),
        ];
        let scores = rerank_scores(&results, 10);
        assert_eq!(scores.len(), 5, "every doc gets an index+score");
        assert_eq!(
            scores,
            vec![(3, 9.0), (0, 7.5), (4, 6.0), (1, 4.0), (2, 1.0)]
        );
        // Order must be best-first.
        assert!(scores.windows(2).all(|w| w[0].1 >= w[1].1));
        // top_n clamps.
        let capped = rerank_scores(&results, 2);
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].0, 3);
        assert_eq!(capped[1].0, 0);
        assert!((capped[0].1 - 9.0).abs() < f32::EPSILON);
        // top_n larger than docs returns everything.
        assert_eq!(rerank_scores(&results, 100).len(), 5);
    }

    #[test]
    fn reranker_cache_deduplicates_per_key() {
        // Same contract as the embedder map: same key → same `Arc` and the
        // loader is never invoked twice; a different key loads anew.
        let key_a = ModelCacheKey {
            model_id: "fastembed/jina-reranker-v1-turbo-en".to_owned(),
            max_length: 512,
            intra_threads: Some(4),
            runtime: PathBuf::from("/tmp/libonnxruntime.so"),
        };
        let key_b = ModelCacheKey {
            model_id: "fastembed/bge-reranker-base".to_owned(),
            ..key_a.clone()
        };
        let make = |_id: String| -> Arc<dyn Reranker> {
            struct Fake;
            impl Reranker for Fake {
                fn rerank(&self, _: &str, _: &[String], _: usize) -> Result<Vec<(usize, f32)>> {
                    Err(Error::internal("not used"))
                }
            }
            Arc::new(Fake)
        };
        let first =
            get_or_load_reranker(key_a.clone(), || Ok(make(key_a.model_id.clone()))).expect("load");
        // Same key must not invoke the loader again (panic-guard).
        let second = get_or_load_reranker(key_a.clone(), || panic!("same key must not load twice"))
            .expect("cached");
        assert!(Arc::ptr_eq(&first, &second), "same key shares the instance");
        let other =
            get_or_load_reranker(key_b.clone(), || Ok(make(key_b.model_id.clone()))).expect("load");
        assert!(
            !Arc::ptr_eq(&first, &other),
            "different key is a different instance"
        );
    }

    #[test]
    fn registry_query_and_passage_prefixes_are_role_aware() {
        // Pure prefix contract for the registry rows (FR-10): E5 has both
        // prefixes, BGE has a query instruction and no passage prefix,
        // rerank rows have none.
        let e5 = models::entry("multilingual-e5-small").expect("e5");
        assert_eq!(e5.query_prefix, "query: ");
        assert_eq!(e5.passage_prefix, "passage: ");
        let bge = models::entry("bge-small-en-v1.5").expect("bge");
        assert!(
            bge.query_prefix
                .starts_with("Represent this sentence for searching")
        );
        assert_eq!(bge.passage_prefix, "");
    }

    #[test]
    fn build_cache_key_includes_revision_prompt_and_runtime() {
        let cfg = EmbeddingCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let id = super::super::config::embedding_model_id(&cfg);
        assert!(
            id.starts_with("fastembed/bge-small-en-v1.5@d384@l2@r"),
            "{id}"
        );
        assert!(id.contains("@p1"), "{id} includes the prompt version");
        // Rerank and embedding rows both resolve through the registry.
        let rerank = models::entry("jina-reranker-v1-turbo-en").expect("rerank");
        assert_eq!(rerank.task, Task::Rerank);
    }
}
