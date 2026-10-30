//! Hybrid stage configuration, defaults and resolution (FR-3, FR-5, FR-13,
//! FR-14).
//!
//! Ungated on purpose: the default offline build compiles the resolver, so
//! the feature-off admission path and the cache-key composition are testable
//! without the network feature (SC-3).

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::hybrid::provider::Provider;
use crate::vector_cache::{CacheLimits, Embedder};

use super::Reranker;

/// The only provider accepted in iteration 1 (FR-3).
pub const PROVIDER_OPENROUTER: &str = "openrouter";

/// Local fastembed provider (ADR-17, spec hybrid-local).
pub const PROVIDER_FASTEMBED: &str = "fastembed";

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
    /// Vector-cache capacity policy (DELTA-9).
    pub cache: CacheCfg,
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
            cache: CacheCfg::default(),
        }
    }
}

/// Vector-cache capacity policy (hybrid-providers DELTA-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheCfg {
    /// Total bytes of cached vectors; ANN needs full coverage.
    pub max_bytes: u64,
}

impl Default for CacheCfg {
    fn default() -> Self {
        Self {
            max_bytes: CacheLimits::default().max_bytes,
        }
    }
}

impl CacheCfg {
    /// Cache limits for this policy.
    #[must_use]
    pub fn limits(&self) -> CacheLimits {
        CacheLimits {
            max_bytes: self.max_bytes,
            ..CacheLimits::default()
        }
    }
}

/// Embedding stage configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingCfg {
    /// Provider name: `fastembed` (hybrid-local) or `openrouter`
    /// (hybrid-remote) — allowed sets are feature-gated at resolution
    /// (FR-3, FR-5).
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
    /// Provider name: `fastembed` (hybrid-local) or `openrouter`
    /// (hybrid-remote) — allowed sets are feature-gated at resolution
    /// (FR-3, FR-5).
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

/// One `[providers.<name>]` record, provider-agnostic: the common `enabled`
/// flag plus the provider's own keys, kept exactly as written so a provider
/// module can give them a typed view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderRecord {
    /// Provider prepared on this machine.
    pub enabled: bool,
    /// Provider-specific keys (`runtime`, `runtime_path`, …).
    pub params: toml::Table,
}

impl ProviderRecord {
    /// A record with `enabled` and no provider-specific keys.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            params: toml::Table::new(),
        }
    }
}

/// Machine-local provider records from the **global** config only (FR-1):
/// `[providers.<name>]`. Project files have no `providers` field
/// (structural), so machine capabilities are never committed to repositories.
///
/// Generic on purpose: a new provider needs no change here (FR-4, ADR-18).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProvidersCfg {
    entries: std::collections::BTreeMap<String, ProviderRecord>,
}

impl ProvidersCfg {
    /// The record of `name`, when the global config has one.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ProviderRecord> {
        self.entries.get(name)
    }

    /// Every configured provider, name-ordered.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProviderRecord)> {
        self.entries.iter()
    }

    /// Inserts (or replaces) a record; used by the config loader, the CLI
    /// writers and tests.
    pub fn insert(&mut self, name: impl Into<String>, record: ProviderRecord) {
        self.entries.insert(name.into(), record);
    }

    /// Builder form of [`insert`](Self::insert).
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, record: ProviderRecord) -> Self {
        self.insert(name, record);
        self
    }
}

/// Typed view of `[providers.fastembed]` (ADR-17).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FastembedProvider {
    /// Provider prepared on this machine.
    pub enabled: bool,
    /// Installed ONNX Runtime version, when known.
    pub runtime: Option<String>,
    /// Explicit runtime library path override (FR-11 order).
    pub runtime_path: Option<PathBuf>,
}

impl FastembedProvider {
    /// The generic record this view serialises to.
    #[must_use]
    pub fn to_record(&self) -> ProviderRecord {
        let mut params = toml::Table::new();
        if let Some(runtime) = &self.runtime {
            params.insert("runtime".to_owned(), toml::Value::String(runtime.clone()));
        }
        if let Some(path) = &self.runtime_path {
            params.insert(
                "runtime_path".to_owned(),
                toml::Value::String(path.display().to_string()),
            );
        }
        ProviderRecord {
            enabled: self.enabled,
            params,
        }
    }

