//! Pure, bounded change plans. No executor or implicit I/O is available here.
use serde::Serialize;

use super::schema::{ArtifactId, ComponentImpact, SourceId};
use crate::error::{Error, Result};

mod compile;
mod path;
pub use compile::compile;

/// Explicit caller-owned limits, checked on raw input and serialized output.
#[derive(Debug, Clone, Copy)]
pub struct PlanLimits {
    /// Maximum raw actions, including duplicates.
    pub max_actions: usize,
    /// Maximum raw dependency edges, including duplicates.
    pub max_dependencies: usize,
    /// Maximum encoded input/output bytes and raw input path bytes independently.
    /// This bounds logical metadata, not allocator RSS.
    pub max_metadata_bytes: usize,
}

/// Distinguishes runtime policies, persisted artifacts and source files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResourceId {
    /// Component-owned runtime policy.
    Runtime(String),
    /// Component-owned artifact identity.
    Artifact(ArtifactId),
    /// Source scope and path; never raw source values.
    Source(#[serde(serialize_with = "path::source")] SourceId),
}
/// What the caller actually knows, not an assertion of operational readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExpectedState {
    /// Confirmed absence at the observation boundary.
    Missing,
    /// No usable observation.
    Unknown,
    /// Explicit recovery is needed before observing coherent state.
    NeedsRecovery,
    /// Owner-recognized identity of exactly this resource.
    KnownIdentity(String),
    /// Hash of supplied text; filesystem existence has NOT been observed.
    SuppliedSourceRevision(String),
}
/// One exact precondition; conflicting observations cannot be merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    /// Observed subject.
    pub resource: ResourceId,
    /// Observation boundary/state.
    pub state: ExpectedState,
}
/// Effects are deliberately distinct; no generic "invalidate everything" action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// First runtime construction.
    InitializeRuntime,
    /// Replace runtime policy without changing artifact semantics.
    ReconfigureRuntime,
    /// First store initialization, not deletion/rebuilding of existing data.
    InitializeStore,
    /// Recover coherent storage state.
    RecoverStore,
    /// Change a persisted metadata schema.
    MigrateSchema,
    /// Establish strict ownership of the shared vector budget.
    AdoptSharedStore,
    /// Rewrite a source file under an explicit stale-source precondition.
    RewriteSource,
    /// Evict disposable entries while preserving store identity.
    Evict,
    /// Delete a store, distinct from eviction and rebuilding.
    DeleteStore,
    /// Rebuild derived data.
    Rebuild,
}
/// A proposed output; full settings/text remain separately owned by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Target {
    /// Versioned owner policy fingerprint or exact source revision.
    Identity(String),
    /// Explicit new shared capacity, not consent to adopt it.
    VectorBudgetBytes(u64),
    /// Intended metadata format, not proof of row/query readiness.
    MetadataSchemaVersion(u32),
}
/// Non-secret, finite reasons rather than arbitrary provider/source messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No prior policy.
    FirstInitialization,
    /// Component policy differs.
    SettingsChanged,
    /// Artifact absence was observed.
    MissingArtifact,
    /// An older metadata schema was observed.
    SchemaVersionChanged,
    /// Recovery evidence was observed.
    RecoveryRequired,
    /// Explicit versioned source proposal.
    SourceMigration,
    /// Explicit strict shared-budget adoption.
    SharedBudgetOwnership,
}
/// Required permission classes, never granted by compiling or selecting a budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    /// Potentially destructive or irreversible change.
    Destructive,
    /// Remote inference/API expense.
    Remote,
    /// Model/artifact downloads.
    Downloads,
}
/// A single action per resource; dependencies reference other action resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// Subject of this action.
    pub resource: ResourceId,
    /// Distinct effect.
    pub effect: Effect,
    /// Intended output when known.
    pub target: Option<Target>,
    /// Owner-supplied causes, unioned deterministically.
    pub reasons: Vec<Reason>,
    /// Actions required before this one.
    pub dependencies: Vec<ResourceId>,
    /// Required permissions; the plan never contains supplied consent.
    pub required_grants: Vec<Grant>,
}
/// Why preparation/execution cannot be claimed ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    /// No execution path is provided in this planning slice.
    ExecutorUnavailable,
    /// No trustworthy artifact observation.
    ObservationUnavailable,
    /// Unknown/future identity format or schema version.
    UnsupportedIdentity,
    /// Registered project has not been surveyed.
    UninspectedProject,
    /// Operator has not selected the shared capacity.
    VectorBudgetDecisionRequired,
    /// Strict store owner/adoption is not implemented yet.
    StrictStoreAdoptionUnavailable,
    /// Supplied source text is not an observed on-disk precondition.
    SourceObservationRequired,
}
/// A resource-local or whole-assessment blocker.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Blocker {
    /// None for an assessment-wide prerequisite.
    pub resource: Option<ResourceId>,
    /// Stable reason without raw source/provider text.
    pub code: BlockerCode,
}
/// Pure owner output, with no handles or closures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Assessment {
    /// Explicit observations, including unavailable/supplied-only states.
    pub expected: Vec<Observation>,
    /// Requested effects.
    pub actions: Vec<Action>,
    /// Owner prerequisites not met.
    pub blockers: Vec<Blocker>,
}
/// Canonical content covered by the plan identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanBody {
    /// JSON/canonicalization version, independent from `config_version`.
    pub plan_version: u32,
    /// Sorted, deduplicated observations.
    pub expected: Vec<Observation>,
    /// Deterministic topological order.
    pub actions: Vec<Action>,
    /// Known identities without changes or resource blockers; no readiness claim.
    pub unchanged_observations: Vec<ResourceId>,
    /// Union of required permissions.
    pub required_grants: Vec<Grant>,
    /// All unresolved owner/executor prerequisites.
    pub blockers: Vec<Blocker>,
}
/// A reviewable plan, not an authorization token or executable program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangePlan {
    /// Domain-separated digest of the canonical body.
    pub plan_id: String,
    /// Versioned data covered by the digest.
    #[serde(flatten)]
    pub body: PlanBody,
}

pub(crate) fn invalid(message: &'static str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}

/// Hash a fixed-size owner policy; callers must not pass raw credentials.
pub(crate) fn policy_identity(domain: &str, value: &impl Serialize) -> Result<String> {
    compile::digest(domain, value, usize::MAX).map(|hash| format!("{domain}:{hash}"))
}

pub(crate) fn runtime_assessment(
    component: &str,
    old: Option<String>,
    next: String,
    impact: ComponentImpact,
) -> Assessment {
    let resource = ResourceId::Runtime(component.to_owned());
    let expected = vec![Observation {
        resource: resource.clone(),
        state: old.map_or(ExpectedState::Missing, ExpectedState::KnownIdentity),
    }];
    let (effect, reason) = match impact {
        ComponentImpact::Unchanged => {
            return Assessment {
                expected,
                ..Assessment::default()
            };
        }
        ComponentImpact::InitializeRuntime => {
            (Effect::InitializeRuntime, Reason::FirstInitialization)
        }
        ComponentImpact::ReconfigureRuntime => {
            (Effect::ReconfigureRuntime, Reason::SettingsChanged)
        }
    };
    Assessment {
        expected,
        actions: vec![Action {
            resource,
            effect,
            target: Some(Target::Identity(next)),
            reasons: vec![reason],
            dependencies: vec![],
            required_grants: vec![],
        }],
        blockers: vec![],
    }
}
