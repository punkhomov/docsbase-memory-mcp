//! Read-only observed migration plans. No config/runtime/store mutation occurs.
use std::path::Path;

use super::change::{
    self, Assessment, Blocker, BlockerCode, ExpectedState, Observation, PlanLimits, ResourceId,
};
use super::migrate::{self, ProjectObservation, VectorBudgetDecision};
use super::schema::{ArtifactId, Resolved, SourceId};
use super::source::{SourceLayer, invalid};
use crate::error::Result;
use crate::store::registry_inspect::{self, InspectionLimits, RegistryObservation, RegistryState};
use crate::store::settings::DbSettings;

mod source;
mod view;
use source::{Captured, SourceState};
pub use view::MigrationPlan;

/// Explicit policy owned by migration planning, not by search/runtime consumers.
#[derive(Debug, Clone, Copy)]
pub struct PlanningLimits {
    /// Complete registry observation limits.
    pub registry: InspectionLimits,
    /// Total retained original source bytes across all files.
    pub max_source_bytes: usize,
    /// Complete encoded envelope and intermediate action graph limit.
    pub max_output_bytes: usize,
}
impl PlanningLimits {
    /// Official CLI planning policy. Callers may supply another explicit policy.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            registry: InspectionLimits {
                max_projects: 256,
                max_metadata_bytes: 1024 * 1024,
                max_sqlite_value_bytes: 1024 * 1024,
                sqlite_cache_kib: 256,
            },
            max_source_bytes: 16 * 1024 * 1024,
            max_output_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Survey current source files for the complete registered project inventory.