    /// Reads the view out of a validated record.
    ///
    /// # Errors
    /// [`Error::Admission`] when a key has the wrong type (only reachable for
    /// a record built programmatically, not for a parsed file).
    pub fn from_record(record: &ProviderRecord) -> Result<Self> {
        let runtime = match record.params.get("runtime") {
            None => None,
            Some(toml::Value::String(value)) => Some(value.clone()),
            Some(_) => return Err(type_error("providers.fastembed", "runtime", "a string")),
        };
        let runtime_path = match record.params.get("runtime_path") {
            None => None,
            Some(toml::Value::String(value)) => Some(PathBuf::from(value)),
            Some(_) => {
                return Err(type_error(
                    "providers.fastembed",
                    "runtime_path",
                    "a string",
                ));
            }
        };
        Ok(Self {
            enabled: record.enabled,
            runtime,
            runtime_path,
        })
    }
}

/// Typed view of `[providers.openrouter]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenrouterProvider {
    /// Key validated and recorded on this machine.
    pub enabled: bool,
}

impl OpenrouterProvider {
    /// The generic record this view serialises to.
    #[must_use]
    pub fn to_record(&self) -> ProviderRecord {
        ProviderRecord::new(self.enabled)
    }
}

/// Admission for a provider key of the wrong TOML type.
pub(crate) fn type_error(where_: &str, key: &str, want: &str) -> Error {
    Error::Admission {
        message: format!("{where_}.{key} must be {want}"),
    }
}

/// Typed `fastembed` record, if the global config has one.
///
/// # Errors
/// Propagates a wrong-type key.
pub fn fastembed_record(providers: &ProvidersCfg) -> Result<Option<FastembedProvider>> {
    providers
        .get(PROVIDER_FASTEMBED)
        .map(FastembedProvider::from_record)
        .transpose()
}

/// Typed `openrouter` record, if the global config has one.
///
/// # Errors
/// Propagates a wrong-type key.
pub fn openrouter_record(providers: &ProvidersCfg) -> Result<Option<OpenrouterProvider>> {
    Ok(providers
        .get(PROVIDER_OPENROUTER)
        .map(|record| OpenrouterProvider {
            enabled: record.enabled,
        }))
}

/// A provider's readiness state (FR-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderState {
    /// No provider entry in the global config.
    Unknown,
    /// Entry exists but the provider is not prepared.
    NotConnected,
    /// Prepared (runtime/version recorded).
    Connected,
    /// Entry exists but is unusable (e.g. broken path).
    Broken,
}

/// Provider readiness with a human reason (FR-2 status table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    /// Readiness state.
    pub state: ProviderState,
    /// Resolved runtime library the provider would use, when one exists.
    /// Carried separately from [`reason`](Self::reason) so a `Connected`
    /// row can show its path in the path column (round-7 C1).
    pub path: Option<std::path::PathBuf>,
    /// One-line diagnostic for non-Connected states.
    pub reason: Option<String>,
}

impl ProviderStatus {
    /// Builds a status with no resolved path.
    pub(crate) fn state_only(state: ProviderState, reason: Option<String>) -> Self {
        Self {
            state,
            path: None,
            reason,
        }
    }
}

/// Data-dir ORT install path when one exists. Only the `hybrid-local` build
/// can install a runtime, so the default build has nothing to resolve.
#[cfg(feature = "hybrid-local")]
pub(crate) fn data_dir_library() -> Option<std::path::PathBuf> {
    crate::hybrid::runtime::data_dir_library_path()
}

#[cfg(not(feature = "hybrid-local"))]
pub(crate) fn data_dir_library() -> Option<std::path::PathBuf> {
    None
}

