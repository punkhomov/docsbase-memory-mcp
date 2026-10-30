//! Provider registry and model addressing (spec `provider-agnostic`, ADR-18).
//!
//! Providers are data, not branches: one static table, one lookup by name. The
//! table is **ungated by build features** — a build without `hybrid-local`
//! still knows the `fastembed` provider exists (it must parse
//! `[providers.fastembed]` written by a feature-enabled build, FR-4); the
//! feature only limits *use*, and that check happens when a stage is built.
//!
//! Provisioning deliberately does not live here: it is a CLI concern
//! (`src/cli/connect.rs`), so the core never imports `cli` and the default
//! offline build never drags `ureq`/`zip` in through this table.

use std::sync::Arc;

use super::config::{
    EmbeddingCfg, PROVIDER_FASTEMBED, PROVIDER_OPENROUTER, ProviderRecord, ProviderStatus,
    ProvidersCfg, RerankerCfg,
};
use crate::error::{Error, Result};
use crate::hybrid::Reranker;
use crate::vector_cache::Embedder;

/// Role of a model. Lives here rather than in `models` so the registry can name
/// it in every build: `models` is gated by `hybrid-local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Produces vectors.
    Embedding,
    /// Scores documents against a query.
    Rerank,
}

/// A model address: `<provider>/<model>` (FR-1).
///
/// The provider is the segment before the **first** `/`; the model is the rest,
/// verbatim — remote model ids contain `/` themselves
/// (`openrouter/nvidia/nemotron-3-embed-1b:free`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelAddress<'a> {
    provider: &'a str,
    model: &'a str,
}

impl<'a> ModelAddress<'a> {
    /// Splits `raw` into provider and model.
    ///
    /// # Errors
    /// [`Error::Admission`] when the address has no `/`, an empty provider or
    /// an empty model, or names a provider that is not registered.
    pub fn parse(raw: &'a str) -> Result<Self> {
        Self::parse_with(providers(), raw)
    }

    /// Like [`parse`](Self::parse), but validates the provider half against an
    /// explicit table — the seam the extensibility test uses (FR-10).
    ///
    /// # Errors
    /// Same as [`parse`](Self::parse).
    pub fn parse_with(table: &[&'static dyn Provider], raw: &'a str) -> Result<Self> {
        // Callers hand over CLI arguments; the today's CLI trims them, so the
        // parser does too (a trailing space would otherwise land in `model`).
        let raw = raw.trim();
        let Some((provider, model)) = raw.split_once('/') else {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" is not a model address; write \"<provider>/<model>\" (providers: {})",
                    provider_names_in(table)
                ),
            });
        };
        if provider.is_empty() {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" has an empty provider; write \"<provider>/<model>\" (providers: {})",
                    provider_names_in(table)
                ),
            });
        }
        // An empty first model segment (`openrouter//x`) would be persisted and
        // sent to the API; FR-1 lists it as an admission.
        if model.is_empty() || model.starts_with('/') {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" has an empty model name; write \"<provider>/<model>\" (providers: {})",
                    provider_names_in(table)
                ),
            });
        }
        // Validates the provider half (unknown names never reach a stage).
        provider_in(table, provider)?;
        Ok(Self { provider, model })
    }

    /// Provider half of the address.
    #[must_use]
    pub fn provider(&self) -> &'a str {
        self.provider
    }

    /// Model half of the address, verbatim.
    #[must_use]
    pub fn model(&self) -> &'a str {
        self.model
    }
}

impl std::fmt::Display for ModelAddress<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// `enabled` from a `[providers.<name>]` table (absent = true, as before).
///
/// # Errors
/// [`Error::Admission`] when the key is not a boolean.
fn record_enabled(name: &str, table: &toml::Table) -> Result<bool> {
    match table.get("enabled") {
        None => Ok(true),
        Some(toml::Value::Boolean(value)) => Ok(*value),
        Some(_) => Err(super::config::type_error(
            &format!("providers.{name}"),
            "enabled",
            "a boolean",
        )),
    }
}

/// Rejects keys outside `allowed` (`enabled` is always allowed).
///
/// # Errors
/// [`Error::Admission`] listing the provider's allowed keys.
fn reject_unknown_keys(name: &str, table: &toml::Table, allowed: &[&str]) -> Result<()> {
    let unknown: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "enabled" && !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let mut keys = vec!["enabled"];
    keys.extend_from_slice(allowed);
    Err(Error::Admission {
        message: format!(
            "providers.{name}: unknown key(s) {}; allowed keys: {}",
            unknown.join(", "),
            keys.join(", ")
        ),
    })
}

/// One entry of a provider's closed catalog (data only, no ML types).
///
/// `dims` is `Option` because a rerank model has no vector width
/// (`models::ModelEntry.dim` is `None` for those rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    /// Model name as the provider knows it (no provider prefix).
    pub name: &'static str,
    /// Which stage the model serves.
    pub task: Task,
    /// Vector width for embedding rows.
    pub dims: Option<usize>,
    /// Pinned revision the catalog entry refers to.
    pub revision: &'static str,
}

