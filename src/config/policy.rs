//! Pure consent policy. A satisfied decision is never execution readiness.
//!
//! Callers own terminal I/O, current state observation, capability/blocker checks
//! and apply. This module cannot activate v2 or clear any planner blocker.

use super::bootstrap::{Entrypoint, EntrypointFacts};
use super::change::{
    self, Assessment, ChangePlan, Effect, ExpectedState, Grant, PlanLimits, ResourceId,
};
use crate::error::Result;

/// Whether an unadorned entrypoint has opted into the new policy contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// Preserve v1 startup when no new policy was requested.
    LegacyV1,
    /// Resolve Auto when no explicit policy was requested.
    Managed,
}
/// Policy requested at the controlling boundary, before reading any consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationPolicy {
    /// Prompt only for a direct CLI with a controlling terminal; otherwise fail.
    Auto,
    /// Ask a controlling CLI before consent-requiring changes.
    Prompt,
    /// Accept local changes, subject to separate permission grants.
    Apply,
    /// Refuse changes that require consent.
    Fail,
}
/// Runtime-facing policy; Auto has already been resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Controlling CLI must collect an explicit answer.
    Prompt,
    /// Explicit apply, still subject to grants and execution readiness.
    Apply,
    /// Required changes must be reported without consent I/O.
    Fail,
}
/// Captured launch policy. Facts cannot be changed after resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPolicy {
    policy: Policy,
    controlling_cli: bool,
    is_tty: bool,
}
impl ResolvedPolicy {
    /// Resolved policy, independent of later ambient TTY/environment changes.
    #[must_use]
    pub const fn policy(self) -> Policy {
        self.policy
    }
}
/// Distinguishes legacy startup from explicit consent handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyContext {
    /// Existing startup must retain its existing behavior and exit codes.
    Legacy,
    /// New contract resolved at the controlling boundary.
    Managed(ResolvedPolicy),
}
/// Resolve policy using supplied facts only; no terminal or environment reads.
#[must_use]
pub fn resolve(
    mode: PolicyMode,
    requested: Option<InvalidationPolicy>,
    facts: EntrypointFacts,
) -> PolicyContext {
    if mode == PolicyMode::LegacyV1 && requested.is_none() {
        return PolicyContext::Legacy;
    }
    let controlling_cli = facts.mode == Entrypoint::Cli;
    let policy = match requested.unwrap_or(InvalidationPolicy::Auto) {
        InvalidationPolicy::Auto if controlling_cli && facts.is_tty => Policy::Prompt,
        InvalidationPolicy::Auto | InvalidationPolicy::Fail => Policy::Fail,
        InvalidationPolicy::Prompt => Policy::Prompt,
        InvalidationPolicy::Apply => Policy::Apply,
    };
    PolicyContext::Managed(ResolvedPolicy {
        policy,
        controlling_cli,
        is_tty: facts.is_tty,
    })
}

/// Three independent, explicitly supplied permission classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantSet {
    remote: bool,
    downloads: bool,
    destructive: bool,
}
impl GrantSet {
    /// No expense, downloads or irreversible changes have been approved.
    #[must_use]
    pub const fn empty() -> Self {
        Self::new(false, false, false)
    }
    /// Explicit permissions in Remote, Downloads, Destructive order.
    #[must_use]
    pub const fn new(remote: bool, downloads: bool, destructive: bool) -> Self {
        Self {
            remote,
            downloads,
            destructive,
        }
    }
    fn includes(self, required: Self) -> bool {
        (!required.remote || self.remote)
            && (!required.downloads || self.downloads)
            && (!required.destructive || self.destructive)
    }
    fn union(self, other: Self) -> Self {
        Self::new(
            self.remote || other.remote,
            self.downloads || other.downloads,
            self.destructive || other.destructive,
        )
    }
    fn required(grants: &[Grant]) -> Self {
        let mut required = Self::empty();
        for grant in grants {
            match grant {
                Grant::Remote => required.remote = true,
                Grant::Downloads => required.downloads = true,
                Grant::Destructive => required.destructive = true,
            }
        }
        required
    }
}

