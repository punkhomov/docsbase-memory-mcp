//! Hybrid stage configuration, defaults and resolution (FR-3, FR-5, FR-13,
//! FR-14).
//!
//! Ungated on purpose: the default offline build compiles the resolver, so
//! the feature-off admission path and the cache-key composition are testable
//! without the network feature (SC-3).

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::vector_cache::Embedder;

use super::Reranker;

/// The only provider accepted in iteration 1 (FR-3).
pub const PROVIDER_OPENROUTER: &str = "openrouter";

/// Default embedding model (FR-33): the free Nemotron route. Pilot T11
/// showed parity or better vs paid qwen-8b on the benchmark probe
/// (Rrf 18 vs 15 hits / 0.594 vs 0.512 nDCG on 20 queries). Paid routes
/// stay available via explicit `models use`.
pub const DEFAULT_EMBEDDING_MODEL: &str = "nvidia/nemotron-3-embed-1b:free";

/// Default dimensions: the model's fixed native width (no MRL truncation;
/// other widths are rejected by the provider with HTTP 400).
pub const DEFAULT_DIMENSIONS: u32 = 2048;

/// Default rerank model (FR-33): the free Nemotron route. Pilot T11 showed
/// 6/6 top-1 agreement with cohere on probe queries; paid routes stay
/// available via explicit `models use`.
pub const DEFAULT_RERANK_MODEL: &str = "nvidia/llama-nemotron-rerank-vl-1b-v2:free";

/// Lowest accepted rerank window (BM25 candidates sent to the provider).
pub const MIN_RERANK_WINDOW: u32 = 10;

/// Highest accepted rerank window.
pub const MAX_RERANK_WINDOW: u32 = 100;

/// Default rerank window (FR-11).
pub const DEFAULT_RERANK_WINDOW: u32 = 50;

/// Candidate-list ordering mode (FR-17).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ranking {
    /// Lexical BM25 order; the default so a ranking change is opt-in (NFR-6).
    #[default]
    Lexical,
    /// Reciprocal Rank Fusion of the BM25 and dense lists (FR-16).
    Rrf,
}

/// Resolved hybrid configuration; the file-level forms (bool shorthand or
/// table) are parsed in `crate::config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HybridCfg {
    /// Master switch; when false both stages stay off (the default).
    pub enabled: bool,
    /// Embedding stage; `None` = stage disabled.
    pub embedding: Option<EmbeddingCfg>,
    /// Rerank stage; `None` = stage disabled.
    pub reranker: Option<RerankerCfg>,
    /// Candidate ordering (FR-17); `Rrf` requires the embedding stage.
    pub ranking: Ranking,
}

impl HybridCfg {
    /// Both stages with defaults; the `hybrid = true` shorthand (FR-13).
    #[must_use]
    pub fn enabled_with_defaults() -> Self {
        Self {
            enabled: true,
            embedding: Some(EmbeddingCfg::default()),
            reranker: Some(RerankerCfg::default()),
            ranking: Ranking::Lexical,
        }
    }
}

/// Embedding stage configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingCfg {
    /// Provider name; only `openrouter` is accepted (FR-3).
    pub provider: String,
    /// Model id as the provider knows it.
    pub model: String,
    /// Requested output width (MRL); part of the cache key (FR-5).
    pub dimensions: u32,
    /// Client-side L2 normalization; part of the cache key (FR-5, FR-8).
    pub normalize: bool,
}

impl Default for EmbeddingCfg {
    fn default() -> Self {
        Self {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: DEFAULT_EMBEDDING_MODEL.to_owned(),
            dimensions: DEFAULT_DIMENSIONS,
            normalize: true,
        }
    }
}

/// Rerank stage configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankerCfg {
    /// Provider name; only `openrouter` is accepted (FR-3).
    pub provider: String,
    /// Model id as the provider knows it.
    pub model: String,
    /// BM25 candidates sent per rerank request (FR-11 bounds).
    pub window: u32,
}

impl Default for RerankerCfg {
    fn default() -> Self {
        Self {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: DEFAULT_RERANK_MODEL.to_owned(),
            window: DEFAULT_RERANK_WINDOW,
        }
    }
}

/// Content-addressed cache key of the embedding stage (FR-5):
/// `openrouter/<model>@d<dim>@<l2|raw>`.
#[must_use]
pub fn cache_model_id(cfg: &EmbeddingCfg) -> String {
    let post = if cfg.normalize { "l2" } else { "raw" };
    format!(
        "{}/{model}@d{dim}@{post}",
        cfg.provider,
        model = cfg.model,
        dim = cfg.dimensions
    )
}

