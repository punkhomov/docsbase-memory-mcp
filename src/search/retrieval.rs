//! Project-owned search request defaults and candidate window policy.

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

pub(crate) const DEFAULT_RRF_CANDIDATE_WINDOW: u32 = 50;
pub(crate) const MAX_CHUNKS_PER_DOC: usize = 10;

const OVERSCAN: &str = "retrieval.post_filter_overscan";
const LIMIT: &str = "retrieval.default_limit";
const DEDUPE: &str = "retrieval.default_dedupe";
const PER_DOC: &str = "retrieval.default_n_per_doc";
const RRF_WINDOW: &str = "retrieval.rrf_candidate_window";

/// Whether a search caps chunks from each document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Dedupe {
    None,
    Path,
}

/// Validated defaults and candidate window policy for a project's searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalSettings {
    post_filter_overscan: usize,
    default_limit: usize,
    default_dedupe: Dedupe,
    default_n_per_doc: usize,
    rrf_candidate_window: u32,
}

impl RetrievalSettings {
    pub(crate) const fn default_limit(self) -> usize {
        self.default_limit
    }

    pub(crate) const fn default_dedupe(self) -> Dedupe {
        self.default_dedupe
    }

    pub(crate) const fn default_n_per_doc(self) -> usize {
        self.default_n_per_doc
    }

    pub(crate) const fn rrf_candidate_window(self) -> u32 {
        self.rrf_candidate_window
    }

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
    default_limit: Option<usize>,
    default_dedupe: Option<Dedupe>,
    default_n_per_doc: Option<usize>,
    rrf_candidate_window: Option<u32>,
}

pub(crate) struct RetrievalSpec;

impl RetrievalSpec {
    pub(crate) fn view_fields(
        settings: RetrievalSettings,
        view: &mut ViewBuilder<'_>,
    ) -> Result<()> {
        use serde_json::json;
        for (field, value) in Self::FIELDS.iter().zip([
            json!(settings.post_filter_overscan),
            json!(settings.default_limit),
            json!(settings.default_dedupe),
            json!(settings.default_n_per_doc),
            json!(settings.rrf_candidate_window),
        ]) {
            view.push(field.key, value, None)?;
        }
        Ok(())
    }
}

impl SettingsSpec for RetrievalSpec {
    type Patch = RetrievalPatch;
    type Settings = RetrievalSettings;
    type Defaults = RetrievalSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new(OVERSCAN, Scope::Project, Unit::Multiplier),
        FieldSpec::new(LIMIT, Scope::Project, Unit::Count),
        FieldSpec::new(DEDUPE, Scope::Project, Unit::Name),
        FieldSpec::new(PER_DOC, Scope::Project, Unit::Count),
        FieldSpec::new(RRF_WINDOW, Scope::Project, Unit::Count),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        RetrievalSettings {
            post_filter_overscan: DEFAULT_POST_FILTER_OVERSCAN,
            default_limit: 10,
            default_dedupe: Dedupe::None,
            // Two retains more paths on ru_morph than one while halving repetition.
            default_n_per_doc: 2,
            rrf_candidate_window: DEFAULT_RRF_CANDIDATE_WINDOW,
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
        let next = RetrievalSettings {
            post_filter_overscan: patch
                .post_filter_overscan
                .unwrap_or(settings.post_filter_overscan),
            default_limit: patch.default_limit.unwrap_or(settings.default_limit),
            default_dedupe: patch.default_dedupe.unwrap_or(settings.default_dedupe),
            default_n_per_doc: patch
                .default_n_per_doc
                .unwrap_or(settings.default_n_per_doc),
            rrf_candidate_window: patch
                .rrf_candidate_window
                .unwrap_or(settings.rrf_candidate_window),
        };
        Self::validate(&next, &Capabilities::current())?;
        let mut next_origins = origins.clone();
        for (field, supplied) in Self::FIELDS.iter().zip([
            patch.post_filter_overscan.is_some(),
            patch.default_limit.is_some(),
            patch.default_dedupe.is_some(),
            patch.default_n_per_doc.is_some(),
            patch.rrf_candidate_window.is_some(),
        ]) {
            if supplied {
                next_origins.set(field.key, source)?;
            }
        }
        *settings = next;
        *origins = next_origins;
        Ok(())
    }

    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        if settings.post_filter_overscan == 0 {
            return Err(Error::Admission {
                message: format!("{OVERSCAN} must be at least 1"),
            });
        }
        if settings.default_limit > MAX_HITS {
            return Err(Error::Admission {
                message: format!("{LIMIT} must be 0..={MAX_HITS}"),
            });
        }
        if !(1..=MAX_CHUNKS_PER_DOC).contains(&settings.default_n_per_doc) {
            return Err(Error::Admission {
                message: format!("{PER_DOC} must be 1..={MAX_CHUNKS_PER_DOC}"),
            });
        }
        if !(1..=MAX_HITS).contains(&(settings.rrf_candidate_window as usize)) {
            return Err(Error::Admission {
                message: format!("{RRF_WINDOW} must be 1..={MAX_HITS}"),
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
