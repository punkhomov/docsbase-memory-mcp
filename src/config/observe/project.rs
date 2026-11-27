//! Read-only assessment of the configuration effects on a selected project.

use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::config::change::{
    self, Action, Assessment, Blocker, BlockerCode, ChangePlan, Effect, ExpectedState, Grant,
    Observation, PlanLimits, Reason, ResourceId, Target,
};
use crate::config::schema::{ArtifactId, Resolved, SourceId};
use crate::error::Result;
use crate::store::{registry_inspect, settings::DbSettings};
use crate::vector_cache::EmbeddingRecipe;

use super::{PlanningLimits, source};

#[derive(Serialize)]
pub(crate) struct ProjectConfigPlan {
    #[serde(flatten)]
    pub(crate) plan: ChangePlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_survey: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation_failure: Option<registry_inspect::UnavailableReason>,
}

pub(crate) fn project_plan(
    config_dir: &Path,
    cache: &Path,
    root: &Path,
    settings: &Resolved<DbSettings>,
    limits: PlanningLimits,
) -> Result<ProjectConfigPlan> {
    let mut bytes_left = limits.max_source_bytes;
    let global = source::capture(
        SourceId::Global(config_dir.join("config.toml")),
        &mut bytes_left,
    );
    let project = source::capture(
        SourceId::Project(root.join(".docsbase.toml")),
        &mut bytes_left,
    );
    let mut assessment = Assessment::default();
    assessment.expected.push(Observation {
        resource: artifact("project_plan_context", None),
        state: ExpectedState::KnownIdentity(change::policy_identity(
            "project-plan-context-v1",
            &(cache, root),
        )?),
    });
    for captured in [&global, &project] {
        let resource = ResourceId::Source(captured.observation.source.clone());
        let state = match &captured.observation.state {
            source::SourceState::Missing => ExpectedState::Missing,
            source::SourceState::Loaded { precondition, .. } => {
                ExpectedState::KnownIdentity(precondition.clone())
            }
            source::SourceState::Unavailable(_) => {
                block(
                    &mut assessment,
                    &resource,
                    BlockerCode::ObservationUnavailable,
                );
                ExpectedState::Unknown
            }
        };
        assessment.expected.push(Observation { resource, state });
    }

    let (full_survey, observation_failure) = if assessment.blockers.is_empty() {
        match crate::config::resolve::resolve_observed_v2(
            global.layer.as_ref(),
            project.layer.as_ref(),
        ) {
            Ok(Some(config)) if permits_project(&config, root) => {
                assess_project(cache, root, settings, limits, &config, &mut assessment)?
            }
            Ok(None) => {
                block(
                    &mut assessment,
                    &ResourceId::Source(global.observation.source),
                    BlockerCode::UnsupportedIdentity,
                );
                (None, None)
            }
            Ok(Some(_)) | Err(_) => {
                assessment.blockers.push(Blocker {
                    resource: None,
                    code: BlockerCode::InvalidSettings,
                });
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let plan = change::compile(
        &[assessment],
        PlanLimits {
            max_actions: 2,
            max_dependencies: 1,
            max_metadata_bytes: limits.max_output_bytes,
        },
    )?;
    Ok(ProjectConfigPlan {
        plan,
        full_survey,
        observation_failure,
    })
}

fn permits_project(config: &Config, root: &Path) -> bool {
    use crate::daemon::settings::DaemonProfile;
    match &config.daemon.profile {
        DaemonProfile::Shared => true,
        DaemonProfile::Website { project_root } => {
            project_root.canonicalize().is_ok_and(|fixed| fixed == root)
        }
    }
}

fn assess_project(
    cache: &Path,
    root: &Path,
    settings: &Resolved<DbSettings>,
    limits: PlanningLimits,
    config: &Config,
    assessment: &mut Assessment,
) -> Result<(Option<bool>, Option<registry_inspect::UnavailableReason>)> {
    match registry_inspect::inspect_project_recipes(cache, root, settings, limits.registry)? {
        Ok(inspection) => {
            let registry = inspection
                .schema_version
                .map(|schema| {
                    change::policy_identity(
                        "project-registry-v1",
                        &(schema, inspection.epoch, inspection.project_id),
                    )
                })
                .transpose()?;
            assessment.expected.push(Observation {
                resource: artifact("project_registry", inspection.project_id),
                state: registry.map_or(ExpectedState::Missing, ExpectedState::KnownIdentity),
            });
            if let Some(selected) = inspection.selected {
                return Ok((assess_selected(selected, config, assessment)?, None));
            }
            assessment.expected.push(Observation {
                resource: artifact("project_index", inspection.project_id),
                state: ExpectedState::Missing,
            });
            Ok((None, None))
        }
        Err(reason) => {
            assessment.expected.push(Observation {
                resource: artifact("project_index", None),
                state: if reason == registry_inspect::UnavailableReason::RecoveryRequired {
                    ExpectedState::NeedsRecovery
                } else {
                    ExpectedState::Unknown
                },
            });
            block(
                assessment,
                &artifact("project_index", None),
                BlockerCode::ObservationUnavailable,
            );
            Ok((None, Some(reason)))
        }
    }
}

fn assess_selected(
    selected: registry_inspect::SelectedIndexEvidence,
    config: &Config,
    assessment: &mut Assessment,
) -> Result<Option<bool>> {
    let identity = selected.index;
    let project = Some(identity.project_id);
    let resource = artifact("project_index", project);
    let previous = change::policy_identity(
        "project-index-generation-v1",
        &(
            identity.epoch,
            identity.generation_id,
            identity.content_revision,
            selected.seal,
            selected.recipes.as_ref().map(|recipes| recipes.full_survey),
        ),
    )?;
    assessment.expected.push(Observation {
        resource: resource.clone(),
        state: ExpectedState::KnownIdentity(previous),
    });
    let Some(recipes) = selected.recipes else {
        block(assessment, &resource, BlockerCode::UnsupportedIdentity);
        return Ok(None);
    };
    let lexical = crate::index::outcome::recipe_id(config);
    let embedding = config
        .hybrid_cfg
        .embedding_model_key()
        .as_deref()
        .map(EmbeddingRecipe::for_model);
    if recipes.lexical != lexical || recipes.embedding != embedding {
        push_replacement(assessment, identity.project_id, &lexical, embedding, config)?;
    }
    Ok(Some(recipes.full_survey))
}

fn push_replacement(
    assessment: &mut Assessment,
    project_id: i64,
    lexical: &str,
    embedding: Option<EmbeddingRecipe>,
    config: &Config,
) -> Result<()> {
    let mut dependencies = Vec::new();
    if let Some(recipe) = embedding {
        let vectors = artifact("document_vectors", Some(project_id));
        // The seal identifies the model recipe. It does not count vectors
        // present in the separately owned, live cache.
        assessment.expected.push(Observation {
            resource: vectors.clone(),
            state: ExpectedState::Unknown,
        });
        let remote = config
            .hybrid_cfg
            .embedding
            .as_ref()
            .is_some_and(|stage| stage.provider == crate::hybrid::config::PROVIDER_OPENROUTER);
        assessment.actions.push(Action {
            resource: vectors.clone(),
            effect: Effect::Rebuild,
            target: Some(Target::Identity(change::policy_identity(
                "document-vector-inputs-v1",
                &(lexical, recipe),
            )?)),
            reasons: vec![Reason::SettingsChanged],
            dependencies: Vec::new(),
            required_grants: if remote {
                vec![Grant::Remote]
            } else {
                Vec::new()
            },
        });
        dependencies.push(vectors);
    }
    assessment.actions.push(Action {
        resource: artifact("project_index", Some(project_id)),
        effect: Effect::Rebuild,
        target: Some(Target::Identity(change::policy_identity(
            "project-index-replacement-v1",
            &(lexical, embedding),
        )?)),
        reasons: vec![Reason::SettingsChanged],
        dependencies,
        required_grants: Vec::new(),
    });
    Ok(())
}

fn artifact(component: &str, project_id: Option<i64>) -> ResourceId {
    ResourceId::Artifact(ArtifactId {
        component: component.into(),
        project_id,
    })
}

fn block(assessment: &mut Assessment, resource: &ResourceId, code: BlockerCode) {
    assessment.blockers.push(Blocker {
        resource: Some(resource.clone()),
        code,
    });
}
