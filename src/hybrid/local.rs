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

use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::error::{Error, Result};
use crate::hybrid::models::{self, ModelEntry, Pooling, Task};
use crate::platform::file_lease::SharedFileLease;
use crate::vector_cache::Embedder;

use super::Reranker;
use super::config::{EmbeddingCfg, ProvidersCfg, RerankerCfg};
use super::local_settings::InferenceSettings;
pub use super::local_settings::{DEFAULT_BATCH_SIZE, DEFAULT_MAX_LENGTH};

/// Identity of a live native model session (NFR-4/NFR-7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelCacheKey {
    /// Canonical model id (`embedding_model_id`).
    pub model_id: String,
    /// Tokenizer max length.
    pub max_length: usize,
    /// Resolved intra-op threads.
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
    let folder = model_folder(providers, entry);
    if !models::cached_in(entry, &folder)? {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" is not fetched; run `docsbase models fetch fastembed/{}`",
                cfg.model, cfg.model
            ),
        });
    }
    let inference = providers.fastembed_inference()?;
    let key = embedding_key(cfg, runtime, inference);
    let instance = get_or_load_by_key((key.clone(), folder.clone()), embedder_instances(), || {
        FastEmbedEmbedder::load(entry, &key, &folder).map(Arc::new)
    })?;
    Ok(Arc::new(FastEmbedEmbedder {
        entry,
        model_id: key.model_id,
        instance,
        batch_size: inference.batch_size,
        cpu_pool: providers.cpu_pool(),
    }))
}

/// Builds the process-wide cache key for an embedding config — the single
/// source of truth for the dedup contract (NFR-4/NFR-7): the canonical
/// `model_id` (provider, model, dims, normalize, revision, prompt version),
/// the bounded max length, the host's standalone thread default and the
/// resolved runtime path. The stage factory builds its key from the captured
/// provider settings. Only the model ID names cached vectors.
#[must_use]
pub fn cache_key(cfg: &EmbeddingCfg, runtime: PathBuf) -> ModelCacheKey {
    embedding_key(
        cfg,
        runtime,
        InferenceSettings::defaults(
            crate::config::bootstrap::DefaultsContext::for_host().available_parallelism,
        ),
    )
}

fn embedding_key(
    cfg: &EmbeddingCfg,
    runtime: PathBuf,
    inference: InferenceSettings,
) -> ModelCacheKey {
    model_key(
        super::config::fastembed_embedding_model_id(cfg, inference.embedding_max_length),
        runtime,
        inference.embedding_max_length,
        inference.intra_threads,
    )
}

fn model_key(
    model_id: String,
    runtime: PathBuf,
    max_length: usize,
    intra_threads: usize,
) -> ModelCacheKey {
    ModelCacheKey {
        model_id,
        max_length,
        intra_threads: Some(intra_threads),
        runtime,
    }
}

struct LoadedModel<T> {
    model: Mutex<T>,
    // Drop the native session before releasing the model-directory lease.
    _lease: SharedFileLease,
}

// File leases belong to one folder even when weights and vector recipes
// are identical across installations.
type ModelInstances<T> = Mutex<HashMap<(ModelCacheKey, PathBuf), Weak<T>>>;

/// Serializes model construction (cold path only, C11): without it, two
/// cold same-key callers could both run `load()` (two ORT sessions, two
/// shared-lock attempts) and the re-check would only discard the loser.
/// Initial lookups release the map lock before waiting for this lock.
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Core process-wide per-key instance cache: returns the existing instance
/// for `key`, or constructs one with `load` and registers it. The single
/// generic implementation behind both model maps. Weak entries let the
/// last stage release the model session and its file lock.
fn get_or_load_by_key<T: ?Sized>(
    key: (ModelCacheKey, PathBuf),
    instances: &ModelInstances<T>,
    load: impl FnOnce() -> Result<Arc<T>>,
) -> Result<Arc<T>> {
    {
        let map = instances
            .lock()
            .map_err(|poison| Error::internal(format!("instance cache lock poisoned: {poison}")))?;
        if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
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
        if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
    }
    let shared = load()?;
    let mut map = instances
        .lock()
        .map_err(|poison| Error::internal(format!("instance cache lock poisoned: {poison}")))?;
    map.retain(|_, instance| instance.strong_count() != 0);
    map.insert(key, Arc::downgrade(&shared));
    Ok(shared)
}