/// Reads independently; any later apply must revalidate all source/context facts.
///
/// # Errors
/// Invalid caller context/limits, invalid semantic configuration, excessive plan
/// output or invalid budget choice. File/registry unavailability yields a blocked
/// plan, never an incomplete rewrite proposal.
pub fn migration_plan(
    config_root: &Path,
    cache_root: &Path,
    db: &Resolved<DbSettings>,
    limits: PlanningLimits,
    decision: Option<VectorBudgetDecision>,
) -> Result<MigrationPlan> {
    plan_with(config_root, cache_root, db, limits, decision, || {})
}
fn plan_with(
    config_root: &Path,
    cache_root: &Path,
    db: &Resolved<DbSettings>,
    limits: PlanningLimits,
    decision: Option<VectorBudgetDecision>,
    after_sources: impl FnOnce(),
) -> Result<MigrationPlan> {
    let mut metadata_left = limits.registry.max_metadata_bytes;
    admit_path(config_root, &mut metadata_left)?;
    admit_path(cache_root, &mut metadata_left)?;
    if limits.max_output_bytes == 0 || limits.registry.max_projects.checked_add(2).is_none() {
        return Err(invalid("invalid migration planning limits"));
    }
    if let Some(VectorBudgetDecision::UseBytes(n)) = decision {
        if n == 0 || i64::try_from(n).is_err() {
            return Err(invalid("vector budget must be a positive TOML integer"));
        }
    }
    let registry = registry_inspect::inspect_registry(cache_root, db, limits.registry)?;
    let mut ids = vec![SourceId::Global(config_root.join("config.toml"))];
    if let RegistryState::Ready { projects, .. } = &registry.state {
        for project in projects {
            ids.push(SourceId::Project(project.root.join(".docsbase.toml")));
        }
    }
    // Bound paths before cloning into observations, drafts and their projections.
    for id in &ids {
        if let SourceId::Global(path) | SourceId::Project(path) = id {
            admit_path(path, &mut metadata_left)?;
        }
    }
    let mut bytes_left = limits.max_source_bytes;
    let captured: Vec<_> = ids
        .into_iter()
        .map(|id| source::capture(id, &mut bytes_left))
        .collect();
    after_sources();
    let final_registry = registry_inspect::inspect_registry(cache_root, db, limits.registry)?;
    let stable =
        registry == final_registry && !matches!(registry.state, RegistryState::Unavailable(_));
    let mut assessment = observations(&registry, &captured, stable);
    let summary = if stable
        && captured
            .iter()
            .all(|c| !matches!(c.observation.state, SourceState::Unavailable(_)))
    {
        Some(propose(&captured, decision, limits, &mut assessment)?)
    } else {
        None
    };
    let plan = change::compile(
        &[assessment],
        PlanLimits {
            max_actions: limits.registry.max_projects + 2,
            max_dependencies: limits.registry.max_projects + 1,
            max_metadata_bytes: limits.max_output_bytes,
        },
    )?;
    MigrationPlan {
        observation_version: 1,
        observation_id: String::new(),
        config_root: config_root.to_owned(),
        registry,
        registry_stable: stable,
        sources: captured.into_iter().map(|c| c.observation).collect(),
        decision: decision.into(),
        summary,
        plan,
        atomic_snapshot: false,
        runtime_activation_available: false,
    }
    .identify(limits.max_output_bytes)
}
fn admit_path(path: &Path, bytes_left: &mut usize) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("migration paths must be UTF-8"))?;
    if !path.is_absolute()
        || text.as_bytes().contains(&0)
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid(
            "migration paths must be absolute without parent traversal",
        ));
    }
    *bytes_left = bytes_left
        .checked_sub(text.len())
        .ok_or_else(|| invalid("migration path metadata limit exceeded"))?;
    Ok(())
}
fn source_state(state: &SourceState) -> ExpectedState {
    match state {
        SourceState::Missing => ExpectedState::Missing,
        SourceState::Loaded { revision, .. } => ExpectedState::KnownIdentity(revision.clone()),
        SourceState::Unavailable(_) => ExpectedState::Unknown,
    }
}
fn observations(registry: &RegistryObservation, sources: &[Captured], stable: bool) -> Assessment {
    let resource = ResourceId::Artifact(ArtifactId {
        component: "project_registry".into(),
        project_id: None,
    });
    let state = if stable {
        match &registry.state {
            RegistryState::Missing => ExpectedState::Missing,
            RegistryState::Ready { revision, .. } => ExpectedState::KnownIdentity(revision.clone()),
            RegistryState::Unavailable(_) => ExpectedState::Unknown,
        }
    } else {
        ExpectedState::Unknown
    };
    let mut result = Assessment::default();
    if !stable {
        result.blockers.push(Blocker {
            resource: Some(resource.clone()),
            code: BlockerCode::ObservationUnavailable,
        });
    }
    result.expected.push(Observation { resource, state });
    for captured in sources {
        let resource = ResourceId::Source(captured.observation.source.clone());
        if matches!(captured.observation.state, SourceState::Unavailable(_)) {
            result.blockers.push(Blocker {
                resource: Some(resource.clone()),
                code: BlockerCode::ObservationUnavailable,
            });
        }
        result.expected.push(Observation {
            resource,
            state: source_state(&captured.observation.state),
        });
    }
    result
}
fn propose(
    sources: &[Captured],
    decision: Option<VectorBudgetDecision>,
    limits: PlanningLimits,
    assessment: &mut Assessment,
) -> Result<view::Summary> {
    let first = sources
        .first()
        .ok_or_else(|| invalid("global observation missing"))?;
    let empty;
    let global = if let Some(layer) = &first.layer {
        layer
    } else {
        empty = SourceLayer::parse(first.observation.source.clone(), "")?;
        &empty
    };
    let mut registered = Vec::new();
    let mut projects = Vec::new();
    for captured in &sources[1..] {
        let SourceId::Project(path) = &captured.observation.source else {
            return Err(invalid("unexpected source in project survey"));
        };
        registered.push(path.clone());
        projects.push(match &captured.layer {
            Some(layer) => ProjectObservation::Loaded(Box::new(layer.clone())),
            None => ProjectObservation::Missing(path.clone()),
        });
    }
    let draft = migrate::draft_migration(global, &registered, &projects, decision)?;
    let mut output_left = limits.max_source_bytes;
    for edit in &draft.edits {
        output_left = output_left
            .checked_sub(edit.after.len())
            .ok_or_else(|| invalid("proposed source aggregate byte limit exceeded"))?;
        let observed = sources
            .iter()
            .find(|c| c.observation.source == edit.source)
            .ok_or_else(|| invalid("migration edit outside observed inventory"))?;
        let before = observed
            .layer
            .as_ref()
            .map_or("", |layer| layer.original.as_str());
        if before != edit.before
            || blake3::hash(before.as_bytes()).to_hex().as_str() != edit.expected_source_hash
        {
            return Err(invalid("migration source precondition mismatch"));
        }
    }
    let mut proposed = draft.assess_change()?;
    // Retain actual states for ALL input sources, including unchanged/missing ones.
    proposed
        .expected
        .retain(|o| !matches!(o.resource, ResourceId::Source(_)));
    proposed
        .blockers
        .retain(|b| b.code != BlockerCode::SourceObservationRequired);
    assessment.expected.extend(proposed.expected);
    assessment.blockers.extend(proposed.blockers);
    assessment.actions.extend(proposed.actions);
    Ok(view::Summary::from_draft(&draft))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_change_after_sources_blocks_every_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let cache = dir.path().join("cache");
        let db = crate::store::settings::standalone().unwrap();
        let plan = plan_with(
            &config,
            &cache,
            &db,
            PlanningLimits::standard(),
            Some(VectorBudgetDecision::AcceptSuggestedMaximum),
            || {
                std::fs::create_dir(&cache).unwrap();
                std::fs::write(
                    cache.join(format!("{}-wal", crate::store::DB_FILE)),
                    "changed",
                )
                .unwrap();
            },
        )
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&plan.to_json(100_000).unwrap()).unwrap();
        assert_eq!(json["registry_stable"], false);
        assert_eq!(json["plan"]["actions"], serde_json::json!([]));
    }
}
