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

use super::config::{
    EmbeddingCfg, PROVIDER_FASTEMBED, PROVIDER_OPENROUTER, ProviderStatus, ProvidersCfg,
    RerankerCfg,
};
use crate::error::{Error, Result};

/// Role of a model. Lives here rather than in `models` so the registry can name
/// it in every build: `models` is gated by `hybrid-local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Produces vectors.
    Embedding,
    /// Scores documents against a query.
    Rerank,
}

impl Task {
    /// Human label used by CLI output and admissions.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Rerank => "rerank",
        }
    }
}

/// One entry of a provider's closed catalog (data only, no ML types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    /// Model name as the provider knows it (no provider prefix).
    pub name: &'static str,
    /// Which stage the model serves.
    pub task: Task,
    /// Vector width the model produces.
    pub dims: usize,
    /// Pinned revision the catalog entry refers to.
    pub revision: &'static str,
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
        let Some((provider, model)) = raw.split_once('/') else {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" is not a model address; write \"<provider>/<model>\" (providers: {})",
                    provider_names()
                ),
            });
        };
        if provider.is_empty() {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" has an empty provider; write \"<provider>/<model>\" (providers: {})",
                    provider_names()
                ),
            });
        }
        if model.is_empty() {
            return Err(Error::Admission {
                message: format!(
                    "\"{raw}\" has an empty model name; write \"<provider>/<model>\" (providers: {})",
                    provider_names()
                ),
            });
        }
        // Validates the provider half (unknown names never reach a stage).
        crate::hybrid::provider::provider(provider)?;
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

    /// Readiness for the status table (FR-7).
    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus;

    /// Provider-specific validation and defaults for the embedding stage.
    ///
    /// # Errors
    /// [`Error::Admission`] for unknown models, wrong task and invalid dims.
    fn resolve_embedding(&self, model: &str, dims: Option<u32>) -> Result<EmbeddingCfg>;

    /// Provider-specific validation and defaults for the rerank stage.
    ///
    /// # Errors
    /// [`Error::Admission`] for unknown models, wrong task and out-of-range
    /// windows.
    fn resolve_reranker(&self, model: &str, window: u32) -> Result<RerankerCfg>;

    /// Closed catalog of known models, or `None` when the provider has none
    /// (remote APIs accept arbitrary ids).
    fn catalog(&self, _task: Task) -> Option<&'static [CatalogModel]> {
        None
    }
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

    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus {
        super::config::provider_status(providers, PROVIDER_FASTEMBED)
    }

    fn resolve_embedding(&self, model: &str, dims: Option<u32>) -> Result<EmbeddingCfg> {
        super::config::resolve_embedding(PROVIDER_FASTEMBED, Some(model.to_owned()), dims, true)
    }

    fn resolve_reranker(&self, model: &str, window: u32) -> Result<RerankerCfg> {
        super::config::resolve_reranker(PROVIDER_FASTEMBED, Some(model.to_owned()), window)
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

    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus {
        super::config::provider_status(providers, PROVIDER_OPENROUTER)
    }

    fn resolve_embedding(&self, model: &str, dims: Option<u32>) -> Result<EmbeddingCfg> {
        super::config::resolve_embedding(PROVIDER_OPENROUTER, Some(model.to_owned()), dims, true)
    }

    fn resolve_reranker(&self, model: &str, window: u32) -> Result<RerankerCfg> {
        super::config::resolve_reranker(PROVIDER_OPENROUTER, Some(model.to_owned()), window)
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
/// [`Error::Admission`] listing the registered names.
pub fn provider(name: &str) -> Result<&'static dyn Provider> {
    providers()
        .iter()
        .copied()
        .find(|p| p.name() == name)
        .ok_or_else(|| Error::Admission {
            message: format!(
                "unknown provider \"{name}\" (registered: {})",
                provider_names()
            ),
        })
}

/// Comma-separated registry names, for admissions.
#[must_use]
pub fn provider_names() -> String {
    providers()
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a build feature is enabled. The single place that maps a feature
/// name to `cfg!`, so providers never grow their own `cfg!` chains.
#[must_use]
pub fn feature_enabled(feature: &str) -> bool {
    match feature {
        "hybrid-local" => cfg!(feature = "hybrid-local"),
        "hybrid-remote" => cfg!(feature = "hybrid-remote"),
        _ => false,
    }
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
    fn feature_enabled_matches_the_build() {
        assert_eq!(
            feature_enabled("hybrid-local"),
            cfg!(feature = "hybrid-local")
        );
        assert_eq!(
            feature_enabled("hybrid-remote"),
            cfg!(feature = "hybrid-remote")
        );
        assert!(!feature_enabled("no-such-feature"));
    }
}
