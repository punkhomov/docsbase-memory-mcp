//! Project ANN construction and search policy.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::error::{Error, Result};

const MIN_QUALIFIED_M: usize = 8;

/// The pinned HNSW dump format requires the dependency's full layer count.
pub(crate) const PERSISTED_LAYERS: usize = 16;

// This recipe predates versioned slot names. Keep it bound to the original
// directory even if a later release chooses a different default.
pub(super) const LEGACY_BUILD_RECIPE: AnnBuildSettings = AnnBuildSettings {
    m: 16,
    ef_construction: 200,
};

/// Checked settings that identify the construction recipe of a graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnnBuildSettings {
    m: usize,
    ef_construction: usize,
}

impl AnnBuildSettings {
    /// Admit a recipe within the current construction and memory domain.
    ///
    /// # Errors
    /// Fan-out is outside 8–16, or the construction beam is outside
    /// twice the fan-out through 200.
    pub fn new(m: usize, ef_construction: usize) -> Result<Self> {
        let max_m = LEGACY_BUILD_RECIPE.m();
        let max_ef = LEGACY_BUILD_RECIPE.ef_construction();
        if !(MIN_QUALIFIED_M..=max_m).contains(&m) || !(2 * m..=max_ef).contains(&ef_construction) {
            return Err(Error::Admission {
                message: format!(
                    "ANN build requires {MIN_QUALIFIED_M} <= m <= {max_m} and 2 * m <= ef_construction <= {max_ef}"
                ),
            });
        }
        Ok(Self { m, ef_construction })
    }

    /// Graph fan-out on the upper layers. The bottom layer can use twice this.
    #[must_use]
    pub const fn m(self) -> usize {
        self.m
    }

    /// Beam width used while inserting a point.
    #[must_use]
    pub const fn ef_construction(self) -> usize {
        self.ef_construction
    }

    pub(crate) const fn owner_defaults() -> Self {
        LEGACY_BUILD_RECIPE
    }

    pub(crate) fn view_fields(self, view: &mut crate::config::view::ViewBuilder<'_>) -> Result<()> {
        view.push("ann.build.m", serde_json::json!(self.m()), None)?;
        view.push(
            "ann.build.ef_construction",
            serde_json::json!(self.ef_construction()),
            None,
        )
    }
}

/// Partial construction settings for global and project layers.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnBuildPatch {
    /// Graph fan-out override.
    pub m: Option<usize>,
    /// Construction beam override.
    pub ef_construction: Option<usize>,
}

/// Owner of the graph construction defaults and validated recipe.
pub struct AnnBuildSpec;
impl SettingsSpec for AnnBuildSpec {
    type Patch = AnnBuildPatch;
    type Settings = AnnBuildSettings;
    type Defaults = AnnBuildSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("ann.build.m", Scope::Project, Unit::Count),
        FieldSpec::new("ann.build.ef_construction", Scope::Project, Unit::Count),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        AnnBuildSettings::owner_defaults()
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
        let candidate = AnnBuildSettings::new(
            patch.m.unwrap_or(settings.m),
            patch.ef_construction.unwrap_or(settings.ef_construction),
        )?;
        let mut candidate_origins = origins.clone();
        if patch.m.is_some() {
            candidate_origins.set("ann.build.m", source)?;
        }
        if patch.ef_construction.is_some() {
            candidate_origins.set("ann.build.ef_construction", source)?;
        }
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }

    fn validate(_: &Self::Settings, _: &Capabilities) -> Result<()> {
        // The private settings fields can only come from a checked recipe.
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
    #[serde(default, deserialize_with = "decode_table")]
    pub search: Option<AnnSearchPatch>,
    #[serde(default, deserialize_with = "decode_table")]
    pub build: Option<AnnBuildPatch>,
}

impl AnnConfigPatch {
    pub(crate) fn decode(table: toml::Table) -> Result<Self> {
        let patch: Self = table.try_into().map_err(|_| Error::Admission {
            message: "invalid ANN settings".to_owned(),
        })?;
        if patch.search.is_none() && patch.build.is_none() {
            return Err(Error::Admission {
                message: "ann must contain a search or build table".to_owned(),
            });
        }
        Ok(patch)
    }
}

fn decode_table<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let table = <toml::Table as serde::Deserialize>::deserialize(deserializer)?;
    table.try_into().map(Some).map_err(serde::de::Error::custom)
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
