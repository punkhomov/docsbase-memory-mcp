//! Pure project ANN search policy. Search settings do not identify persisted
//! graphs; construction and runtime/source composition have separate owners.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::error::{Error, Result};

/// Existing qualified search-beam ceiling, not a total query memory budget.
/// Raising this domain requires separate resource qualification.
pub const QUALIFIED_EF_MAX: usize = 4096;

/// Immutable checked search policy, borrowed per call without shared mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnSearchSettings {
    ef_base: usize,
    ef_max: usize,
    fetch_multiplier: usize,
}
impl AnnSearchSettings {
    /// Check an explicit policy before it can reach HNSW.
    ///
    /// # Errors
    /// Zero fields, base above max, or max above the qualified beam domain.
    pub fn new(ef_base: usize, ef_max: usize, fetch_multiplier: usize) -> Result<Self> {
        let settings = Self {
            ef_base,
            ef_max,
            fetch_multiplier,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Minimum beam for an admitted query.
    #[must_use]
    pub const fn ef_base(self) -> usize {
        self.ef_base
    }
    /// Maximum beam and fetch for the explicit search API.
    #[must_use]
    pub const fn ef_max(self) -> usize {
        self.ef_max
    }
    /// Positive multiplier applied to fetch before capping the beam.
    #[must_use]
    pub const fn fetch_multiplier(self) -> usize {
        self.fetch_multiplier
    }

    /// Compute an admitted beam without wrapping or calling HNSW.
    ///
    /// `hnsw_rs` promotes ef to fetch internally. Rejecting fetch above max
    /// prevents that promotion from bypassing this policy's beam ceiling.
    /// The dependency's visited/candidate storage is not a total RSS bound.
    ///
    /// # Errors
    /// Fetch exceeds this policy's maximum beam.
    pub fn beam_for(self, fetch: usize) -> Result<usize> {
        if fetch > self.ef_max {
            return Err(Error::Admission {
                message: format!(
                    "ANN fetch {fetch} exceeds search policy maximum {}",
                    self.ef_max
                ),
            });
        }
        Ok(self.legacy_beam_for(fetch))
    }

    // Compatibility search historically caps only the ef argument, leaving
    // hnsw_rs free to promote it for fetch > max. Do not claim bounded fetch.
    pub(crate) fn legacy_beam_for(self, fetch: usize) -> usize {
        self.ef_base
            .max(fetch.saturating_mul(self.fetch_multiplier))
            .min(self.ef_max)
    }

    pub(crate) const fn owner_defaults() -> Self {
        Self {
            ef_base: 64,
            ef_max: QUALIFIED_EF_MAX,
            fetch_multiplier: 4,
        }
    }

    fn validate(self) -> Result<()> {
        if self.ef_base == 0
            || self.ef_base > self.ef_max
            || self.ef_max > QUALIFIED_EF_MAX
            || self.fetch_multiplier == 0
        {
            return Err(Error::Admission {
                message: format!(
                    "ANN search requires 1 <= ef_base <= ef_max <= {QUALIFIED_EF_MAX} and positive fetch_multiplier"
                ),
            });
        }
        Ok(())
    }
}

/// Optional global or project overrides for the ANN search owner.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnSearchPatch {
    /// Minimum beam width.
    pub ef_base: Option<usize>,
    /// Maximum beam and explicit fetch.
    pub ef_max: Option<usize>,
    /// Positive multiplier applied to fetch.
    pub fetch_multiplier: Option<usize>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnnConfigPatch {
    pub search: AnnSearchPatch,
}

/// Owner of ANN search defaults, scope, validation and local runtime effect.
pub struct AnnSearchSpec;
impl SettingsSpec for AnnSearchSpec {
    type Patch = AnnSearchPatch;
    type Settings = AnnSearchSettings;
    type Defaults = AnnSearchSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("ann.search.ef_base", Scope::Project, Unit::Count),
        FieldSpec::new("ann.search.ef_max", Scope::Project, Unit::Count),
        FieldSpec::new("ann.search.fetch_multiplier", Scope::Project, Unit::Count),
    ];
    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        AnnSearchSettings::owner_defaults()
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
        // SettingsSpec is public: preserve the private-field invariant even
        // when a caller bypasses Resolution and invokes overlay directly.
        let mut candidate = *settings;
        let mut candidate_origins = origins.clone();
        if let Some(value) = patch.ef_base {
            candidate.ef_base = value;
            candidate_origins.set("ann.search.ef_base", source)?;
        }
        if let Some(value) = patch.ef_max {
            candidate.ef_max = value;
            candidate_origins.set("ann.search.ef_max", source)?;
        }
        if let Some(value) = patch.fetch_multiplier {
            candidate.fetch_multiplier = value;
            candidate_origins.set("ann.search.fetch_multiplier", source)?;
        }
        candidate.validate()?;
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        settings.validate()
    }
    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