fn embedder_instances() -> &'static ModelInstances<LoadedModel<fastembed::TextEmbedding>> {
    static INSTANCES: OnceLock<ModelInstances<LoadedModel<fastembed::TextEmbedding>>> =
        OnceLock::new();
    INSTANCES.get_or_init(Mutex::default)
}

/// Local embedding stage with its own batch size and a shared native session.
pub struct FastEmbedEmbedder {
    entry: &'static ModelEntry<'static>,
    model_id: String,
    instance: Arc<LoadedModel<fastembed::TextEmbedding>>,
    batch_size: usize,
    cpu_pool: Option<Arc<super::cpu::CpuPool>>,
}

impl FastEmbedEmbedder {
    /// Loads the model from the verified cache. The fastembed model is a
    /// static-typed registry row, so the onnx/tokenizer files are read from
    /// the cache directory by `files[]` paths.
    ///
    /// # Errors
    /// Admission before load (runtime/model); `Internal` after a successful
    /// load when the session/tokenizer construction fails.
    fn load(
        entry: &'static ModelEntry<'static>,
        key: &ModelCacheKey,
        cache_dir: &Path,
    ) -> Result<LoadedModel<fastembed::TextEmbedding>> {
        // The runtime must be initialized before any fastembed constructor
        // (FR-11). `init` is idempotent per process.
        super::runtime::init(&key.runtime)?;

        let lock_guard = take_shared_lock(cache_dir)?;

        let onnx_path = entry
            .files
            .iter()
            .find(|spec| spec.path.starts_with("onnx/"))
            .map(|spec| cache_dir.join(spec.path))
            .ok_or_else(|| {
                Error::internal(format!("registry row {} has no onnx file", entry.name))
            })?;
        let onnx = read_cache_file(cache_dir, &onnx_path, entry)?;
        let tokenizer_files = tokenizer_files(cache_dir, entry)?;

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
        // enum needs `&'static str`, which the compiled registry supplies.
        if let Some(output_key) = entry.output_key {
            model.output_key = Some(fastembed::OutputKey::ByName(output_key));
        }

        let model =
            fastembed::TextEmbedding::try_new_from_user_defined(model, embedding_options(key))
                .map_err(|err| {
                    Error::internal(format!("failed to load the fastembed model: {err}"))
                })?;

        Ok(LoadedModel {
            model: Mutex::new(model),
            _lease: lock_guard,
        })
    }

    /// Query embedding with the registry `query_prefix` (FR-10).
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let prefixed = apply_role_prefix(text, self.entry, true);
        let mut session = self
            .instance
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("embedder lock poisoned: {poison}")))?;
        let model = &mut *session;
        execute_inference(self.cpu_pool.as_deref(), || {
            let mut vectors = model
                .embed([prefixed], Some(self.batch_size))
                .map_err(|err| Error::internal(format!("fastembed inference failed: {err}")))?;
            vectors
                .pop()
                .ok_or_else(|| Error::internal("fastembed returned no vector for one input"))
        })
    }
}

impl Embedder for FastEmbedEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|text| apply_role_prefix(text, self.entry, false))
            .collect();
        // A Rayon worker must never block on a session whose holder can enter
        // that same pool to tokenize. Acquire the guard on the caller.
        let mut session = self
            .instance
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("embedder lock poisoned: {poison}")))?;
        let model = &mut *session;
        execute_inference(self.cpu_pool.as_deref(), || {
            embed_batches(&prefixed, self.batch_size, |chunk, batch_size| {
                model
                    .embed(chunk, Some(batch_size))
                    .map_err(|err| Error::internal(format!("fastembed inference failed: {err}")))
            })
        })
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_text(text)
    }
}

fn model_folder(providers: &ProvidersCfg, entry: &ModelEntry<'_>) -> PathBuf {
    providers
        .runtime_bootstrap()
        .and_then(|runtime| runtime.cache_root.as_deref())
        .map_or_else(
            || models::cache_dir(entry.name, entry.revision),
            |root| models::cache_dir_under(root, entry.name, entry.revision),
        )
}

