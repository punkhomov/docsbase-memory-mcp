//! Query-time settings owned by reciprocal rank fusion.

use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

/// Validated rank constant and lane weights for one project's fusion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RrfSettings {
    pub(super) rank_constant: u32,
    pub(super) lexical_weight: f32,
    pub(super) dense_weight: f32,
}

impl RrfSettings {
    pub(super) const DEFAULTS: Self = Self {
        rank_constant: super::RRF_K,
        lexical_weight: 1.0,
        dense_weight: 1.0,
    };
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RrfPatch {
    rank_constant: Option<u32>,
    lexical_weight: Option<f64>,
    dense_weight: Option<f64>,
}

pub(crate) struct RrfSpec;

pub(crate) fn standalone() -> Result<RrfSettings> {
    let resolved = resolve_component::<RrfSpec>(&DefaultsContext::standalone(), [])?;
    Ok(*resolved.settings())
}

impl RrfSpec {
    pub(crate) fn view_fields(settings: RrfSettings, view: &mut ViewBuilder<'_>) -> Result<()> {
        for (field, value) in Self::FIELDS.iter().zip([
            serde_json::json!(settings.rank_constant),
            serde_json::json!(settings.lexical_weight),
            serde_json::json!(settings.dense_weight),
        ]) {
            view.push(field.key, value, None)?;
        }
        Ok(())
    }
}

#[allow(clippy::cast_possible_truncation)] // Validate the narrowed score representation.
fn resolve_weight(value: Option<f64>, inherited: f32) -> f32 {
    value.map_or(inherited, |weight| weight as f32)
}

impl SettingsSpec for RrfSpec {
    type Patch = RrfPatch;
    type Settings = RrfSettings;
    type Defaults = RrfSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("rrf.rank_constant", Scope::Project, Unit::Count),
        FieldSpec::new("rrf.lexical_weight", Scope::Project, Unit::Multiplier),
        FieldSpec::new("rrf.dense_weight", Scope::Project, Unit::Multiplier),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        RrfSettings::DEFAULTS
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
        let candidate = RrfSettings {
            rank_constant: patch.rank_constant.unwrap_or(settings.rank_constant),
            lexical_weight: resolve_weight(patch.lexical_weight, settings.lexical_weight),
            dense_weight: resolve_weight(patch.dense_weight, settings.dense_weight),
        };
        Self::validate(&candidate, &Capabilities::current())?;
        let mut next_origins = origins.clone();
        for (field, overridden) in Self::FIELDS.iter().zip([
            patch.rank_constant.is_some(),
            patch.lexical_weight.is_some(),
            patch.dense_weight.is_some(),
        ]) {
            if overridden {
                next_origins.set(field.key, source)?;
            }
        }
        *settings = candidate;
        *origins = next_origins;
        Ok(())
    }

    fn validate(settings: &RrfSettings, _: &Capabilities) -> Result<()> {
        if settings.rank_constant == 0 {
            return Err(Error::Admission {
                message: "rrf.rank_constant must be positive".into(),
            });
        }
        for (key, weight) in [
            ("rrf.lexical_weight", settings.lexical_weight),
            ("rrf.dense_weight", settings.dense_weight),
        ] {
            if !weight.is_finite() || weight <= 0.0 {
                return Err(Error::Admission {
                    message: format!("{key} must be a finite positive 32-bit weight"),
                });
            }
        }
        Ok(())
    }

    fn impact(old: Option<&RrfSettings>, next: &RrfSettings, _: &ArtifactFacts) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
