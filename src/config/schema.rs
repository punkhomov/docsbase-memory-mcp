//! Data-only settings contracts shared by component owners (CF1).

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, Result};

use super::bootstrap::DefaultsContext;

/// Owner of a setting; project layers cannot change shared resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Machine/bootstrap state.
    Machine,
    /// One daemon-wide resource.
    Daemon,
    /// Per-project behavior.
    Project,
    /// A provider/model shared by its callers.
    Provider,
}

/// A source identity, never a source's values or credentials.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SourceId {
    /// Component-owned baseline.
    Defaults,
    /// Global file.
    Global(PathBuf),
    /// Repository-local file.
    Project(PathBuf),
    /// Explicit command-line patch.
    Cli,
    /// Whitelisted environment variable name.
    Environment(&'static str),
    /// Supplied snapshot whose earlier provenance is unavailable.
    ProvidedSnapshot,
    /// Captured OS facts.
    Os,
}

impl SourceId {
    /// Whether this layer may configure `scope`.
    #[must_use]
    pub fn permits(&self, scope: Scope) -> bool {
        !matches!(self, Self::Project(_)) || scope == Scope::Project
    }
}

/// Units for future effective views; values stay in typed settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Count of objects or attempts.
    Count,
    /// Bytes.
    Bytes,
    /// Duration represented in milliseconds when rendered.
    Milliseconds,
    /// Boolean switch.
    Boolean,
    /// Identifier or enum name.
    Name,
    /// Replaced collection.
    List,
}

/// Descriptor supplied by an owner, without loader-specific validation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// Stable settings key.
    pub key: &'static str,
    /// Owner scope.
    pub scope: Scope,
    /// Presentation unit.
    pub unit: Unit,
}

impl FieldSpec {
    /// Declares one field.
    #[must_use]
    pub const fn new(key: &'static str, scope: Scope, unit: Unit) -> Self {
        Self { key, scope, unit }
    }
}

/// Origin of an effective field. No raw source values are retained.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FieldOrigin {
    /// Source path/name only.
    pub source: SourceId,
    /// Owning scope.
    pub scope: Scope,
    /// Presentation unit.
    pub unit: Unit,
}

/// Per-field provenance, shared by resolution and future effective views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    fields: BTreeMap<&'static str, FieldOrigin>,
}

impl Provenance {
    pub(crate) fn initial(fields: &[FieldSpec], source: &SourceId) -> Self {
        Self {
            fields: fields
                .iter()
                .map(|field| {
                    (
                        field.key,
                        FieldOrigin {
                            source: source.clone(),
                            scope: field.scope,
                            unit: field.unit,
                        },
                    )
                })
                .collect(),
        }
    }

    /// Origin of a resolved field.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FieldOrigin> {
        self.fields.get(key)
    }

    /// All origins in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &FieldOrigin)> {
        self.fields.iter().map(|(key, value)| (*key, value))
    }

    /// Changes an origin after an owner applies a patch.
    ///
    /// # Errors
    /// An undeclared field or a source outside its declared scope.
    pub fn set(&mut self, key: &'static str, source: &SourceId) -> Result<()> {
        let field = self
            .fields
            .get_mut(key)
            .ok_or_else(|| Error::internal(format!("undeclared settings field {key}")))?;
        if !source.permits(field.scope) {
            return Err(Error::Admission {
                message: format!("settings scope forbids project override of {key}"),
            });
        }
        field.source = source.clone();
        Ok(())
    }

    /// Rebinds a legacy field to an owner's versioned descriptor after layering.
    /// Validates before mutation so a rejected projection preserves all origins.
    pub(crate) fn rebind(
        &mut self,
        legacy_key: &'static str,
        field: FieldSpec,
        source: &SourceId,
    ) -> Result<()> {
        if !self.fields.contains_key(legacy_key) || self.fields.contains_key(field.key) {
            return Err(Error::internal("invalid provenance field projection"));
        }
        if !source.permits(field.scope) {
            return Err(Error::Admission {
                message: "source cannot own projected settings scope".to_owned(),
            });
        }
        self.fields.remove(legacy_key);
        self.fields.insert(
            field.key,
            FieldOrigin {
                source: source.clone(),
                scope: field.scope,
                unit: field.unit,
            },
        );
        Ok(())
    }

    pub(crate) fn replace_group(&mut self, prefix: &str, source: &SourceId) -> Result<()> {
        let keys: Vec<_> = self
            .fields
            .keys()
            .copied()
            .filter(|key| key.starts_with(prefix))
            .collect();
        for key in keys {
            self.set(key, source)?;
        }
        Ok(())
    }
}