/// The pair of stages a provider can build: embedding and rerank.
pub type Stages = (Option<Arc<dyn Embedder>>, Option<Arc<dyn Reranker>>);

/// One row of the provider registry (FR-5).
///
/// Everything the core needs to resolve and build a provider's stages. A
/// provider without a catalog or without provisioning simply keeps the
/// defaults.
pub trait Provider: Sync {
    /// Registry name — the first segment of every model address.
    fn name(&self) -> &'static str;

    /// Build feature required to *use* this provider (`None` = always usable).
    /// Absence of the feature never hides the provider from the registry.
    fn feature(&self) -> Option<&'static str> {
        None
    }

    /// Whether this build can actually use the provider. A provider that
    /// declares a [`feature`](Self::feature) must override this with its own
    /// `cfg!(feature = "…")` literal: `cfg!` needs a literal, so the mapping
    /// cannot live in one shared function (a new provider would otherwise be
    /// silently unusable — FR-5, G4).
    fn enabled(&self) -> bool {
        true
    }

    /// Validates this provider's `[providers.<name>]` keys (FR-4). The default
    /// accepts only `enabled` and rejects anything else, so a typo cannot
    /// silently disable a provider; a provider with its own keys overrides
    /// this. Parsing never looks at build features: a file written by a
    /// feature-enabled build must load in every build.
    ///
    /// # Errors
    /// [`Error::Admission`] naming the provider and its allowed keys.
    fn parse_record(&self, table: &toml::Table) -> Result<ProviderRecord> {
        reject_unknown_keys(self.name(), table, &[])?;
        Ok(ProviderRecord::new(record_enabled(self.name(), table)?))
    }

    /// Pre-write check for the embedding stage (FR-8): can this build serve it
    /// *now*? The provider owns the probe, so the CLI never branches on names.
    ///
    /// # Errors
    /// Admission with exactly one corrective action, or an incompatible
    /// response schema from a remote probe.
    fn probe_embedding(&self, _cfg: &EmbeddingCfg) -> Result<()> {
        Ok(())
    }

    /// Pre-write check for the rerank stage (FR-8).
    ///
    /// # Errors
    /// Same as [`probe_embedding`](Self::probe_embedding).
    fn probe_reranker(&self, _cfg: &RerankerCfg) -> Result<()> {
        Ok(())
    }

    /// Whether this provider requires `normalize = true` (registry models are
    /// L2-normalized by contract).
    fn requires_normalize(&self) -> bool {
        false
    }

    /// Canonical cache key of an embedding config, when this provider defines
    /// its own shape. `None` keeps the legacy formatter (FR-9).
    fn cache_model_id(&self, _cfg: &EmbeddingCfg) -> Option<String> {
        None
    }

    /// Closed catalog of known models, or `None` when the provider has none
    /// (remote APIs accept arbitrary ids). Callers filter by [`Task`].
    fn catalog(&self) -> Option<&'static [CatalogModel]> {
        None
    }

    /// Readiness for the status table (FR-7).
    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus;

    /// Prepared version to show in the status table, when the provider records
    /// one (`fastembed` records its ONNX Runtime version).
    fn version(&self, _providers: &ProvidersCfg) -> Option<String> {
        None
    }

    /// Provider-specific validation and defaults for the embedding stage.
    ///
    /// `model = None` means "the provider's default", which only a provider
    /// with a default may accept; `normalize` is the user's setting and is part
    /// of the cache key, so it must pass through unchanged.
    ///
    /// # Errors
    /// [`Error::Admission`] for unknown models, wrong task, missing dims where
    /// the provider has no catalog, and invalid dims.
    fn resolve_embedding(
        &self,
        model: Option<&str>,
        dims: Option<u32>,
        normalize: bool,
    ) -> Result<EmbeddingCfg>;

    /// Builds the stages this provider owns. One call for both stages so a
    /// provider can share expensive resources between them (the `OpenRouter`
    /// client owns a `reqwest::Client` and a `tokio::Runtime`).
    ///
    /// # Errors
    /// Propagates provider construction (missing key, missing runtime/model,
    /// feature-off admission).
    fn build_stages(
        &self,
        embedding: Option<&EmbeddingCfg>,
        reranker: Option<&RerankerCfg>,
        providers: &ProvidersCfg,
    ) -> Result<Stages>;

    /// The admission raised when a stage needs this provider but the build
    /// lacks its feature. Provider-owned so the corrective action stays exact.
    fn feature_missing(&self) -> Error {
        Error::Admission {
            message: format!(
                "hybrid is enabled with a \"{}\" stage but this build lacks the required feature; rebuild with it or set hybrid = false",
                self.name()
            ),
        }
    }

    /// Provider-specific validation and defaults for the rerank stage.
    ///
    /// Implementations must reject a window outside
    /// `MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW` (the config and CLI layers also
    /// check it, for their own messages).
    ///
    /// # Errors
    /// [`Error::Admission`] for unknown models, wrong task and out-of-range
    /// windows.
    fn resolve_reranker(&self, model: Option<&str>, window: u32) -> Result<RerankerCfg>;
}