/// Readiness of one provider (FR-2). `fastembed` is `Connected` when the
/// entry exists, is enabled and a runtime library actually resolves — the
/// recorded `runtime_path` or, when absent, the data-dir install that
/// `runtime::resolve` would pick (FR-2/deepseek round-6: reporting a
/// working data-dir install as `NotConnected` printed a wrong remedy).
/// `OpenRouter` is `Connected` when enabled.
#[must_use]
pub fn provider_status(providers: &ProvidersCfg, name: &str) -> ProviderStatus {
    match crate::hybrid::provider::provider(name) {
        Ok(provider) => provider.status(providers),
        // An unknown name is a diagnostic, not an internal error: the status
        // table must keep printing rows for names it does not know. The reason
        // is the bare message — the `admission refused:` taxonomy prefix and
        // the address-form hint belong to CLI errors, not to a status row
        // (NFR-4).
        Err(err) => ProviderStatus {
            state: ProviderState::Unknown,
            path: None,
            reason: Some(err.inner_message()),
        },
    }
}

/// Provider-aware resolution of the embedding stage (FR-5): `OpenRouter`
/// fills its documented defaults when absent; `fastembed` requires an
/// explicit `model`, takes dims from the registry when absent, rejects a
/// mismatch, and requires `normalize = true` (FR-9).
///
/// # Errors
/// Returns [`Error::Admission`] for an unknown provider, a missing fastembed
/// model, a wrong non-registry dim, or `normalize = false` on fastembed.
pub fn resolve_embedding(
    provider: &str,
    model: Option<String>,
    dimensions: Option<u32>,
    normalize: bool,
) -> Result<EmbeddingCfg> {
    match provider {
        PROVIDER_OPENROUTER => Ok(EmbeddingCfg {
            provider: provider.to_owned(),
            model: model.unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_owned()),
            dimensions: dimensions.unwrap_or(DEFAULT_DIMENSIONS),
            normalize,
        }),
        PROVIDER_FASTEMBED => {
            #[cfg(feature = "hybrid-local")]
            {
                let model = model.ok_or_else(|| Error::Admission {
                    message: "hybrid.embedding: provider \"fastembed\" requires an explicit `model` (registry name); run `docsbase models list`"
                        .to_owned(),
                })?;
                if !normalize {
                    return Err(Error::Admission {
                        message:
                            "hybrid.embedding: fastembed registry models require normalize = true"
                                .to_owned(),
                    });
                }
                let entry = crate::hybrid::models::entry(&model)?;
                let dim = entry.dim.ok_or_else(|| Error::Admission {
                    message: format!(
                        "hybrid.embedding: \"{model}\" is a rerank model, not an embedding model"
                    ),
                })?;
                let dim = u32::try_from(dim).map_err(|_| Error::Admission {
                    message: format!(
                        "hybrid.embedding: registry dim for \"{model}\" is out of range"
                    ),
                })?;
                match dimensions {
                    None => Ok(EmbeddingCfg {
                        provider: provider.to_owned(),
                        model,
                        dimensions: dim,
                        normalize: true,
                    }),
                    Some(given) if given == dim => Ok(EmbeddingCfg {
                        provider: provider.to_owned(),
                        model,
                        dimensions: given,
                        normalize: true,
                    }),
                    Some(given) => Err(Error::Admission {
                        message: format!(
                            "hybrid.embedding: dimensions {given} do not match the registry dimension {dim} for \"{model}\""
                        ),
                    }),
                }
            }
            #[cfg(not(feature = "hybrid-local"))]
            {
                let _ = (model, dimensions, normalize);
                Err(Error::Admission {
                    message: "hybrid.embedding: provider \"fastembed\" requires the `hybrid-local` build feature"
                        .to_owned(),
                })
            }
        }
        _ => Err(Error::Admission {
            message: format!(
                "hybrid.embedding: unknown provider \"{provider}\" (supported: \"{PROVIDER_OPENROUTER}\", \"{PROVIDER_FASTEMBED}\")"
            ),
        }),
    }
}