/// An immutable, bounded canonical review, compiled from actual owner assessments.
#[derive(Debug)]
pub struct ConsentReview {
    plan: ChangePlan,
}
impl ConsentReview {
    /// Compile a new review; mutable or unvalidated `ChangePlan`s are not inputs.
    ///
    /// # Errors
    /// The existing compiler rejects inconsistent, cyclic or over-limit input.
    pub fn compile(assessments: &[Assessment], limits: PlanLimits) -> Result<Self> {
        Ok(Self {
            plan: change::compile(assessments, limits)?,
        })
    }
    /// Original plan, including every execution and observation blocker.
    #[must_use]
    pub const fn plan(&self) -> &ChangePlan {
        &self.plan
    }
    /// Determine consent only, without prompting, applying or claiming readiness.
    /// A future executor must separately enforce all blockers/capabilities and
    /// reobserve state under its own admission before using a receipt.
    #[must_use]
    pub fn evaluate(&self, policy: ResolvedPolicy, supplied: GrantSet) -> ConsentDecision {
        if !self.requires_consent() {
            return ConsentDecision::ConsentSatisfied(self.receipt());
        }
        let refuse = ConsentDecision::Refused;
        if policy.policy == Policy::Fail {
            return refuse(ConsentRefusal::RequiredChange);
        }
        if !policy.controlling_cli {
            return refuse(ConsentRefusal::NoControllingCli);
        }
        let required = GrantSet::required(&self.plan.body.required_grants);
        match policy.policy {
            Policy::Prompt if !policy.is_tty => refuse(ConsentRefusal::NoTerminal),
            Policy::Prompt => ConsentDecision::PromptRequired(PromptRequest {
                plan_id: self.plan.plan_id.clone(),
                required,
                supplied,
            }),
            Policy::Apply if supplied.includes(required) => {
                ConsentDecision::ConsentSatisfied(self.receipt())
            }
            Policy::Apply => refuse(ConsentRefusal::MissingGrants),
            Policy::Fail => refuse(ConsentRefusal::RequiredChange),
        }
    }
    fn receipt(&self) -> ConsentReceipt {
        ConsentReceipt {
            plan_id: self.plan.plan_id.clone(),
        }
    }
    fn requires_consent(&self) -> bool {
        !self.plan.body.required_grants.is_empty()
            || self.plan.body.actions.iter().any(|action| {
                let state = self
                    .plan
                    .body
                    .expected
                    // The compiler sorts observations by resource. Avoid a full
                    // scan per action and allocate no extra lookup structure.
                    .binary_search_by(|fact| fact.resource.cmp(&action.resource))
                    .ok()
                    .and_then(|index| self.plan.body.expected.get(index))
                    .map(|fact| &fact.state);
                !matches!(
                    (action.effect, &action.resource, state),
                    (
                        Effect::InitializeRuntime,
                        ResourceId::Runtime(_),
                        Some(ExpectedState::Missing)
                    ) | (
                        Effect::ReconfigureRuntime,
                        ResourceId::Runtime(_),
                        Some(ExpectedState::KnownIdentity(_))
                    ) | (
                        Effect::InitializeStore,
                        ResourceId::Artifact(_),
                        Some(ExpectedState::Missing)
                    )
                )
            })
    }
}
/// Consent status only; no variant promises successful execution or exit zero.
#[derive(Debug)]
pub enum ConsentDecision {
    /// No further consent is needed for this exact plan.
    ConsentSatisfied(ConsentReceipt),
    /// A controlling CLI must display the review and collect an answer.
    PromptRequired(PromptRequest),
    /// Consent refused before any side effects.
    Refused(ConsentRefusal),
}
impl ConsentDecision {
    /// Refusal reason, if consent was refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<ConsentRefusal> {
        match self {
            Self::Refused(reason) => Some(*reason),
            Self::ConsentSatisfied(_) | Self::PromptRequired(_) => None,
        }
    }
}
/// Opaque interactive request, available only for a direct CLI with a TTY.
#[derive(Debug)]
pub struct PromptRequest {
    plan_id: String,
    required: GrantSet,
    supplied: GrantSet,
}
impl PromptRequest {
    /// The review identity the controlling CLI must display and bind its answer to.
    #[must_use]
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }
    /// Record an explicit answer and independently supplied additional grants.
    /// This performs no I/O and cannot clear execution blockers.
    ///
    /// # Errors
    /// Wrong displayed identity, declined review or missing permission classes.
    pub fn answer(
        &self,
        reviewed_plan_id: &str,
        accepted: bool,
        additional: GrantSet,
    ) -> std::result::Result<ConsentReceipt, ConsentRefusal> {
        if reviewed_plan_id != self.plan_id {
            return Err(ConsentRefusal::StalePlan);
        }
        if !accepted {
            return Err(ConsentRefusal::Declined);
        }
        if !self.supplied.union(additional).includes(self.required) {
            return Err(ConsentRefusal::MissingGrants);
        }
        Ok(ConsentReceipt {
            plan_id: self.plan_id.clone(),
        })
    }
}
/// Records consent for one exact canonical plan; not an execution/auth token.
#[derive(Debug)]
pub struct ConsentReceipt {
    plan_id: String,
}
impl ConsentReceipt {
    /// Compare with a newly compiled review from freshly observed owner facts.
    /// The caller owns actual state observation/admission; no filesystem freshness
    /// or execution readiness is inferred here, even on success.
    ///
    /// # Errors
    /// Any source, observation, action, target, grant or blocker changed plan ID.
    pub fn revalidate(&self, current: &ConsentReview) -> std::result::Result<(), ConsentRefusal> {
        if self.plan_id != current.plan.plan_id {
            return Err(ConsentRefusal::StalePlan);
        }
        Ok(())
    }
}
/// Finite refusal reasons for the new policy path, with no raw source values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentRefusal {
    /// Fail policy encountered a consent-requiring action.
    RequiredChange,
    /// A service/MCP/autostart/standalone loop cannot consent to rebuilding.
    NoControllingCli,
    /// Explicit prompt has no controlling terminal.
    NoTerminal,
    /// One or more required permission classes were not supplied.
    MissingGrants,
    /// The operator declined the exact review.
    Declined,
    /// Reviewed identity no longer matches the intended/current plan.
    StalePlan,
}
impl ConsentRefusal {
    /// New policy-path refusal codes only; legacy commands are unchanged.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::RequiredChange => 3,
            Self::StalePlan => 4,
            Self::NoControllingCli | Self::NoTerminal | Self::MissingGrants | Self::Declined => 5,
        }
    }
}
