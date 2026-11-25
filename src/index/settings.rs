//! Owned Markdown chunk construction settings.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

/// Checked prose chunk geometry. Fences and tables retain their atomic shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSettings {
    max_chars: u16,
    overlap_chars: u16,
}

const DEFAULT_SETTINGS: ChunkSettings = ChunkSettings {
    max_chars: 1500,
    overlap_chars: 150,
};
const RECIPE_VERSION: i64 = 1;

impl ChunkSettings {
    /// Select an explicit chunk size and overlap allowance.
    ///
    /// # Errors
    /// The char cap is outside 1–1500 or the overlap exceeds 150.
    pub fn new(max_chars: u16, overlap_chars: u16) -> Result<Self> {
        let limits = Self::owner_defaults();
        if max_chars == 0 || max_chars > limits.max_chars || overlap_chars > limits.overlap_chars {
            return Err(Error::Admission {
                message: format!(
                    "index.chunk requires 1 <= max_chars <= {} and overlap_chars <= {}",
                    limits.max_chars, limits.overlap_chars
                ),
            });
        }
        Ok(Self {
            max_chars,
            overlap_chars,
        })
    }

    /// Prose size before overlap.
    #[must_use]
    pub const fn max_chars(self) -> usize {
        self.max_chars as usize
    }

    /// Upper bound on the overlap allowance. Prose tails also obey the
    /// splitter's cap-relative limit.
    #[must_use]
    pub const fn overlap_chars(self) -> usize {
        self.overlap_chars as usize
    }

    pub(crate) const fn owner_defaults() -> Self {
        DEFAULT_SETTINGS
    }

    pub(crate) fn recipe_id(self) -> i64 {
        // Version in the high word, followed by the two 16-bit settings.
        // Bump the version for changes to chunk boundaries or line mapping.
        (RECIPE_VERSION << 32) | (i64::from(self.max_chars) << 16) | i64::from(self.overlap_chars)
    }

    pub(crate) fn view_fields(self, view: &mut ViewBuilder<'_>) -> Result<()> {
        view.push(
            "index.chunk.max_chars",
            serde_json::json!(self.max_chars),
            None,
        )?;
        view.push(
            "index.chunk.overlap_chars",
            serde_json::json!(self.overlap_chars),
            None,
        )
    }
}

/// Partial Markdown chunk overrides.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkPatch {
    /// Prose size before overlap.
    pub max_chars: Option<u16>,
    /// Maximum overlap allowance.
    pub overlap_chars: Option<u16>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexFilePatch {
    pub(crate) chunk: ChunkPatch,
}

/// Owner of file resolution and validation for Markdown chunk geometry.
pub struct ChunkSpec;
impl SettingsSpec for ChunkSpec {
    type Patch = ChunkPatch;
    type Settings = ChunkSettings;
    type Defaults = ChunkSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("index.chunk.max_chars", Scope::Project, Unit::Count),
        FieldSpec::new("index.chunk.overlap_chars", Scope::Project, Unit::Count),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        ChunkSettings::owner_defaults()
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
        let candidate = ChunkSettings::new(
            patch.max_chars.unwrap_or(settings.max_chars),
            patch.overlap_chars.unwrap_or(settings.overlap_chars),
        )?;
        let mut candidate_origins = origins.clone();
        if patch.max_chars.is_some() {
            candidate_origins.set("index.chunk.max_chars", source)?;
        }
        if patch.overlap_chars.is_some() {
            candidate_origins.set("index.chunk.overlap_chars", source)?;
        }
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }

    fn validate(_: &Self::Settings, _: &Capabilities) -> Result<()> {
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