/// Provider-aware resolution of the rerank stage (FR-5): `OpenRouter` fills
/// its default model; `fastembed` requires an explicit registry model of the
/// `Rerank` task.
///
/// # Errors
/// Returns [`Error::Admission`] for an unknown provider, a missing fastembed
/// model, or an embedding model selected for rerank.
pub fn resolve_reranker(provider: &str, model: Option<String>, window: u32) -> Result<RerankerCfg> {
    match provider {
        PROVIDER_OPENROUTER => Ok(RerankerCfg {
            provider: provider.to_owned(),
            model: model.unwrap_or_else(|| DEFAULT_RERANK_MODEL.to_owned()),
            window,
        }),
        PROVIDER_FASTEMBED => {
            #[cfg(feature = "hybrid-local")]
            {
                let model = model.ok_or_else(|| Error::Admission {
                    message: "hybrid.reranker: provider \"fastembed\" requires an explicit `model` (registry name); run `docsbase models list`"
                        .to_owned(),
                })?;
                let entry = crate::hybrid::models::entry(&model)?;
                if entry.task != crate::hybrid::models::Task::Rerank {
                    return Err(Error::Admission {
                        message: format!(
                            "hybrid.reranker: \"{model}\" is an embedding model, not a rerank model"
                        ),
                    });
                }
                Ok(RerankerCfg {
                    provider: provider.to_owned(),
                    model,
                    window,
                })
            }
            #[cfg(not(feature = "hybrid-local"))]
            {
                let _ = (model, window);
                Err(Error::Admission {
                    message: "hybrid.reranker: provider \"fastembed\" requires the `hybrid-local` build feature"
                        .to_owned(),
                })
            }
        }
        _ => Err(Error::Admission {
            message: format!(
                "hybrid.reranker: unknown provider \"{provider}\" (supported: \"{PROVIDER_OPENROUTER}\", \"{PROVIDER_FASTEMBED}\")"
            ),
        }),
    }
}

/// Legacy `OpenRouter` cache key (FR-5): `openrouter/<model>@d<dim>@<l2|raw>`.
/// Kept byte-identical so existing vector caches are not orphaned by the
/// provider-aware formatter (T3, FR-9).
fn legacy_cache_model_id(cfg: &EmbeddingCfg) -> String {
    let post = if cfg.normalize { "l2" } else { "raw" };
    format!(
        "{}/{model}@d{dim}@{post}",
        cfg.provider,
        model = cfg.model,
        dim = cfg.dimensions
    )
}