/// `fastembed` — local ONNX inference behind `hybrid-local` (ADR-17).
struct FastembedProvider;

impl Provider for FastembedProvider {
    fn name(&self) -> &'static str {
        PROVIDER_FASTEMBED
    }

    fn feature(&self) -> Option<&'static str> {
        Some("hybrid-local")
    }

    fn enabled(&self) -> bool {
        cfg!(feature = "hybrid-local")
    }

    fn parse_record(&self, table: &toml::Table) -> Result<ProviderRecord> {
        reject_unknown_keys(PROVIDER_FASTEMBED, table, &["runtime", "runtime_path"])?;
        let enabled = record_enabled(PROVIDER_FASTEMBED, table)?;
        // Reuse the typed view's own type checks instead of duplicating them.
        let mut params = toml::Table::new();
        for key in ["runtime", "runtime_path"] {
            if let Some(value) = table.get(key) {
                if !value.is_str() {
                    return Err(super::config::type_error(
                        &format!("providers.{PROVIDER_FASTEMBED}"),
                        key,
                        "a string",
                    ));
                }
                params.insert((*key).to_owned(), value.clone());
            }
        }
        Ok(ProviderRecord { enabled, params })
    }

    fn probe_embedding(&self, cfg: &EmbeddingCfg) -> Result<()> {
        #[cfg(feature = "hybrid-local")]
        {
            // Runtime first (connect hint), then the cached model (fetch hint).
            let global = crate::config::Config::global()?;
            super::runtime::resolve(&global.providers)?;
            let entry = super::models::entry(&cfg.model)?;
            if !super::models::cached(entry)? {
                return Err(Error::Admission {
                    message: format!(
                        "model \"{}\" is not fetched; run `docsbase models fetch {PROVIDER_FASTEMBED}/{}`",
                        cfg.model, cfg.model
                    ),
                });
            }
            Ok(())
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = cfg;
            Err(self.feature_missing())
        }
    }

    fn probe_reranker(&self, cfg: &RerankerCfg) -> Result<()> {
        #[cfg(feature = "hybrid-local")]
        {
            let global = crate::config::Config::global()?;
            super::runtime::resolve(&global.providers)?;
            let entry = super::models::entry(&cfg.model)?;
            if !super::models::cached(entry)? {
                return Err(Error::Admission {
                    message: format!(
                        "model \"{}\" is not fetched; run `docsbase models fetch {PROVIDER_FASTEMBED}/{}`",
                        cfg.model, cfg.model
                    ),
                });
            }
            Ok(())
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = cfg;
            Err(self.feature_missing())
        }
    }

    fn requires_normalize(&self) -> bool {
        true
    }

    fn cache_model_id(&self, cfg: &EmbeddingCfg) -> Option<String> {
        #[cfg(feature = "hybrid-local")]
        {
            // Resolution (FR-5) guarantees the entry exists before this runs;
            // the fallback keeps the formatter infallible.
            super::models::canonical_id_for(&cfg.model, cfg.dimensions as usize, cfg.normalize)
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = cfg;
            None
        }
    }

    fn catalog(&self) -> Option<&'static [CatalogModel]> {
        #[cfg(feature = "hybrid-local")]
        {
            // Projection of the authoritative registry, built once: a second
            // hand-written catalog would drift from what `fetch` downloads
            // (T1 review P2-3). `catalog_matches_the_registry` pins parity.
            static CATALOG: std::sync::OnceLock<&'static [CatalogModel]> =
                std::sync::OnceLock::new();
            let catalog = CATALOG.get_or_init(|| {
                let rows: Vec<CatalogModel> = super::models::REGISTRY
                    .iter()
                    .map(|entry| CatalogModel {
                        name: entry.name,
                        task: entry.task,
                        dims: entry.dim,
                        revision: entry.revision,
                    })
                    .collect();
                Box::leak(rows.into_boxed_slice())
            });
            Some(catalog)
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            None
        }
    }

    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus {
        use super::config::ProviderState::{Broken, Connected, NotConnected, Unknown};
        let connect = format!("run `docsbase connect {PROVIDER_FASTEMBED}`");
        let Ok(provider) = super::config::fastembed_record(providers) else {
            return ProviderStatus::state_only(Unknown, Some(connect));
        };
        let Some(provider) = provider else {
            return ProviderStatus::state_only(Unknown, Some(connect));
        };
        if !provider.enabled {
            return ProviderStatus::state_only(NotConnected, Some(connect));
        }
        match &provider.runtime_path {
            // A recorded path that no longer exists is Broken, not Connected
            // (FR-2).
            Some(path) if !path.exists() => ProviderStatus {
                state: Broken,
                path: Some(path.clone()),
                reason: Some(format!(
                    "runtime library {} is missing; re-run `docsbase connect {PROVIDER_FASTEMBED} --force`",
                    path.display()
                )),
            },
            Some(path) => ProviderStatus {
                state: Connected,
                path: Some(path.clone()),
                reason: None,
            },
            // No recorded path: the resolver falls back to the data-dir
            // install, so report exactly what it would find (no recursion into
            // `runtime_status`, which calls back here).
            None => match super::config::data_dir_library() {
                Some(path) => ProviderStatus {
                    state: Connected,
                    path: Some(path),
                    reason: None,
                },
                None => ProviderStatus::state_only(
                    NotConnected,
                    Some("runtime not installed; run `docsbase connect fastembed`".to_owned()),
                ),
            },
        }
    }

    fn version(&self, providers: &ProvidersCfg) -> Option<String> {
        super::config::fastembed_record(providers)
            .ok()
            .flatten()
            .and_then(|record| record.runtime)
    }

    fn build_stages(
        &self,
        embedding: Option<&EmbeddingCfg>,
        reranker: Option<&RerankerCfg>,
        providers: &ProvidersCfg,
    ) -> Result<Stages> {
        #[cfg(feature = "hybrid-local")]
        {
            let embedder = embedding
                .map(|cfg| super::local::embedder(cfg, providers))
                .transpose()?;
            let reranker = reranker
                .map(|cfg| super::local::reranker(cfg, providers))
                .transpose()?;
            Ok((embedder, reranker))
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = (embedding, reranker, providers);
            Err(self.feature_missing())
        }
    }

    fn feature_missing(&self) -> Error {
        Error::Admission {
            message: "hybrid is enabled with a fastembed stage but this build lacks the `hybrid-local` feature; \
                      rebuild with --features hybrid-local or set hybrid = false"
                .to_owned(),
        }
    }

    fn resolve_embedding(
        &self,
        model: Option<&str>,
        dims: Option<u32>,
        normalize: bool,
    ) -> Result<EmbeddingCfg> {
        let Some(model) = model else {
            return Err(Error::Admission {
                message: "hybrid.embedding: provider \"fastembed\" requires an explicit `model` (registry name); run `docsbase models list`"
                    .to_owned(),
            });
        };
        if !normalize {
            return Err(Error::Admission {
                message: "hybrid.embedding: fastembed registry models require normalize = true"
                    .to_owned(),
            });
        }
        #[cfg(feature = "hybrid-local")]
        {
            let entry = super::models::entry(model)?;
            let Some(dim) = entry.dim else {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.embedding: \"{model}\" is a rerank model, not an embedding model"
                    ),
                });
            };
            let Ok(dim) = u32::try_from(dim) else {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.embedding: registry dim for \"{model}\" is out of range"
                    ),
                });
            };
            match dims {
                None => Ok(EmbeddingCfg {
                    provider: PROVIDER_FASTEMBED.to_owned(),
                    model: model.to_owned(),
                    dimensions: dim,
                    normalize: true,
                }),
                Some(given) if given == dim => Ok(EmbeddingCfg {
                    provider: PROVIDER_FASTEMBED.to_owned(),
                    model: model.to_owned(),
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
            let _ = (dims, model);
            Err(Error::Admission {
                message: "hybrid.embedding: provider \"fastembed\" requires the `hybrid-local` build feature"
                    .to_owned(),
            })
        }
    }

    fn resolve_reranker(&self, model: Option<&str>, window: u32) -> Result<RerankerCfg> {
        if !(super::config::MIN_RERANK_WINDOW..=super::config::MAX_RERANK_WINDOW).contains(&window)
        {
            return Err(Error::Admission {
                message: format!(
                    "hybrid.reranker.window must be between {} and {}, got {window}",
                    super::config::MIN_RERANK_WINDOW,
                    super::config::MAX_RERANK_WINDOW
                ),
            });
        }
        let Some(model) = model else {
            return Err(Error::Admission {
                message: "hybrid.reranker: provider \"fastembed\" requires an explicit `model` (registry name); run `docsbase models list`"
                    .to_owned(),
            });
        };
        #[cfg(feature = "hybrid-local")]
        {
            let entry = super::models::entry(model)?;
            if entry.task != Task::Rerank {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.reranker: \"{model}\" is an embedding model, not a rerank model"
                    ),
                });
            }
            Ok(RerankerCfg {
                provider: PROVIDER_FASTEMBED.to_owned(),
                model: model.to_owned(),
                window,
            })
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = (window, model);
            Err(Error::Admission {
                message: "hybrid.reranker: provider \"fastembed\" requires the `hybrid-local` build feature"
                    .to_owned(),
            })
        }
    }
}

