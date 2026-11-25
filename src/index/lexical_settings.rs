//! Query-only BM25 scoring and collection settings owned by the lexical index.

use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

/// Resolves and validates the owner's defaults for compatibility entry points.
pub(crate) fn standalone() -> Result<LexicalSettings> {
    let resolved = resolve_component::<LexicalSpec>(&DefaultsContext::standalone(), [])?;
    Ok(*resolved.settings())
}

/// Validated BM25 scoring and candidate collection for a project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexicalSettings {
    pub(super) text: f32,
    pub(super) title: f32,
    pub(super) heading: f32,
    pub(super) identifiers: f32,
    pub(super) collection: LexicalCollection,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LexicalCollection {
    pub(super) long_chunk_penalty: f32,
    initial_fetch_multiplier: usize,
    initial_fetch_additive: usize,
    widening_multiplier: usize,
}

impl LexicalCollection {
    pub(super) fn initial_fetch(self, limit: usize, total: usize) -> usize {
        limit
            .saturating_mul(self.initial_fetch_multiplier)
            .max(limit.saturating_add(self.initial_fetch_additive))
            .min(total.max(1))
    }

    pub(super) fn widen(self, fetch: usize, total: usize) -> usize {
        fetch.saturating_mul(self.widening_multiplier).min(total)
    }
}

impl LexicalSettings {
    // SQ8 keeps the least-biased weights that preserve the golden gate.
    const DEFAULTS: Self = Self {
        text: 1.0,
        title: 2.0,
        heading: 1.5,
        identifiers: 2.5,
        collection: LexicalCollection {
            long_chunk_penalty: 0.5,
            initial_fetch_multiplier: 4,
            initial_fetch_additive: 16,
            widening_multiplier: 4,
        },
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
    long_chunk_penalty: Option<f64>,
    initial_fetch_multiplier: Option<usize>,
    initial_fetch_additive: Option<usize>,
    widening_multiplier: Option<usize>,
}

impl LexicalPatch {
    fn overrides(self) -> [bool; 8] {
        [
            self.text.is_some(),
            self.title.is_some(),
            self.heading.is_some(),
            self.identifiers.is_some(),
            self.long_chunk_penalty.is_some(),
            self.initial_fetch_multiplier.is_some(),
            self.initial_fetch_additive.is_some(),
            self.widening_multiplier.is_some(),
        ]
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

#[allow(clippy::cast_possible_truncation)] // The owner checks the range before narrowing.
fn resolve_penalty(override_value: Option<f64>, inherited: f32) -> Result<f32> {
    let Some(value) = override_value else {
        return Ok(inherited);
    };
    let penalty = value as f32;
    if !(value <= 1.0 && penalty > 0.0) {
        return Err(Error::Admission {
            message: "lexical.long_chunk_penalty must be a positive 32-bit weight at most 1".into(),
        });
    }
    Ok(penalty)
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
    pub(crate) fn view_fields(settings: LexicalSettings, view: &mut ViewBuilder<'_>) -> Result<()> {
        let collection = settings.collection;
        let values = settings.values().map(|weight| serde_json::json!(weight));
        let collection_values = [
            serde_json::json!(collection.long_chunk_penalty),
            serde_json::json!(collection.initial_fetch_multiplier),
            serde_json::json!(collection.initial_fetch_additive),
            serde_json::json!(collection.widening_multiplier),
        ];
        for (field, value) in Self::FIELDS
            .iter()
            .zip(values.into_iter().chain(collection_values))
        {
            view.push(field.key, value, None)?;
        }
        Ok(())
    }
}

impl SettingsSpec for LexicalSpec {
    type Patch = LexicalPatch;
    type Settings = LexicalSettings;
    type Defaults = LexicalSettings;
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
        FieldSpec::new(
            "lexical.long_chunk_penalty",
            Scope::Project,
            Unit::Multiplier,
        ),
        FieldSpec::new(
            "lexical.initial_fetch_multiplier",
            Scope::Project,
            Unit::Multiplier,
        ),
        FieldSpec::new(
            "lexical.initial_fetch_additive",
            Scope::Project,
            Unit::Count,
        ),
        FieldSpec::new(
            "lexical.widening_multiplier",
            Scope::Project,
            Unit::Multiplier,
        ),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        LexicalSettings::DEFAULTS
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
        let candidate = LexicalSettings {
            text: resolve_weight(patch.text, settings.text)?,
            title: resolve_weight(patch.title, settings.title)?,
            heading: resolve_weight(patch.heading, settings.heading)?,
            identifiers: resolve_weight(patch.identifiers, settings.identifiers)?,
            collection: LexicalCollection {
                long_chunk_penalty: resolve_penalty(
                    patch.long_chunk_penalty,
                    settings.collection.long_chunk_penalty,
                )?,
                initial_fetch_multiplier: patch
                    .initial_fetch_multiplier
                    .unwrap_or(settings.collection.initial_fetch_multiplier),
                initial_fetch_additive: patch
                    .initial_fetch_additive
                    .unwrap_or(settings.collection.initial_fetch_additive),
                widening_multiplier: patch
                    .widening_multiplier
                    .unwrap_or(settings.collection.widening_multiplier),
            },
        };
        Self::validate(&candidate, &Capabilities::current())?;
        let mut candidate_origins = origins.clone();
        for (field, overridden) in Self::FIELDS.iter().zip(patch.overrides()) {
            if overridden {
                candidate_origins.set(field.key, source)?;
            }
        }
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }

    fn validate(weights: &LexicalSettings, _: &Capabilities) -> Result<()> {
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
        let collection = weights.collection;
        if !(collection.long_chunk_penalty > 0.0 && collection.long_chunk_penalty <= 1.0) {
            return Err(Error::Admission {
                message: "lexical.long_chunk_penalty must be a positive 32-bit weight at most 1"
                    .into(),
            });
        }
        if collection.initial_fetch_multiplier == 0 || collection.widening_multiplier < 2 {
            return Err(Error::Admission {
                message: "lexical.initial_fetch_multiplier must be at least 1 and lexical.widening_multiplier at least 2"
                    .into(),
            });
        }
        Ok(())
    }

    fn impact(
        old: Option<&LexicalSettings>,
        next: &LexicalSettings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}

#[cfg(test)]
mod tests {
    use super::{LexicalCollection, standalone};

    #[test]
    fn lexical_windows_grow_without_exceeding_the_corpus() {
        let defaults = standalone().expect("valid standalone defaults").collection;
        assert_eq!(defaults.initial_fetch(1, 200), 17);
        assert_eq!(defaults.initial_fetch(10, 200), 40);
        assert_eq!(defaults.widen(40, 200), 160);
        assert_eq!(defaults.widen(160, 200), 200);
        assert_eq!(defaults.initial_fetch(10, 0), 1);

        let small = LexicalCollection {
            initial_fetch_multiplier: 1,
            initial_fetch_additive: 0,
            widening_multiplier: 2,
            ..defaults
        };
        assert_eq!(small.initial_fetch(1, 200), 1);
        assert_eq!(small.widen(1, 200), 2);

        let large = LexicalCollection {
            initial_fetch_multiplier: usize::MAX,
            initial_fetch_additive: usize::MAX,
            widening_multiplier: usize::MAX,
            ..defaults
        };
        assert_eq!(large.initial_fetch(10, 200), 200);
        assert_eq!(large.widen(10, 200), 200);
    }
}