fn execute_inference<T: Send>(
    pool: Option<&super::cpu::CpuPool>,
    work: impl FnOnce() -> Result<T> + Send,
) -> Result<T> {
    match pool {
        Some(pool) => pool.install(work)?,
        None => work(),
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
    let folder = model_folder(providers, entry);
    if !models::cached_in(entry, &folder)? {
        return Err(Error::Admission {
            message: format!(
                "fastembed model \"{}\" is not fetched; run `docsbase models fetch fastembed/{}`",
                cfg.model, cfg.model
            ),
        });
    }
    // The reranker key mirrors the embedder's canonical shape minus the
    // vector-affecting components (scores are never cached; the revision is
    // build-static, so `@r<rev8>` adds nothing today). Kept as the canonical
    // `model_id` for the dedup contract (NFR-4).
    let inference = providers.fastembed_inference()?;
    let key = model_key(
        format!("fastembed/{}", cfg.model),
        runtime,
        DEFAULT_MAX_LENGTH,
        inference.intra_threads,
    );
    let instance = get_or_load_by_key((key.clone(), folder.clone()), reranker_instances(), || {
        FastEmbedReranker::load(entry, &key, &folder).map(Arc::new)
    })?;
    Ok(Arc::new(FastEmbedReranker {
        instance,
        batch_size: inference.batch_size,
        cpu_pool: providers.cpu_pool(),
    }))
}

fn reranker_instances() -> &'static ModelInstances<LoadedModel<fastembed::TextRerank>> {
    static INSTANCES: OnceLock<ModelInstances<LoadedModel<fastembed::TextRerank>>> =
        OnceLock::new();
    INSTANCES.get_or_init(Mutex::default)
}

/// Local `FastEmbed` cross-encoder stage with a shared native session.
pub struct FastEmbedReranker {
    instance: Arc<LoadedModel<fastembed::TextRerank>>,
    batch_size: usize,
    cpu_pool: Option<Arc<super::cpu::CpuPool>>,
}

impl FastEmbedReranker {
    /// Loads the rerank model from the verified cache.
    ///
    /// # Errors
    /// Admission before load (runtime/model); `Internal` after a successful
    /// load when session/tokenizer construction fails.
    fn load(
        entry: &ModelEntry<'_>,
        key: &ModelCacheKey,
        cache_dir: &Path,
    ) -> Result<LoadedModel<fastembed::TextRerank>> {
        super::runtime::init(&key.runtime)?;
        let lock_guard = take_shared_lock(cache_dir)?;

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
        let onnx = read_cache_file(cache_dir, &onnx_path, entry)?;
        let tokenizer_files = tokenizer_files(cache_dir, entry)?;
        let model = fastembed::UserDefinedRerankingModel::new(
            fastembed::OnnxSource::Memory(onnx),
            tokenizer_files,
        );
        let model = fastembed::TextRerank::try_new_from_user_defined(model, reranking_options(key))
            .map_err(|err| {
                Error::internal(format!("failed to load the fastembed reranker: {err}"))
            })?;
        Ok(LoadedModel {
            model: Mutex::new(model),
            _lease: lock_guard,
        })
    }
}

