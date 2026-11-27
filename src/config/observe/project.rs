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
use crate::store::{
    generation_build::CatalogHead, registry_inspect, registry_inspect::ObservedProjectIndex,
    settings::DbSettings,
};
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
    #[serde(skip)]
    pub(crate) replacement: Option<ProjectReplacement>,
}

pub(crate) struct ProjectReplacement {
    pub(crate) global: Resolved<Config>,
    pub(crate) config: Config,
    pub(crate) reviewed: ReviewedReplacement,
}

pub(crate) struct ReviewedReplacement {
    pub(crate) base: ObservedProjectIndex,
    pub(crate) head: CatalogHead,
    sources: [source::SourceObservation; 2],
}

impl ReviewedReplacement {
    /// Check the exact files reviewed by the CLI, including absence.
    pub(crate) fn verify_sources(&self) -> Result<()> {
        let mut bytes_left = PlanningLimits::standard().max_source_bytes;
        for expected in &self.sources {
            let current = source::capture(expected.source.clone(), &mut bytes_left);
            if current.observation != *expected {
                return Err(change::invalid(
                    "configuration sources changed; run config plan again",
                ));
            }
        }
        Ok(())
    }
}

impl ProjectReplacement {
    fn supports(&self, action: &Action) -> bool {
        let ResourceId::Artifact(artifact) = &action.resource else {
            return false;
        };
        if action.effect != Effect::Rebuild
            || artifact.project_id != Some(self.reviewed.base.project_id)
        {
            return false;
        }
        match artifact.component.as_str() {
            "project_index" => true,
            "document_vectors" => self
                .config
                .hybrid_cfg
                .embedding
                .as_ref()
                .is_some_and(|stage| {
                    crate::hybrid::provider::provider(&stage.provider)
                        .is_ok_and(crate::hybrid::provider::Provider::enabled)
                }),
            _ => false,
        }
    }
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
        assess_source(&captured.observation, &mut assessment);
    }

    let mut replacement = None;
    let (full_survey, observation_failure) = if assessment.blockers.is_empty() {
        match crate::config::resolve::resolve_observed_v2(
            global.layer.as_ref(),
            project.layer.as_ref(),
        ) {
            Ok(Some((global_config, config))) if permits_project(&config, root) => {
                match registry_inspect::inspect_project_recipes(
                    cache,
                    root,
                    settings,
                    limits.registry,
                )? {
                    Ok(inspection) => {
                        let survey = assess_project(&inspection, &config, &mut assessment)?;
                        if !assessment.actions.is_empty()
                            && let Some(selected) = inspection.selected
                        {
                            let head =
                                CatalogHead::from_observed_seal(selected.index, selected.seal)?;
                            replacement = Some(ProjectReplacement {
                                global: global_config,
                                config,
                                reviewed: ReviewedReplacement {
                                    base: selected.index,
                                    head,
                                    sources: [global.observation, project.observation],
                                },
                            });
                        }
                        (survey, None)
                    }
                    Err(reason) => {
                        assess_unavailable_project(reason, &mut assessment);
                        (None, Some(reason))
                    }
                }
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
    let plan = change::compile_for(
        &[assessment],
        PlanLimits {
            max_actions: 2,
            max_dependencies: 1,
            max_metadata_bytes: limits.max_output_bytes,
        },
        |action| {
            replacement
                .as_ref()
                .is_some_and(|replacement| replacement.supports(action))
        },
    )?;
    Ok(ProjectConfigPlan {
        plan,
        full_survey,
        observation_failure,
        replacement,
    })
}

fn assess_source(source: &source::SourceObservation, assessment: &mut Assessment) {
    let resource = ResourceId::Source(source.source.clone());
    let state = match &source.state {
        source::SourceState::Missing => ExpectedState::Missing,
        source::SourceState::Loaded { precondition, .. } => {
            ExpectedState::KnownIdentity(precondition.clone())
        }
        source::SourceState::Unavailable(_) => {
            block(assessment, &resource, BlockerCode::ObservationUnavailable);
            ExpectedState::Unknown
        }
    };
    assessment.expected.push(Observation { resource, state });
}

fn assess_unavailable_project(
    reason: registry_inspect::UnavailableReason,
    assessment: &mut Assessment,
) {
    let resource = artifact("project_index", None);
    assessment.expected.push(Observation {
        resource: resource.clone(),
        state: if reason == registry_inspect::UnavailableReason::RecoveryRequired {
            ExpectedState::NeedsRecovery
        } else {
            ExpectedState::Unknown
        },
    });
    block(assessment, &resource, BlockerCode::ObservationUnavailable);
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
    inspection: &registry_inspect::ProjectRecipeInspection,
    config: &Config,
    assessment: &mut Assessment,
) -> Result<Option<bool>> {
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
    if let Some(selected) = &inspection.selected {
        return assess_selected(selected, config, assessment);
    }
    assessment.expected.push(Observation {
        resource: artifact("project_index", inspection.project_id),
        state: ExpectedState::Missing,
    });
    Ok(None)
}

fn assess_selected(
    selected: &registry_inspect::SelectedIndexEvidence,
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
    let Some(recipes) = &selected.recipes else {
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
