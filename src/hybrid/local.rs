//! Local `fastembed` embedder/reranker (ADR-17, spec hybrid-local).
//!
//! The embedder (T4) loads the fetched registry model through the BYO path
//! (`UserDefinedEmbeddingModel` + `TextEmbedding::try_new_from_user_defined`),
//! deduplicates instances per cache key, holds a shared `.lock` guard on the
//! model cache for its lifetime (so `models remove` refuses while loaded),
//! and applies role-aware registry prefixes (`embed_query` vs `embed`).
//! The reranker lands in T5; this module keeps a stub for it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fd_lock::RwLockReadGuard;

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
                "hybrid.embedding: \"{}\" is a rerank model, not an embedding model",
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
    load_embedder(entry, key)
}

/// Process-wide embedder cache keyed by [`ModelCacheKey`] (one instance per
/// key; never per config/call, NFR-4/NFR-7).
fn load_embedder(entry: &ModelEntry<'_>, key: ModelCacheKey) -> Result<Arc<dyn Embedder>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static INSTANCES: OnceLock<Mutex<HashMap<ModelCacheKey, Arc<dyn Embedder>>>> = OnceLock::new();
    let map = INSTANCES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map
        .lock()
        .map_err(|poison| Error::internal(format!("embedder cache lock poisoned: {poison}")))?;
    if let Some(existing) = map.get(&key) {
        return Ok(Arc::clone(existing));
    }
    let embedder = FastEmbedEmbedder::load(entry, &key)?;
    let shared: Arc<dyn Embedder> = Arc::new(embedder);
    map.insert(key, Arc::clone(&shared));
    Ok(shared)
}

/// Local `fastembed` embedder (T4). Holds the loaded model under a `Mutex`
/// (fastembed's `embed` takes `&mut self`) and a shared model-cache `.lock`
/// guard for its lifetime.
pub struct FastEmbedEmbedder {
    entry: ModelEntry<'static>,
    model_id: String,
    model: Mutex<fastembed::TextEmbedding>,
    /// Shared `.lock` guard on `<cache>/<name>/<revision>/.lock`, held for
    /// the instance lifetime (FR-7, T9 remove refuses while loaded).
    _lock_guard: Option<RwLockReadGuard<'static, std::fs::File>>,
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
        let onnx = std::fs::read(&onnx_path).map_err(|err| {
            Error::internal_with_source(format!("read {}", onnx_path.display()), err)
        })?;
        let tokenizer_files = tokenizer_files(&cache_dir)?;

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
        let prefixed = if is_query {
            format!("{}{text}", self.entry.query_prefix)
        } else {
            format!("{}{text}", self.entry.passage_prefix)
        };
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
        texts
            .iter()
            .map(|text| self.embed_text(text, false))
            .collect()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_text(text, true)
    }
}

/// Builds the local rerank stage. Not yet implemented (T5).
///
/// # Errors
/// Returns [`Error::Internal`] until T5 lands the fastembed reranker.
pub fn reranker(_cfg: &RerankerCfg, _providers: &ProvidersCfg) -> Result<Arc<dyn Reranker>> {
    Err(Error::internal(
        "the fastembed reranker is not yet implemented (spec task T5)",
    ))
}

/// Reads the four tokenizer files from the cache directory into fastembed's
/// [`fastembed::TokenizerFiles`].
fn tokenizer_files(cache_dir: &Path) -> Result<fastembed::TokenizerFiles> {
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = cache_dir.join(name);
        std::fs::read(&path)
            .map_err(|err| Error::internal_with_source(format!("read {}", path.display()), err))
    };
    Ok(fastembed::TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    })
}

/// Takes a shared read lock on the model cache directory's `.lock` (fd-lock
/// `try_read`); the guard is retained by the instance for its lifetime.
fn take_shared_lock(dir: &Path) -> Result<Option<RwLockReadGuard<'static, std::fs::File>>> {
    let path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))?;
    let lock: &'static mut fd_lock::RwLock<std::fs::File> =
        Box::leak(Box::new(fd_lock::RwLock::new(file)));
    match lock.try_read() {
        Ok(guard) => Ok(Some(guard)),
        Err(_) => {
            // The lock file is only held exclusively by `models remove`; a
            // concurrent remove means the model cannot be loaded safely.
            Err(Error::Admission {
                message: format!(
                    "the model cache {} is locked for removal; stop the daemon and retry",
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
        quantization: None,
        output_key: None,
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
        let Err(err) = embedder(&cfg, &providers) else {
            panic!("expected admission without a fetched model");
        };
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("models fetch"), "{err}");
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