impl Reranker for FastEmbedReranker {
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        let mut session = self
            .instance
            .model
            .lock()
            .map_err(|poison| Error::internal(format!("reranker lock poisoned: {poison}")))?;
        let model = &mut *session;
        let docs_ref: Vec<&str> = docs.iter().map(String::as_str).collect();
        execute_inference(self.cpu_pool.as_deref(), || {
            let results = model
                .rerank(query, &docs_ref, false, Some(self.batch_size))
                .map_err(|err| {
                    Error::internal(format!("fastembed rerank inference failed: {err}"))
                })?;
            Ok(rerank_scores(&results, top_n))
        })
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

fn embedding_options(key: &ModelCacheKey) -> fastembed::InitOptionsUserDefined {
    fastembed::InitOptionsUserDefined::new()
        .with_max_length(key.max_length)
        .with_intra_threads(key.intra_threads.unwrap_or(1))
}

fn reranking_options(key: &ModelCacheKey) -> fastembed::RerankInitOptionsUserDefined {
    fastembed::RerankInitOptionsUserDefined::new()
        .with_max_length(key.max_length)
        .with_intra_threads(key.intra_threads.unwrap_or(1))
}

fn embed_batches(
    texts: &[String],
    batch_size: usize,
    mut infer: impl FnMut(&[String], usize) -> Result<Vec<Vec<f32>>>,
) -> Result<Vec<Vec<f32>>> {
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(batch_size) {
        let vectors = infer(chunk, batch_size)?;
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
            "fastembed model \"{}\" cache file {} is unreadable ({}); re-run `docsbase models fetch fastembed/{}`",
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
                "fastembed model \"{}\" cache file {} has size {} (registry says {}); re-run `docsbase models fetch fastembed/{}`",
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
                "fastembed model \"{}\" cache file {} is corrupted (sha256 mismatch); re-run `docsbase models fetch fastembed/{} --force`",
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
pub(crate) fn take_shared_lock(dir: &Path) -> Result<SharedFileLease> {
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
                    "fastembed model \"{name}\" is not fetched; run `docsbase models fetch fastembed/{name}`"
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
    match SharedFileLease::try_acquire(file) {
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

#[cfg(test)]
mod tests {
    use super::super::config::PROVIDER_FASTEMBED;
    use super::*;
    use crate::hybrid::models::test_support::{FakeSource, fake_cache_dir, fake_entry};

    fn get_or_load_embedder(
        key: ModelCacheKey,
        load: impl FnOnce() -> Result<Arc<dyn Embedder>>,
    ) -> Result<Arc<dyn Embedder>> {
        static INSTANCES: OnceLock<ModelInstances<dyn Embedder>> = OnceLock::new();
        get_or_load_by_key(
            (key, PathBuf::new()),
            INSTANCES.get_or_init(Mutex::default),
            load,
        )
    }

    fn get_or_load_reranker(
        key: ModelCacheKey,
        load: impl FnOnce() -> Result<Arc<dyn Reranker>>,
    ) -> Result<Arc<dyn Reranker>> {
        static INSTANCES: OnceLock<ModelInstances<dyn Reranker>> = OnceLock::new();
        get_or_load_by_key(
            (key, PathBuf::new()),
            INSTANCES.get_or_init(Mutex::default),
            load,
        )
    }

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
    fn configured_embedding_length_reaches_onnx_options_and_persistent_identity() {
        let cfg = super::super::config::resolve_embedding(
            super::super::config::PROVIDER_FASTEMBED,
            Some("multilingual-e5-small"),
            None,
            true,
        )
        .unwrap();
        let default = super::super::config::embedding_model_id(&cfg);
        let mut providers = ProvidersCfg::default();
        assert_eq!(providers.embedding_model_id(&cfg).unwrap(), default);
        let mut record = super::super::config::ProviderRecord::new(true);
        record
            .params
            .insert("embedding_max_length".into(), 128.into());
        record.params.insert("batch_size".into(), 2.into());
        providers.insert(super::super::config::PROVIDER_FASTEMBED, record);
        let inference = providers.fastembed_inference().unwrap();
        let key = embedding_key(&cfg, PathBuf::from("onnx-runtime"), inference);
        assert_eq!(embedding_options(&key).max_length, 128);
        assert_eq!(key.model_id, format!("{default}@t128"));
        assert_eq!(key.model_id, providers.embedding_model_id(&cfg).unwrap());
    }

    #[test]
    fn cache_key_uses_canonical_id_and_bounded_limits() {
        // Round-5 deepseek: the dedup contract (NFR-4/NFR-7) must be pinned
        // through the PRODUCTION constructor, not a hand-built literal —
        // `embedder` builds its key with this function.
        let cfg = crate::hybrid::EmbeddingCfg {
            provider: super::super::config::PROVIDER_FASTEMBED.to_owned(),
            model: "multilingual-e5-small".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let runtime = PathBuf::from("/tmp/libonnxruntime.so");
        let key = cache_key(&cfg, runtime.clone());
        assert_eq!(
            key.model_id,
            super::super::config::embedding_model_id(&cfg),
            "key carries the canonical id"
        );
        assert!(key.model_id.contains("@r761b726d"), "{} ", key.model_id);
        assert_eq!(key.max_length, DEFAULT_MAX_LENGTH, "bounded max length");
        assert_eq!(
            key.intra_threads,
            Some(
                InferenceSettings::defaults(
                    crate::config::bootstrap::DefaultsContext::for_host().available_parallelism
                )
                .intra_threads
            ),
            "capped threads"
        );
        assert!(key.intra_threads.unwrap_or(0) <= 4, "never above the cap");
        assert_eq!(key.runtime, runtime, "runtime path is part of the key");

        // Dedup contract: same config + runtime = same key (one instance);
        // a normalize flip changes the key (different vectors).
        assert_eq!(cache_key(&cfg, runtime.clone()), key, "same key dedups");
        let mut raw = cfg.clone();
        raw.normalize = false;
        assert_ne!(
            cache_key(&raw, runtime).model_id,
            key.model_id,
            "normalize change must invalidate"
        );
    }

    #[test]
    fn embed_batches_pass_the_resolved_size_and_cover_every_input() {
        let texts: Vec<_> = (0..8).map(|item| item.to_string()).collect();
        let mut chunks = Vec::new();
        let vectors = embed_batches(&texts, 3, |chunk, batch_size| {
            assert_eq!(batch_size, 3);
            chunks.push(chunk.to_vec());
            Ok(vec![vec![1.0_f32]; chunk.len()])
        })
        .unwrap();
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 2]);
        assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), texts);
        assert_eq!(vectors.len(), texts.len());
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
        let _guard = crate::cli::test_env::env_guard();
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
        let providers = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            super::super::config::FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(runtime_path),
            }
            .to_record(),
        );
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
            err.to_string()
                .contains("models fetch fastembed/bge-small-en-v1.5"),
            "{err}"
        );
        // Existing root: succeeds.
        std::fs::create_dir_all(&root).expect("root");
        let _guard = take_shared_lock(&rev).expect("lock");
    }

    #[test]
    fn last_model_handle_releases_the_shared_file_lock() {
        struct LockedModel<T> {
            _lease: T,
        }
        impl<T: Send + Sync> Embedder for LockedModel<T> {
            fn model_id(&self) -> &'static str {
                "fastembed/retained-model"
            }
            fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
                Err(Error::internal("not used"))
            }
        }

        let root = tempfile::tempdir().unwrap();
        let key = ModelCacheKey {
            model_id: "fastembed/retained-model".to_owned(),
            max_length: DEFAULT_MAX_LENGTH,
            intra_threads: Some(1),
            runtime: root.path().join("runtime"),
        };
        let load = || {
            let model: Arc<dyn Embedder> = Arc::new(LockedModel {
                _lease: take_shared_lock(&root.path().join("rev"))?,
            });
            Ok(model)
        };
        let first = get_or_load_embedder(key.clone(), load).unwrap();
        let retained =
            get_or_load_embedder(key.clone(), || panic!("the same model is still live")).unwrap();
        let mut exclusive = fd_lock::RwLock::new(
            std::fs::File::options()
                .read(true)
                .write(true)
                .open(root.path().join(".lock"))
                .unwrap(),
        );
        drop(first);
        assert!(exclusive.try_write().is_err());
        drop(retained);
        assert!(exclusive.try_write().is_ok());
        assert!(get_or_load_embedder(key, load).is_ok());
    }

    #[test]
    fn fastembed_inference_settings_accept_positive_provider_overrides() {
        let source = "config_version=2\n[providers.fastembed]\nenabled = true\nbatch_size = 3\nintra_threads = 2\n";
        let config = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join("config.toml"), source).unwrap();
        let parsed =
            crate::config::Config::global_from(config.path()).expect("provider inference settings");
        let inference = parsed.providers.fastembed_inference().unwrap();
        assert_eq!(inference.batch_size, 3);
        let cfg = EmbeddingCfg {
            provider: PROVIDER_FASTEMBED.to_owned(),
            model: "multilingual-e5-small".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let runtime = PathBuf::from("local-ort");
        let baseline = cache_key(&cfg, runtime.clone());
        let key = embedding_key(&cfg, runtime.clone(), inference);
        assert_eq!(key.model_id, baseline.model_id);
        assert_eq!(embedding_options(&key).max_length, DEFAULT_MAX_LENGTH);
        assert_eq!(embedding_options(&key).intra_threads, Some(2));
        assert_eq!(reranking_options(&key).intra_threads, Some(2));
        let other_batch = InferenceSettings {
            batch_size: 13,
            ..inference
        };
        assert_eq!(embedding_key(&cfg, runtime, other_batch), key);
    }

    #[test]
    fn fastembed_inference_settings_reject_invalid_counts() {
        let owner = crate::hybrid::provider(PROVIDER_FASTEMBED).unwrap();
        for (key, value) in [
            ("batch_size", "0"),
            ("batch_size", "-1"),
            ("intra_threads", "0"),
            ("intra_threads", "2147483648"),
            ("intra_threads", "1.5"),
        ] {
            let raw: toml::Table = format!("{key} = {value}").parse().unwrap();
            let error = owner.parse_record(&raw).unwrap_err();
            assert!(error.inner_message().contains(key), "{error}");
        }
    }

    #[test]
    fn instant_cache_deduplicates_per_key() {
        // Use the production instance registry algorithm with fake embedders.
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
        let providers = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            super::super::config::FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(runtime_path),
            }
            .to_record(),
        );
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