/// `openrouter` — remote embeddings/rerank behind `hybrid-remote` (ADR-14).
struct OpenrouterProvider;

impl Provider for OpenrouterProvider {
    fn name(&self) -> &'static str {
        PROVIDER_OPENROUTER
    }

    fn feature(&self) -> Option<&'static str> {
        Some("hybrid-remote")
    }

    fn enabled(&self) -> bool {
        cfg!(feature = "hybrid-remote")
    }

    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus {
        use super::config::ProviderState::{Connected, NotConnected, Unknown};
        let record = super::config::openrouter_record(providers).ok().flatten();
        match &record {
            None => ProviderStatus::state_only(
                Unknown,
                Some("run `docsbase connect openrouter`".to_owned()),
            ),
            Some(provider) if provider.enabled => ProviderStatus::state_only(Connected, None),
            Some(_) => ProviderStatus::state_only(
                NotConnected,
                Some("run `docsbase connect openrouter`".to_owned()),
            ),
        }
    }

    fn probe_embedding(&self, cfg: &EmbeddingCfg) -> Result<()> {
        #[cfg(feature = "hybrid-remote")]
        {
            let transport = super::openrouter::ReqwestTransport::new()?;
            super::openrouter::probe_embedding_with(&transport, &cfg.model, cfg.dimensions)
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            let _ = cfg;
            Err(self.feature_missing())
        }
    }

    fn probe_reranker(&self, cfg: &RerankerCfg) -> Result<()> {
        #[cfg(feature = "hybrid-remote")]
        {
            let transport = super::openrouter::ReqwestTransport::new()?;
            super::openrouter::probe_reranker_with(&transport, &cfg.model)
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            let _ = cfg;
            Err(self.feature_missing())
        }
    }

    fn build_stages(
        &self,
        embedding: Option<&EmbeddingCfg>,
        reranker: Option<&RerankerCfg>,
        providers: &ProvidersCfg,
    ) -> Result<Stages> {
        #[cfg(feature = "hybrid-remote")]
        {
            let _ = providers;
            // One client for both stages (it owns an http client and a
            // runtime); built only when a stage actually needs it, so a
            // fastembed-only config never demands `OPENROUTER_API_KEY`.
            let client = Arc::new(super::openrouter::OpenRouterClient::from_env()?);
            let embedder = embedding.map(|cfg| {
                Arc::new(super::openrouter::OpenRouterEmbedder::with_client(
                    cfg.clone(),
                    Arc::clone(&client),
                )) as Arc<dyn Embedder>
            });
            let reranker = reranker.map(|cfg| {
                Arc::new(super::openrouter::OpenRouterReranker::with_client(
                    cfg.clone(),
                    client,
                )) as Arc<dyn Reranker>
            });
            Ok((embedder, reranker))
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            let _ = (embedding, reranker, providers);
            Err(self.feature_missing())
        }
    }

    fn feature_missing(&self) -> Error {
        Error::Admission {
            message: "hybrid is enabled with an OpenRouter stage but this build lacks the `hybrid-remote` feature; \
                      rebuild with --features hybrid-remote or set hybrid = false"
                .to_owned(),
        }
    }

    fn resolve_embedding(
        &self,
        model: Option<&str>,
        dims: Option<u32>,
        normalize: bool,
    ) -> Result<EmbeddingCfg> {
        Ok(EmbeddingCfg {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: model.map_or_else(
                || super::config::DEFAULT_EMBEDDING_MODEL.to_owned(),
                str::to_owned,
            ),
            dimensions: dims.unwrap_or(super::config::DEFAULT_DIMENSIONS),
            normalize,
        })
    }

    fn resolve_reranker(&self, model: Option<&str>, window: u32) -> Result<RerankerCfg> {
        if !(super::config::MIN_RERANK_WINDOW..=super::config::MAX_RERANK_WINDOW).contains(&window)
        {
            return Err(Error::Admission {
                message: format!(
                    "hybrid.reranker.window must be between {} and {}, got {window}",
                    super::config::MIN_RERANK_WINDOW,
                    super::config::MAX_RERANK_WINDOW
                ),
            });
        }
        Ok(RerankerCfg {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: model.map_or_else(
                || super::config::DEFAULT_RERANK_MODEL.to_owned(),
                str::to_owned,
            ),
            window,
        })
    }
}