/// The single canonical cache key (FR-9, SC-10): `OpenRouter` keeps the
/// legacy format byte-for-byte; `fastembed` uses `models::canonical_id`
/// (one formatter). The embedder, `coverage()` and the ANN warm path all
/// call this function.
#[must_use]
pub fn embedding_model_id(cfg: &EmbeddingCfg) -> String {
    #[cfg(feature = "hybrid-local")]
    if cfg.provider == PROVIDER_FASTEMBED {
        // Resolution (FR-5) guarantees the entry exists before this runs
        // (`resolve_embedding` rejects unknown models), so the fallback
        // below is unreachable through validated config; it exists only to
        // keep this pure formatter infallible. It must NOT be used for new
        // fastembed keys (a legacy-shaped key would drop the revision/prompt
        // components and could collide across revisions, FR-9).
        if let Some(id) = crate::hybrid::models::canonical_id_for(
            &cfg.model,
            cfg.dimensions as usize,
            cfg.normalize,
        ) {
            return id;
        }
    }
    legacy_cache_model_id(cfg)
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
/// Build capability is compile-time now (`Provider::enabled`), so there is no
/// feature flag to pass in: a stage naming a provider this build cannot use is
/// an [`Error::Admission`], as is a missing API key or an unknown provider —
/// no silent BM25-only fallback (NFR-2, SC-3).
///
/// # Errors
/// Returns [`Error::Admission`] for a feature-off build, an unknown provider,
/// an out-of-range window or a missing `OPENROUTER_API_KEY`.
pub fn from_config(cfg: &HybridCfg, providers: &ProvidersCfg) -> Result<HybridStages> {
    from_config_with(crate::hybrid::provider::providers(), cfg, providers)
}

/// [`from_config`] against an explicit provider table: the seam the
/// extensibility test uses (FR-10), so a third provider needs no global
/// override.
///
/// # Errors
/// [`Error::Admission`] for a configuration that cannot resolve: `rrf` without
/// an embedding stage, a stage naming an unknown or feature-less provider, an
/// empty cache, or a stage shape the provider rejects.
pub fn from_config_with(
    table: &[&'static dyn Provider],
    cfg: &HybridCfg,
    providers: &ProvidersCfg,
) -> Result<HybridStages> {
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
    validate_stage_shapes(table, cfg)?;
    if cfg.cache.max_bytes == 0 {
        return Err(Error::Admission {
            message: "hybrid.cache.max_bytes must be > 0".to_owned(),
        });
    }
    // Per-provider feature gating: a stage may only demand the feature it
    // needs (FR-3, FR-4, feature matrix rev 0.6).
    let mut needed: Vec<&'static dyn Provider> = Vec::new();
    let stage_providers = [
        cfg.embedding.as_ref().map(|stage| stage.provider.as_str()),
        cfg.reranker.as_ref().map(|stage| stage.provider.as_str()),
    ];
    for name in stage_providers.into_iter().flatten() {
        // `validate_stage_shapes` already proved the name is registered.
        let provider = crate::hybrid::provider::provider_in(table, name)?;
        if !provider.enabled() {
            return Err(provider.feature_missing());
        }
        if !needed.iter().any(|known| known.name() == provider.name()) {
            needed.push(provider);
        }
    }
    let mut stages = HybridStages {
        ranking: cfg.ranking,
        ..HybridStages::default()
    };
    for provider in needed {
        let (embedder, reranker) = provider.build_stages(
            cfg.embedding
                .as_ref()
                .filter(|stage| stage.provider == provider.name()),
            cfg.reranker
                .as_ref()
                .filter(|stage| stage.provider == provider.name()),
            providers,
        )?;
        stages.embedder = stages.embedder.or(embedder);
        stages.reranker = stages.reranker.or(reranker);
    }
    Ok(stages)
}

fn validate_stage_shapes(table: &[&'static dyn Provider], cfg: &HybridCfg) -> Result<()> {
    if let Some(embedding) = &cfg.embedding {
        if crate::hybrid::provider::provider_in(table, &embedding.provider).is_err() {
            return Err(unknown_provider("hybrid.embedding", &embedding.provider));
        }
        if embedding.dimensions == 0 {
            return Err(Error::Admission {
                message: "hybrid.embedding.dimensions must be > 0".to_owned(),
            });
        }
    }
    if let Some(reranker) = &cfg.reranker {
        if crate::hybrid::provider::provider_in(table, &reranker.provider).is_err() {
            return Err(unknown_provider("hybrid.reranker", &reranker.provider));
        }
        if !(MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&reranker.window) {
            return Err(Error::Admission {
                message: format!(
                    "hybrid.reranker.window must be between {MIN_RERANK_WINDOW} and {MAX_RERANK_WINDOW}, got {}",
                    reranker.window
                ),
            });
        }
    }
    Ok(())
}

fn unknown_provider(where_: &str, provider: &str) -> Error {
    Error::Admission {
        message: format!(
            "{where_}: unknown provider \"{provider}\" (supported: \"{PROVIDER_OPENROUTER}\", \"{PROVIDER_FASTEMBED}\")"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_embedding_openrouter_fills_documented_defaults() {
        let cfg = resolve_embedding(PROVIDER_OPENROUTER, None, None, true).expect("resolve");
        assert_eq!(cfg.provider, PROVIDER_OPENROUTER);
        assert_eq!(cfg.model, DEFAULT_EMBEDDING_MODEL);
        assert_eq!(cfg.dimensions, DEFAULT_DIMENSIONS);
        assert!(cfg.normalize);
        // Explicit values win.
        let explicit = resolve_embedding(
            PROVIDER_OPENROUTER,
            Some("org/model".to_owned()),
            Some(1024),
            false,
        )
        .expect("resolve");
        assert_eq!(explicit.model, "org/model");
        assert_eq!(explicit.dimensions, 1024);
        assert!(!explicit.normalize);
    }

    #[test]
    fn resolve_reranker_openrouter_fills_default_model() {
        let cfg = resolve_reranker(PROVIDER_OPENROUTER, None, 30).expect("resolve");
        assert_eq!(cfg.model, DEFAULT_RERANK_MODEL);
        assert_eq!(cfg.window, 30);
    }

    #[test]
    fn resolve_rejects_unknown_providers() {
        let err = resolve_embedding("no-such-provider", None, None, true).expect_err("unknown");
        assert!(matches!(err, Error::Admission { .. }));
        let err = resolve_reranker("no-such-provider", None, 30).expect_err("unknown");
        assert!(matches!(err, Error::Admission { .. }));
    }

    #[test]
    fn embedding_model_id_openrouter_is_byte_identical_to_legacy() {
        let cfg = EmbeddingCfg {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: "nvidia/nemotron-3-embed-1b:free".to_owned(),
            dimensions: 2048,
            normalize: true,
        };
        // The provider-aware formatter must never drift from the legacy
        // format: compare against the legacy function itself and pin the
        // exact bytes.
        assert_eq!(embedding_model_id(&cfg), legacy_cache_model_id(&cfg));
        assert_eq!(
            embedding_model_id(&cfg),
            "openrouter/nvidia/nemotron-3-embed-1b:free@d2048@l2"
        );
        let raw = EmbeddingCfg {
            normalize: false,
            ..cfg.clone()
        };
        assert_eq!(embedding_model_id(&raw), legacy_cache_model_id(&raw));
        assert_eq!(
            embedding_model_id(&raw),
            "openrouter/nvidia/nemotron-3-embed-1b:free@d2048@raw"
        );
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn fastembed_resolution_requires_model_and_validates_dims() {
        // Missing model → admission, never the OpenRouter default (FR-5).
        let err = resolve_embedding(PROVIDER_FASTEMBED, None, None, true).expect_err("no model");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("model"));
        // Missing dims → registry dim.
        let cfg = resolve_embedding(
            PROVIDER_FASTEMBED,
            Some("multilingual-e5-small".to_owned()),
            None,
            true,
        )
        .expect("resolve");
        assert_eq!(cfg.dimensions, 384);
        // Wrong dims → admission.
        let err = resolve_embedding(
            PROVIDER_FASTEMBED,
            Some("multilingual-e5-small".to_owned()),
            Some(128),
            true,
        )
        .expect_err("wrong dims");
        assert!(matches!(err, Error::Admission { .. }));
        // normalize = false → admission (FR-9).
        let err = resolve_embedding(
            PROVIDER_FASTEMBED,
            Some("multilingual-e5-small".to_owned()),
            None,
            false,
        )
        .expect_err("normalize false");
        assert!(matches!(err, Error::Admission { .. }));
        // An embedding model for rerank → admission.
        let err = resolve_reranker(
            PROVIDER_FASTEMBED,
            Some("multilingual-e5-small".to_owned()),
            30,
        )
        .expect_err("embedding model for rerank");
        assert!(matches!(err, Error::Admission { .. }));
        // A rerank model resolves.
        let rerank = resolve_reranker(
            PROVIDER_FASTEMBED,
            Some("jina-reranker-v1-turbo-en".to_owned()),
            30,
        )
        .expect("resolve");
        assert_eq!(rerank.window, 30);
    }

    /// Fake embedder whose model id comes from the canonical formatter — the
    /// same source the real fastembed embedder will use (T4).
    #[cfg_attr(not(feature = "hybrid-local"), allow(dead_code))]
    struct FakeFastembedEmbedder {
        model_id: String,
    }

    impl crate::vector_cache::Embedder for FakeFastembedEmbedder {
        fn model_id(&self) -> &str {
            &self.model_id
        }

        #[allow(clippy::cast_precision_loss)] // test embedder, dimension width only
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| vec![text.len() as f32; 384])
                .collect())
        }
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn coverage_and_ann_warm_use_the_canonical_id() {
        use crate::daemon::registry;
        use crate::daemon::tools::run_project_index;
        use crate::hybrid::dense::{AnnPolicy, DenseCache};
        use crate::hybrid::jobs::{coverage_with, embed_project_chunks_with};
        use crate::store::Db;

        let cache = tempfile::TempDir::new().expect("cache");
        let root = tempfile::TempDir::new().expect("root");
        std::fs::write(root.path().join("a.md"), "# A\n\nwidgets here.\n").expect("doc");
        let mut db = Db::open(cache.path()).expect("db");
        let project =
            registry::ensure_project(&mut db, &root.path().canonicalize().expect("canonical"))
                .expect("project");
        run_project_index(
            &mut db,
            cache.path(),
            &project,
            &crate::config::Config::default(),
        )
        .expect("index");

        let cfg = EmbeddingCfg {
            provider: PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            dimensions: 384,
            normalize: true,
        };
        let model_id = embedding_model_id(&cfg);
        let embedder = FakeFastembedEmbedder {
            model_id: model_id.clone(),
        };
        assert_eq!(embedder.model_id(), model_id, "same key as the formatter");

        let before = coverage_with(&db, &embedder, project.id).expect("coverage before");
        assert!(before.total > 0);
        assert_eq!(before.cached, 0);
        let backfilled = embed_project_chunks_with(&db, &embedder, project.id).expect("backfill");
        assert!(backfilled >= before.total);
        let after = coverage_with(&db, &embedder, project.id).expect("coverage after");
        assert_eq!(
            after.cached, after.total,
            "100% coverage under the canonical key"
        );
        // The ANN warm path is driven with the same canonical id (server.rs
        // uses embedding_model_id): warm the dense cache under the key and
        // read it back — the rows carry the model id internally.
        let dense = DenseCache::with_policy(AnnPolicy::BuildOrLoad, cache.path());
        dense.warm(&db, project.id, &model_id).expect("ann warm");
        // The dense read-back under the same key must resolve (the rows carry
        // the canonical model id internally); a second warm is a no-op.
        dense.get(&db, project.id, &model_id).expect("dense read");
        dense
            .warm(&db, project.id, &model_id)
            .expect("warm idempotent");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn embed_model_id_fastembed_matches_canonical_for_all_registry_rows() {
        use crate::hybrid::models::{REGISTRY, Task, canonical_id};
        for entry in REGISTRY {
            if entry.task != Task::Embedding {
                continue;
            }
            let dim = entry.dim.expect("embedding dim");
            let cfg = EmbeddingCfg {
                provider: PROVIDER_FASTEMBED.to_owned(),
                model: entry.name.to_owned(),
                dimensions: u32::try_from(dim).expect("dim fits"),
                normalize: true,
            };
            assert_eq!(
                embedding_model_id(&cfg),
                canonical_id(entry, dim, true),
                "single canonical formatter for {}",
                entry.name
            );
        }
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn provider_status_without_runtime_path_matches_data_dir_install() {
        // Round-6 deepseek: an entry with no `runtime_path` resolves to the
        // data-dir install. `provider_status` used to report `NotConnected`
        // for that documented shape while `--check`/`resolve` found the
        // library — `docsbase connect` then printed a wrong remedy. Both
        // branches are asserted explicitly on a sandboxed data dir so the
        // test is host-independent.
        use crate::hybrid::runtime::test_env::{TestDataDir, runtime_test_guard};
        let _guard = runtime_test_guard();
        let data = TestDataDir::new();
        let providers = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: None,
            }
            .to_record(),
        );

        // Empty data dir: nothing to fall back to.
        let empty = provider_status(&providers, PROVIDER_FASTEMBED);
        assert_eq!(empty.state, ProviderState::NotConnected, "{empty:?}");
        assert!(
            empty
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("connect fastembed")),
            "{empty:?}"
        );

        // With a data-dir install the same entry is Connected and names the
        // path the resolver would pick.
        let lib =
            crate::hybrid::runtime::test_env::data_dir_library_slot().expect("target resolved");
        std::fs::create_dir_all(lib.parent().expect("library parent")).expect("create install dir");
        std::fs::write(&lib, b"fake").expect("write library");
        assert_eq!(
            crate::hybrid::runtime::data_dir_library_path(),
            Some(lib.clone()),
            "the resolver now sees the fake install"
        );

        let connected = provider_status(&providers, PROVIDER_FASTEMBED);
        assert_eq!(connected.state, ProviderState::Connected, "{connected:?}");
        assert_eq!(connected.path.as_deref(), Some(lib.as_path()));
        assert_eq!(connected.reason, None, "Connected rows carry no diagnostic");
        drop(data);
    }

    #[test]
    fn provider_status_maps_states() {
        use ProviderState::{Broken, Connected, NotConnected, Unknown};
        // The no-`runtime_path` case consults the data-dir install, so
        // sandbox `DOCSBASE_DATA_DIR` (empty ⇒ NotConnected) and serialize
        // on the runtime test lock — otherwise the assertion depends on the
        // host machine (round-6 deepseek).
        // Only the `hybrid-local` build consults the data dir, so the
        // sandbox (and its lock) exist only there.
        #[cfg(feature = "hybrid-local")]
        let _guard = crate::hybrid::runtime::test_env::runtime_test_guard();
        #[cfg(feature = "hybrid-local")]
        let _data = crate::hybrid::runtime::test_env::TestDataDir::new();
        let none = ProvidersCfg::default();
        assert_eq!(provider_status(&none, PROVIDER_FASTEMBED).state, Unknown);
        assert_eq!(provider_status(&none, PROVIDER_OPENROUTER).state, Unknown);

        let runtime_file = tempfile::TempDir::new().expect("tmp");
        let runtime_path = runtime_file.path().join("libonnxruntime.so");
        std::fs::write(&runtime_path, b"fake").expect("touch runtime");
        let connected_fastembed = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(runtime_path.clone()),
            }
            .to_record(),
        );
        let status = provider_status(&connected_fastembed, PROVIDER_FASTEMBED);
        assert_eq!(status.state, Connected);
        assert_eq!(
            status.path.as_deref(),
            Some(runtime_path.as_path()),
            "resolved library is reported in the path field"
        );
        assert_eq!(status.reason, None, "Connected rows carry no diagnostic");

        let disabled = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            FastembedProvider {
                enabled: false,
                runtime: None,
                runtime_path: None,
            }
            .to_record(),
        );
        assert_eq!(
            provider_status(&disabled, PROVIDER_FASTEMBED).state,
            NotConnected
        );

        let no_runtime = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            FastembedProvider {
                enabled: true,
                runtime: None,
                runtime_path: None,
            }
            .to_record(),
        );
        assert_eq!(
            provider_status(&no_runtime, PROVIDER_FASTEMBED).state,
            NotConnected
        );

        // A recorded runtime path that vanished is Broken, not Connected
        // (FR-2).
        let broken = ProvidersCfg::default().with(
            PROVIDER_FASTEMBED,
            FastembedProvider {
                enabled: true,
                runtime: Some("1.24.4".to_owned()),
                runtime_path: Some(PathBuf::from("/nonexistent/onnxruntime.so")),
            }
            .to_record(),
        );
        assert_eq!(provider_status(&broken, PROVIDER_FASTEMBED).state, Broken);

        let connected_openrouter = ProvidersCfg::default().with(
            PROVIDER_OPENROUTER,
            OpenrouterProvider { enabled: true }.to_record(),
        );
        assert_eq!(
            provider_status(&connected_openrouter, PROVIDER_OPENROUTER).state,
            Connected
        );
    }
}