/// Partial input: absence, a supplied value and semantic disabling differ.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PatchValue<T> {
    /// No override.
    #[default]
    Inherit,
    /// Explicit value.
    Set(T),
    /// Disable an optional facility.
    Disable,
}

/// Build capabilities, available without loading feature-gated providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Local provider implementation compiled in.
    pub hybrid_local: bool,
    /// Remote provider implementation compiled in.
    pub hybrid_remote: bool,
}

impl Capabilities {
    /// Capabilities of this build; not a provisioning or network probe.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            hybrid_local: cfg!(feature = "hybrid-local"),
            hybrid_remote: cfg!(feature = "hybrid-remote"),
        }
    }
}

/// Validated settings and their provenance; consumers only borrow immutable data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<T> {
    settings: T,
    provenance: Provenance,
}

impl<T> Resolved<T> {
    pub(crate) fn new(settings: T, provenance: Provenance) -> Self {
        Self {
            settings,
            provenance,
        }
    }
    /// Complete validated settings.
    #[must_use]
    pub const fn settings(&self) -> &T {
        &self.settings
    }
    /// Sources for the settings.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }
    /// Compatibility boundary for legacy owned settings.
    #[must_use]
    pub fn into_settings(self) -> T {
        self.settings
    }
}

/// Stable identity of an observed artifact; no storage handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactId {
    /// Owning component.
    pub component: String,
    /// Project when the artifact is project-scoped.
    pub project_id: Option<i64>,
}

/// Conservative state returned by future read-only inspectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    /// No artifact exists.
    Missing,
    /// Its compatibility cannot yet be established.
    Unknown,
    /// A coherent read needs explicit recovery first.
    NeedsRecovery,
    /// Verified identity is available.
    Ready,
}

/// One observation used by an owner; no reads occur in this DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFact {
    /// Observed artifact.
    pub id: ArtifactId,
    /// Readiness/compatibility state.
    pub state: ArtifactState,
    /// Relevant non-secret identity, if established.
    pub identity: Option<String>,
}

/// Data-only inputs for future change analysis; no planner is introduced in CF1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtifactFacts {
    /// Available bounded observations.
    pub artifacts: Vec<ArtifactFact>,
}

/// Local runtime effect, before a future planner assigns execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentImpact {
    /// No local change.
    Unchanged,
    /// First construction.
    InitializeRuntime,
    /// Settings require a runtime policy/instance update.
    ReconfigureRuntime,
}

impl ComponentImpact {
    /// Classifies data-only settings comparison.
    #[must_use]
    pub fn between<T: PartialEq>(old: Option<&T>, next: &T) -> Self {
        match old {
            None => Self::InitializeRuntime,
            Some(previous) if previous == next => Self::Unchanged,
            Some(_) => Self::ReconfigureRuntime,
        }
    }
}

/// Pure, typed rules owned by one component, not by the composition loader.
pub trait SettingsSpec {
    /// Optional source fields or component operations.
    type Patch;
    /// Complete settings; runtime objects/credentials do not belong here.
    type Settings: Clone;
    /// Immutable owner defaults, including any legacy reset templates.
    type Defaults;
    /// Component scope enforced by the common resolver.
    const SCOPE: Scope;
    /// Fields exposed in provenance/effective views.
    const FIELDS: &'static [FieldSpec];
    /// Composition can aggregate owner descriptors without copying their keys.
    #[must_use]
    fn fields() -> Vec<FieldSpec> {
        Self::FIELDS.to_vec()
    }
    /// Computes defaults once per resolution from already captured facts.
    fn defaults(context: &DefaultsContext) -> Self::Defaults;
    /// Selects the initial settings from the defaults catalog.
    fn initial(defaults: &Self::Defaults) -> Self::Settings;
    /// Applies a typed patch and records affected origins.
    ///
    /// # Errors
    /// Unsupported values or scope violations, without state changes.
    fn overlay(
        settings: &mut Self::Settings,
        patch: Self::Patch,
        defaults: &Self::Defaults,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()>;
    /// Validates structural settings, never provisioning or network state.
    ///
    /// # Errors
    /// Invalid values or unsupported combinations.
    fn validate(settings: &Self::Settings, capabilities: &Capabilities) -> Result<()>;
    /// Reports a local effect without performing any runtime action.
    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        facts: &ArtifactFacts,
    ) -> ComponentImpact;
}