static FASTEMBED: FastembedProvider = FastembedProvider;
static OPENROUTER: OpenrouterProvider = OpenrouterProvider;

/// The registry itself. A `static` slice (not an array literal per call) so
/// lookups return `'static` entries.
static REGISTRY: [&'static dyn Provider; 2] = [&OPENROUTER, &FASTEMBED];

/// Every registered provider (FR-5, G2). Ungated by features: see the module
/// docs and FR-4.
#[must_use]
pub fn providers() -> &'static [&'static dyn Provider] {
    &REGISTRY
}

/// Looks a provider up by registry name.
///
/// # Errors
/// [`Error::Admission`] listing the registered names and the address form.
pub fn provider(name: &str) -> Result<&'static dyn Provider> {
    provider_in(providers(), name)
}

/// Unknown-provider admission without CLI vocabulary (no address hint): used
/// where the caller is a config file or a status row, not a model address.
///
/// # Errors
/// Always [`Error::Admission`] listing the registered names.
#[must_use]
pub fn unknown_provider(name: &str) -> Error {
    Error::Admission {
        message: format!(
            "unknown provider \"{name}\" (registered: {})",
            provider_names()
        ),
    }
}

/// Like [`provider`], against an explicit table (the FR-10 seam).
///
/// # Errors
/// [`Error::Admission`] listing the registered names and the address form.
pub fn provider_in(table: &[&'static dyn Provider], name: &str) -> Result<&'static dyn Provider> {
    table
        .iter()
        .copied()
        .find(|p| p.name() == name)
        .ok_or_else(|| Error::Admission {
            message: format!(
                "unknown provider \"{name}\" (registered: {}); write \"<provider>/<model>\"",
                provider_names_in(table)
            ),
        })
}

/// Comma-separated registry names, for admissions.
#[must_use]
pub fn provider_names() -> String {
    provider_names_in(providers())
}

/// Comma-separated names of an explicit table (the FR-10 seam).
#[must_use]
pub fn provider_names_in(table: &[&'static dyn Provider]) -> String {
    table
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_address_splits_at_the_first_slash() {
        let addr = ModelAddress::parse("openrouter/nvidia/nemotron-3-embed-1b:free")
            .expect("remote address");
        assert_eq!(addr.provider(), PROVIDER_OPENROUTER);
        assert_eq!(addr.model(), "nvidia/nemotron-3-embed-1b:free");
        assert_eq!(
            addr.to_string(),
            "openrouter/nvidia/nemotron-3-embed-1b:free"
        );

        let local = ModelAddress::parse("fastembed/bge-small-en-v1.5").expect("local address");
        assert_eq!(local.provider(), PROVIDER_FASTEMBED);
        assert_eq!(local.model(), "bge-small-en-v1.5");
    }

    #[test]
    fn model_address_rejects_bare_and_malformed_input() {
        for raw in ["", "bge-small-en-v1.5", "/x", "openrouter/", "nope/x"] {
            let err = ModelAddress::parse(raw).expect_err(raw);
            assert!(
                matches!(err, Error::Admission { .. }),
                "{raw}: expected admission, got {err:?}"
            );
        }
        // Syntax-only layer: it names the form and the registry. The
        // catalog-aware "did you mean fastembed/bge-small-en-v1.5" hint is
        // added by the CLI (FR-2, T4) — parsing must not depend on catalogs.
        let err = ModelAddress::parse("bge-small-en-v1.5").expect_err("bare name");
        let text = err.to_string();
        assert!(text.contains("<provider>/<model>"), "{text}");
        assert!(text.contains(PROVIDER_FASTEMBED), "{text}");
        assert!(text.contains(PROVIDER_OPENROUTER), "{text}");
    }

    #[test]
    fn registry_lists_every_provider_with_its_feature() {
        let names: Vec<_> = providers().iter().map(|p| p.name()).collect();
        assert_eq!(names, vec![PROVIDER_OPENROUTER, PROVIDER_FASTEMBED]);

        let remote = provider(PROVIDER_OPENROUTER).expect("openrouter");
        assert_eq!(remote.feature(), Some("hybrid-remote"));
        let local = provider(PROVIDER_FASTEMBED).expect("fastembed");
        assert_eq!(local.feature(), Some("hybrid-local"));

        // Lookup returns the same static entry (no per-call allocation).
        assert!(std::ptr::eq(
            remote,
            provider(PROVIDER_OPENROUTER).expect("openrouter")
        ));
    }

    #[test]
    fn unknown_provider_lists_the_registry() {
        let Err(err) = provider("ollama") else {
            panic!("unknown provider must be refused");
        };
        let text = err.to_string();
        assert!(text.contains("ollama"), "{text}");
        assert!(text.contains(PROVIDER_FASTEMBED), "{text}");
        assert!(text.contains(PROVIDER_OPENROUTER), "{text}");
    }

    #[test]
    fn provider_enabled_follows_its_own_feature() {
        let remote = provider(PROVIDER_OPENROUTER).expect("openrouter");
        let local = provider(PROVIDER_FASTEMBED).expect("fastembed");
        assert_eq!(remote.enabled(), cfg!(feature = "hybrid-remote"));
        assert_eq!(local.enabled(), cfg!(feature = "hybrid-local"));
        // The registry itself is ungated: both providers exist either way.
        assert_eq!(providers().len(), 2);
    }

    #[test]
    fn parse_with_accepts_a_provider_absent_from_the_global_registry() {
        // The FR-10 seam: a third provider validated against an explicit table.
        struct EchoProvider;
        impl Provider for EchoProvider {
            fn name(&self) -> &'static str {
                "echo"
            }
            fn status(&self, _providers: &ProvidersCfg) -> ProviderStatus {
                ProviderStatus {
                    state: super::super::config::ProviderState::Connected,
                    path: None,
                    reason: None,
                }
            }
            fn resolve_embedding(
                &self,
                model: Option<&str>,
                dims: Option<u32>,
                normalize: bool,
            ) -> Result<EmbeddingCfg> {
                let cfg = EmbeddingCfg {
                    provider: "echo".to_owned(),
                    model: model.unwrap_or("tiny").to_owned(),
                    dimensions: dims.unwrap_or(4),
                    normalize,
                };
                Ok(cfg)
            }
            fn resolve_reranker(&self, _model: Option<&str>, window: u32) -> Result<RerankerCfg> {
                Ok(RerankerCfg {
                    provider: "echo".to_owned(),
                    model: "tiny".to_owned(),
                    window,
                })
            }
            fn build_stages(
                &self,
                _embedding: Option<&EmbeddingCfg>,
                _reranker: Option<&RerankerCfg>,
                _providers: &ProvidersCfg,
            ) -> Result<Stages> {
                Ok((None, None))
            }
        }
        static ECHO: EchoProvider = EchoProvider;
        let table: &[&'static dyn Provider] = &[&ECHO];

        let addr = ModelAddress::parse_with(table, "echo/tiny").expect("echo address");
        assert_eq!(addr.provider(), "echo");
        assert_eq!(addr.model(), "tiny");

        let Err(err) = ModelAddress::parse_with(table, "nope/x") else {
            panic!("unknown provider must be refused");
        };
        let text = err.to_string();
        assert!(text.contains("echo"), "admission lists the table: {text}");
        assert!(!text.contains(PROVIDER_FASTEMBED), "{text}");
        // The global registry still does not know `echo`.
        assert!(ModelAddress::parse("echo/tiny").is_err());
    }

    #[test]
    fn provider_status_golden_unchanged() {
        use super::super::config::{FastembedProvider, OpenrouterProvider, ProviderState};
        use std::path::PathBuf;

        let tmp = tempfile::TempDir::new().expect("tmp");
        let live = tmp.path().join("libonnxruntime.so");
        std::fs::write(&live, b"fake").expect("touch");
        let gone = PathBuf::from("/nonexistent/libonnxruntime.so");

        let cases: Vec<(&str, ProvidersCfg, ProviderState, Option<&str>)> = vec![
            // (provider, config, expected state, expected reason substring)
            (
                PROVIDER_FASTEMBED,
                ProvidersCfg::default(),
                ProviderState::Unknown,
                Some("run `docsbase connect fastembed`"),
            ),
            (
                PROVIDER_FASTEMBED,
                ProvidersCfg::default().with(
                    PROVIDER_FASTEMBED,
                    FastembedProvider {
                        enabled: false,
                        runtime: None,
                        runtime_path: None,
                    }
                    .to_record(),
                ),
                ProviderState::NotConnected,
                Some("run `docsbase connect fastembed`"),
            ),
            (
                PROVIDER_FASTEMBED,
                ProvidersCfg::default().with(
                    PROVIDER_FASTEMBED,
                    FastembedProvider {
                        enabled: true,
                        runtime: Some("1.24.4".to_owned()),
                        runtime_path: Some(live.clone()),
                    }
                    .to_record(),
                ),
                ProviderState::Connected,
                None,
            ),
            (
                PROVIDER_FASTEMBED,
                ProvidersCfg::default().with(
                    PROVIDER_FASTEMBED,
                    FastembedProvider {
                        enabled: true,
                        runtime: Some("1.24.4".to_owned()),
                        runtime_path: Some(gone),
                    }
                    .to_record(),
                ),
                ProviderState::Broken,
                Some("is missing; re-run `docsbase connect fastembed --force`"),
            ),
            (
                PROVIDER_OPENROUTER,
                ProvidersCfg::default(),
                ProviderState::Unknown,
                Some("run `docsbase connect openrouter`"),
            ),
            (
                PROVIDER_OPENROUTER,
                ProvidersCfg::default().with(
                    PROVIDER_OPENROUTER,
                    OpenrouterProvider { enabled: false }.to_record(),
                ),
                ProviderState::NotConnected,
                Some("run `docsbase connect openrouter`"),
            ),
            (
                PROVIDER_OPENROUTER,
                ProvidersCfg::default().with(
                    PROVIDER_OPENROUTER,
                    OpenrouterProvider { enabled: true }.to_record(),
                ),
                ProviderState::Connected,
                None,
            ),
        ];

        for (name, providers, state, reason) in cases {
            let status = super::super::config::provider_status(&providers, name);
            assert_eq!(status.state, state, "{name}: {status:?}");
            match reason {
                None => assert_eq!(status.reason, None, "{name}: {status:?}"),
                Some(fragment) => {
                    let text = status.reason.clone().unwrap_or_default();
                    assert!(text.contains(fragment), "{name}: {text}");
                    // NFR-4: a status row carries no error taxonomy prefix.
                    assert!(!text.contains("admission refused:"), "{name}: {text}");
                }
            }
        }
    }

    #[test]
    fn feature_missing_messages_name_the_declared_feature() {
        // Keeps `feature()` and the message in sync: the string a user reads
        // must name the feature the registry declares (T2 review P3-7).
        for provider in providers() {
            let Some(feature) = provider.feature() else {
                continue;
            };
            let text = provider.feature_missing().to_string();
            assert!(text.contains(feature), "{}: {text}", provider.name());
        }
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn catalog_matches_the_registry() {
        use crate::hybrid::models;
        // The catalog is a projection, so it cannot drift from what `fetch`
        // downloads (T1 review P2-3).
        let local = provider(PROVIDER_FASTEMBED).expect("fastembed");
        let catalog = local.catalog().expect("local provider has a catalog");
        assert_eq!(catalog.len(), models::REGISTRY.len());
        for (row, entry) in catalog.iter().zip(models::REGISTRY.iter()) {
            assert_eq!(row.name, entry.name);
            assert_eq!(row.task, entry.task);
            assert_eq!(row.dims, entry.dim);
            assert_eq!(row.revision, entry.revision);
        }
        // A remote provider has no catalog: its ids are open-ended.
        assert!(
            provider(PROVIDER_OPENROUTER)
                .expect("openrouter")
                .catalog()
                .is_none()
        );
    }

    #[test]
    fn third_provider_needs_only_a_registration() {
        // Scope: the core seams (address, resolution, stage construction,
        // status). The CLI-side validation/probe now go through the same
        // registry lookups; their provider-owned bodies are covered by
        // `probe_embedding`/`requires_normalize` tests per provider.
        // FR-10 / G4: a provider declared entirely in a test module goes
        // through stage construction, status, address parsing and stage
        // resolution. Nothing in `config.rs` or `cli/*` changes for it.
        use crate::hybrid::config::ProviderState;
        use crate::vector_cache::Embedder;

        // Test-only float maths: exact values, no precision concerns.
        #[allow(clippy::cast_precision_loss)]
        fn width(text: &str) -> f32 {
            text.len() as f32
        }

        struct EchoEmbedder;

        impl Embedder for EchoEmbedder {
            fn model_id(&self) -> &'static str {
                "echo/tiny@d4@l2@r0@p1"
            }
            fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(texts
                    .iter()
                    .map(|text| vec![width(text), 0.0, 0.0, 1.0])
                    .collect())
            }
        }

        struct EchoProvider;

        impl Provider for EchoProvider {
            fn name(&self) -> &'static str {
                "echo"
            }
            fn status(&self, _providers: &ProvidersCfg) -> ProviderStatus {
                ProviderStatus {
                    state: ProviderState::Connected,
                    path: None,
                    reason: None,
                }
            }
            fn resolve_embedding(
                &self,
                model: Option<&str>,
                dims: Option<u32>,
                normalize: bool,
            ) -> Result<EmbeddingCfg> {
                Ok(EmbeddingCfg {
                    provider: "echo".to_owned(),
                    model: model.unwrap_or("tiny").to_owned(),
                    dimensions: dims.unwrap_or(4),
                    normalize,
                })
            }
            fn resolve_reranker(&self, _model: Option<&str>, window: u32) -> Result<RerankerCfg> {
                Ok(RerankerCfg {
                    provider: "echo".to_owned(),
                    model: "tiny".to_owned(),
                    window,
                })
            }
            fn build_stages(
                &self,
                embedding: Option<&EmbeddingCfg>,
                _reranker: Option<&RerankerCfg>,
                _providers: &ProvidersCfg,
            ) -> Result<Stages> {
                let stage: Option<Arc<dyn Embedder>> =
                    embedding.map(|_| Arc::new(EchoEmbedder) as Arc<dyn Embedder>);
                Ok((stage, None))
            }
        }

        static ECHO: EchoProvider = EchoProvider;
        let table: &[&'static dyn Provider] = &[&ECHO];

        // 1. Address parsing against the explicit table.
        let address = ModelAddress::parse_with(table, "echo/tiny").expect("echo address");
        assert_eq!(address.provider(), "echo");

        // 2. Stage resolution through the provider.
        let echo = provider_in(table, "echo").expect("echo");
        let cfg = echo
            .resolve_embedding(Some(address.model()), None, true)
            .expect("resolve");
        assert_eq!(cfg.model, "tiny");
        assert_eq!(cfg.dimensions, 4);

        // 3. Stage construction through the generic dispatcher.
        let hybrid = super::super::config::HybridCfg {
            enabled: true,
            embedding: Some(cfg.clone()),
            reranker: None,
            ranking: super::super::config::Ranking::Rrf,
            cache: super::super::config::CacheCfg::default(),
        };
        let stages =
            super::super::config::from_config_with(table, &hybrid, &ProvidersCfg::default())
                .expect("third provider builds a stage");
        let embedder = stages.embedder.expect("echo embedder");
        assert_eq!(embedder.model_id(), "echo/tiny@d4@l2@r0@p1");
        let vector = &embedder.embed(&["abcd".to_owned()]).expect("embed")[0];
        assert!((vector[0] - 4.0).abs() < f32::EPSILON, "{vector:?}");

        // 4. Status goes through the same row.
        let status =
            super::super::config::provider_status_in(table, &ProvidersCfg::default(), "echo");
        assert_eq!(status.state, ProviderState::Connected);
    }

    #[test]
    fn model_address_trims_surrounding_whitespace() {
        let addr = ModelAddress::parse("  fastembed/bge-small-en-v1.5 \n").expect("trimmed");
        assert_eq!(addr.provider(), PROVIDER_FASTEMBED);
        assert_eq!(addr.model(), "bge-small-en-v1.5");
    }
}
