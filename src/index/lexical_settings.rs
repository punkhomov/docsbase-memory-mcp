//! Query-only BM25 field weights owned by the lexical index.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

/// Validated weights for the searchable Tantivy fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexicalWeights {
    pub(super) text: f32,
    pub(super) title: f32,
    pub(super) heading: f32,
    pub(super) identifiers: f32,
}

impl LexicalWeights {
    // SQ8 keeps the least-biased weights that preserve the golden gate.
    pub(crate) const DEFAULTS: Self = Self {
        text: 1.0,
        title: 2.0,
        heading: 1.5,
        identifiers: 2.5,
    };

    fn values(self) -> [f32; 4] {
        [self.text, self.title, self.heading, self.identifiers]
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LexicalPatch {
    #[serde(rename = "text_boost")]
    text: Option<f64>,
    #[serde(rename = "title_boost")]
    title: Option<f64>,
    #[serde(rename = "heading_boost")]
    heading: Option<f64>,
    #[serde(rename = "identifiers_boost")]
    identifiers: Option<f64>,
}

impl LexicalPatch {
    fn values(self) -> [Option<f64>; 4] {
        [self.text, self.title, self.heading, self.identifiers]
    }
}

#[allow(clippy::cast_possible_truncation)] // Check narrowing into Tantivy's score type.
fn resolve_weight(override_value: Option<f64>, inherited: f32) -> Result<f32> {
    let Some(value) = override_value else {
        return Ok(inherited);
    };
    let weight = value as f32;
    if value.is_sign_negative() || !weight.is_finite() || (value > 0.0 && weight == 0.0) {
        return Err(Error::Admission {
            message: "lexical boosts must be finite nonnegative 32-bit weights without underflow"
                .into(),
        });
    }
    Ok(normalize_boost(weight))
}

// Tantivy's LogicalAst elides boosts within this distance from unity.
pub(super) fn normalize_boost(weight: f32) -> f32 {
    if (weight - 1.0).abs() < f32::EPSILON {
        1.0
    } else {
        weight
    }
}

pub(crate) struct LexicalSpec;

impl LexicalSpec {
    pub(crate) fn view_fields(weights: LexicalWeights, view: &mut ViewBuilder<'_>) -> Result<()> {
        for (field, value) in Self::FIELDS.iter().zip(weights.values()) {
            view.push(field.key, serde_json::json!(value), None)?;
        }
        Ok(())
    }
}

impl SettingsSpec for LexicalSpec {
    type Patch = LexicalPatch;
    type Settings = LexicalWeights;
    type Defaults = LexicalWeights;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("lexical.text_boost", Scope::Project, Unit::Multiplier),
        FieldSpec::new("lexical.title_boost", Scope::Project, Unit::Multiplier),
        FieldSpec::new("lexical.heading_boost", Scope::Project, Unit::Multiplier),
        FieldSpec::new(
            "lexical.identifiers_boost",
            Scope::Project,
            Unit::Multiplier,
        ),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        LexicalWeights::DEFAULTS
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
        let candidate = LexicalWeights {
            text: resolve_weight(patch.text, settings.text)?,
            title: resolve_weight(patch.title, settings.title)?,
            heading: resolve_weight(patch.heading, settings.heading)?,
            identifiers: resolve_weight(patch.identifiers, settings.identifiers)?,
        };
        Self::validate(&candidate, &Capabilities::current())?;
        let mut candidate_origins = origins.clone();
        for (field, value) in Self::FIELDS.iter().zip(patch.values()) {
            if value.is_some() {
                candidate_origins.set(field.key, source)?;
            }
        }
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }

    fn validate(weights: &LexicalWeights, _: &Capabilities) -> Result<()> {
        let values = weights.values();
        if values
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
            || values.iter().all(|weight| *weight == 0.0)
        {
            return Err(Error::Admission {
                message:
                    "lexical boosts must be finite and nonnegative with at least one positive boost"
                        .into(),
            });
        }
        Ok(())
    }

    fn impact(
        old: Option<&LexicalWeights>,
        next: &LexicalWeights,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