/// Concrete stages handed to the index and search paths.
#[derive(Clone, Default)]
pub struct HybridStages {
    /// Embedding stage, when enabled and resolvable.
    pub embedder: Option<Arc<dyn Embedder>>,
    /// Rerank stage, when enabled and resolvable.
    pub reranker: Option<Arc<dyn Reranker>>,
    /// Resolved candidate ordering (FR-17).
    pub ranking: Ranking,
}

impl std::fmt::Debug for HybridStages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridStages")
            .field("embedder", &self.embedder.as_ref().map(|_| "<embedder>"))
            .field("reranker", &self.reranker.as_ref().map(|_| "<reranker>"))
            .field("ranking", &self.ranking)
            .finish()
    }
}

/// Builds the stages from resolved configuration (FR-3, FR-13, FR-14).
///
/// `feature_on` mirrors `cfg!(feature = "hybrid-remote")` at the call site;
/// tests pass `false` in a feature-build to exercise the offline-build path.
/// A missing API key or an unknown provider is an [`Error::Admission`]: no
/// silent BM25-only fallback (NFR-2, SC-3).
///
/// # Errors
/// Returns [`Error::Admission`] for a feature-off build, an unknown provider,
/// an out-of-range window or a missing `OPENROUTER_API_KEY`.
pub fn from_config(cfg: &HybridCfg, feature_on: bool) -> Result<HybridStages> {
    if !cfg.enabled {
        return Ok(HybridStages::default());
    }
    // A configuration error outranks build capability: rrf without the
    // embedding stage can never resolve, feature or not (FR-17).
    if cfg.ranking == Ranking::Rrf && cfg.embedding.is_none() {
        return Err(Error::Admission {
            message: "hybrid.ranking = \"rrf\" requires the embedding stage (FR-17)".to_owned(),
        });
    }
    // An enabled table with no stage sections has nothing to resolve: it
    // must not demand the network feature or an API key (review T12).
    if cfg.embedding.is_none() && cfg.reranker.is_none() {
        return Ok(HybridStages {
            ranking: cfg.ranking,
            ..HybridStages::default()
        });
    }
    if !feature_on || !cfg!(feature = "hybrid-remote") {
        return Err(feature_missing());
    }
    #[cfg_attr(not(feature = "hybrid-remote"), allow(unused_mut))]
    let mut stages = HybridStages {
        ranking: cfg.ranking,
        ..HybridStages::default()
    };
    if let Some(embedding) = &cfg.embedding {
        ensure_openrouter(&embedding.provider)?;
        if embedding.dimensions == 0 {
            return Err(Error::Admission {
                message: "hybrid.embedding.dimensions must be > 0".to_owned(),
            });
        }
    }
    if let Some(reranker) = &cfg.reranker {
        ensure_openrouter(&reranker.provider)?;
        if !(MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&reranker.window) {
            return Err(Error::Admission {
                message: format!(
                    "hybrid.reranker.window must be between {MIN_RERANK_WINDOW} and {MAX_RERANK_WINDOW}, got {}",
                    reranker.window
                ),
            });
        }
    }
    #[cfg(feature = "hybrid-remote")]
    {
        let client = std::sync::Arc::new(super::openrouter::OpenRouterClient::from_env()?);
        if let Some(embedding) = &cfg.embedding {
            stages.embedder = Some(std::sync::Arc::new(
                super::openrouter::OpenRouterEmbedder::with_client(
                    embedding.clone(),
                    client.clone(),
                ),
            ));
        }
        if let Some(reranker) = &cfg.reranker {
            stages.reranker = Some(std::sync::Arc::new(
                super::openrouter::OpenRouterReranker::with_client(reranker.clone(), client),
            ));
        }
    }
    Ok(stages)
}

fn ensure_openrouter(provider: &str) -> Result<()> {
    if provider == PROVIDER_OPENROUTER {
        return Ok(());
    }
    Err(Error::Admission {
        message: format!(
            "hybrid provider \"{provider}\" is not supported in iteration 1 (only \"{PROVIDER_OPENROUTER}\")"
        ),
    })
}

fn feature_missing() -> Error {
    Error::Admission {
        message: "hybrid is enabled but this build lacks the `hybrid-remote` feature; \
                  rebuild with --features hybrid-remote or set hybrid = false"
            .to_owned(),
    }
}
