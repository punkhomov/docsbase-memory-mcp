//! Bounded canonicalization and deterministic DAG ordering; no component rules.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use super::{
    Action, Assessment, Blocker, BlockerCode, ChangePlan, ExpectedState, Observation, PlanBody,
    PlanLimits, ResourceId, invalid,
};
use crate::config::schema::SourceId;
use crate::error::Result;

/// Compile owner assessments without I/O, runtime construction, or consent.
///
/// # Errors
/// Exceeded caller limits, inconsistent observations/actions, missing inputs or
/// dependency cycles. The borrowed input remains unchanged on every path.
pub fn compile(assessments: &[Assessment], limits: PlanLimits) -> Result<ChangePlan> {
    admit_input(assessments, limits)?;
    let mut expected = BTreeMap::new();
    let mut actions: BTreeMap<ResourceId, Action> = BTreeMap::new();
    let mut blockers = BTreeSet::new();
    for assessment in assessments {
        for fact in &assessment.expected {
            if let Some(previous) = expected.insert(fact.resource.clone(), fact.state.clone())
                && previous != fact.state
            {
                return Err(invalid("conflicting plan observations"));
            }
        }
        blockers.extend(assessment.blockers.iter().cloned());
        for raw in &assessment.actions {
            let mut action = raw.clone();
            canonicalize(&mut action.required_grants);
            if let Some(previous) = actions.get_mut(&action.resource) {
                if previous.effect != action.effect
                    || previous.target != action.target
                    || previous.required_grants != action.required_grants
                {
                    return Err(invalid("conflicting actions for one resource"));
                }
                previous.reasons.extend(action.reasons);
                previous.dependencies.extend(action.dependencies);
            } else {
                actions.insert(action.resource.clone(), action);
            }
        }
    }
    for action in actions.values_mut() {
        canonicalize(&mut action.reasons);
        canonicalize(&mut action.dependencies);
    }
    for resource in actions.keys() {
        if !expected.contains_key(resource) {
            return Err(invalid("action lacks an explicit observation"));
        }
        blockers.insert(Blocker {
            resource: Some(resource.clone()),
            code: BlockerCode::ExecutorUnavailable,
        });
    }
    let blocked_resources: BTreeSet<_> = blockers
        .iter()
        .filter_map(|b| b.resource.as_ref())
        .collect();
    let unchanged_observations = expected
        .iter()
        .filter(|(id, state)| {
            matches!(state, ExpectedState::KnownIdentity(_))
                && !actions.contains_key(*id)
                && !blocked_resources.contains(id)
        })
        .map(|(id, _)| id.clone())
        .collect();
    let required_grants: BTreeSet<_> = actions
        .values()
        .flat_map(|a| a.required_grants.iter().copied())
        .collect();
    let body = PlanBody {
        plan_version: 1,
        expected: expected
            .into_iter()
            .map(|(resource, state)| Observation { resource, state })
            .collect(),
        actions: topological(actions)?,
        unchanged_observations,
        required_grants: required_grants.into_iter().collect(),
        blockers: blockers.into_iter().collect(),
    };
    let hash = digest("docsbase.change-plan.v1", &body, limits.max_metadata_bytes)?;
    let plan = ChangePlan {
        plan_id: format!("change-plan-v1:{hash}"),
        body,
    };
    digest("", &plan, limits.max_metadata_bytes)?;
    Ok(plan)
}
fn admit_input(assessments: &[Assessment], limits: PlanLimits) -> Result<()> {
    let mut actions_left = limits.max_actions;
    let mut edges_left = limits.max_dependencies;
    let mut raw_paths_left = limits.max_metadata_bytes;
    for assessment in assessments {
        // Canonical path encoding can collapse thousands of separators. Account
        // for raw borrowed path bytes too before cloning any resource identities.
        let resources = assessment
            .expected
            .iter()
            .map(|fact| &fact.resource)
            .chain(assessment.actions.iter().flat_map(|action| {
                std::iter::once(&action.resource).chain(action.dependencies.iter())
            }))
            .chain(
                assessment
                    .blockers
                    .iter()
                    .filter_map(|blocker| blocker.resource.as_ref()),
            );
        for resource in resources {
            if let ResourceId::Source(SourceId::Global(path) | SourceId::Project(path)) = resource {
                raw_paths_left = raw_paths_left
                    .checked_sub(path.as_os_str().as_encoded_bytes().len())
                    .ok_or_else(|| invalid("plan raw path metadata limit exceeded"))?;
            }
        }
        actions_left = actions_left
            .checked_sub(assessment.actions.len())
            .ok_or_else(|| invalid("plan action limit exceeded"))?;
        for action in &assessment.actions {
            edges_left = edges_left
                .checked_sub(action.dependencies.len())
                .ok_or_else(|| invalid("plan dependency limit exceeded"))?;
        }
    }
    // Streaming admission BEFORE cloning/indexing input, including repeated facts
    // and blockers. The caller already owns input allocations.
    digest("", &assessments, limits.max_metadata_bytes)?;
    Ok(())
}
fn canonicalize<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}
fn topological(mut actions: BTreeMap<ResourceId, Action>) -> Result<Vec<Action>> {
    let mut indegrees = BTreeMap::new();
    let mut dependents: BTreeMap<ResourceId, Vec<ResourceId>> = BTreeMap::new();
    let mut ready = BTreeSet::new();
    for (id, action) in &actions {
        indegrees.insert(id.clone(), action.dependencies.len());
        if action.dependencies.is_empty() {
            ready.insert(id.clone());
        }
        for dependency in &action.dependencies {
            if !actions.contains_key(dependency) {
                return Err(invalid("missing action dependency"));
            }
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(id.clone());
        }
    }
    let mut ordered = Vec::with_capacity(actions.len());
    while let Some(id) = ready.pop_first() {
        let action = actions
            .remove(&id)
            .ok_or_else(|| invalid("invalid ready action"))?;
        ordered.push(action);
        if let Some(children) = dependents.remove(&id) {
            for child in children {
                let remaining = indegrees
                    .get_mut(&child)
                    .ok_or_else(|| invalid("invalid action indegree"))?;
                *remaining = remaining
                    .checked_sub(1)
                    .ok_or_else(|| invalid("invalid action indegree"))?;
                if *remaining == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    if !actions.is_empty() {
        return Err(invalid("action dependency cycle"));
    }
    Ok(ordered)
}

struct DigestWriter {
    remaining: usize,
    hasher: blake3::Hasher,
}
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("metadata limit exceeded"))?;
        self.hasher.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn digest(domain: &str, value: &impl serde::Serialize, limit: usize) -> Result<String> {
    let mut writer = DigestWriter {
        remaining: limit,
        hasher: blake3::Hasher::new_derive_key(domain),
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| invalid("plan metadata limit or encoding error"))?;
    Ok(writer.hasher.finalize().to_hex().to_string())
}
