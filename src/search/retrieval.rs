//! Project-owned candidate window policy for document caps.

use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};
use crate::index::tantivy_index::MAX_HITS;

/// Default candidate-window multiplier for document-cap searches.
pub const DEFAULT_POST_FILTER_OVERSCAN: usize = 4;

const KEY: &str = "retrieval.post_filter_overscan";

/// Validated candidate window policy for a project's document-cap searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalSettings {
    post_filter_overscan: usize,
}

impl RetrievalSettings {
    pub(crate) fn lexical_cap_window(self, limit: usize) -> usize {
        limit
            .saturating_mul(self.post_filter_overscan)
            .min(MAX_HITS)
    }

    pub(crate) fn widen_cap_window(self, window: usize, ceiling: usize) -> usize {
        // A factor of one keeps the base window at the requested limit.
        // Growing geometrically still bounds the number of refill rounds.
        window
            .saturating_mul(self.post_filter_overscan.max(2))
            .min(ceiling)
    }
}

pub(crate) fn standalone() -> Result<RetrievalSettings> {
    let resolved = resolve_component::<RetrievalSpec>(&DefaultsContext::standalone(), [])?;
    Ok(*resolved.settings())
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetrievalPatch {
    post_filter_overscan: Option<usize>,
}

pub(crate) struct RetrievalSpec;

impl RetrievalSpec {
    pub(crate) fn view_fields(
        settings: RetrievalSettings,
        view: &mut ViewBuilder<'_>,
    ) -> Result<()> {
        view.push(KEY, serde_json::json!(settings.post_filter_overscan), None)
    }
}

impl SettingsSpec for RetrievalSpec {
    type Patch = RetrievalPatch;
    type Settings = RetrievalSettings;
    type Defaults = RetrievalSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[FieldSpec::new(KEY, Scope::Project, Unit::Multiplier)];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        RetrievalSettings {
            post_filter_overscan: DEFAULT_POST_FILTER_OVERSCAN,
        }
    }

    fn initial(defaults: &Self::Defaults) -> Self::Settings {
        *defaults
    }

    fn overlay(
        settings: &mut Self::Settings,
        patch: Self::Patch,
        _: &Self::Defaults,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if let Some(post_filter_overscan) = patch.post_filter_overscan {
            let next = RetrievalSettings {
                post_filter_overscan,
            };
            Self::validate(&next, &Capabilities::current())?;
            origins.set(KEY, source)?;
            *settings = next;
        }
        Ok(())
    }

    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        if settings.post_filter_overscan == 0 {
            return Err(Error::Admission {
                message: format!("{KEY} must be at least 1"),
            });
        }
        Ok(())
    }

    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
