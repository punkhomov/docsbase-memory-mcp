//! Read-only assessment of the configuration effects on a selected project.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use crate::config::Config;
use crate::config::change::{
    self, Action, Assessment, Blocker, BlockerCode, ChangePlan, Effect, ExpectedState, Grant,
    Observation, PlanLimits, Reason, ResourceId, Target,
};
use crate::config::schema::{ArtifactId, Resolved, SourceId};
use crate::daemon::settings::DaemonProfile;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    basis: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restart_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ann: Option<AnnObservation>,
    #[serde(skip)]
    pub(crate) replacement: Option<ProjectReplacement>,
    #[serde(skip)]
    pub(crate) live_change: Option<LiveProjectChange>,
}

impl ProjectConfigPlan {
    pub(crate) fn live_check_code(&self) -> u8 {
        if self
            .plan
            .body
            .blockers
            .iter()
            .any(|blocker| blocker.code != BlockerCode::ExecutorUnavailable)
        {
            5
        } else if self.restart_required == Some(true) || !self.plan.body.actions.is_empty() {
            3
        } else {
            0
        }
    }
}

pub(crate) struct ActiveProjectConfig<'a> {
    pub(crate) config: Option<Arc<Config>>,
    pub(crate) global: crate::daemon::launch::ConfigStamp,
    pub(crate) daemon: &'a Config,
    pub(crate) ann: AnnObservation,
}

pub(crate) struct LiveProjectChange {
    pub(crate) active: Arc<Config>,
    pub(crate) config: Arc<Config>,
    pub(crate) reviewed: ReviewedReplacement,
}

impl LiveProjectChange {
    pub(crate) fn replaces_index(&self, action: &Action) -> bool {
        self.rebuilds_artifact(action, "project_index")
    }

    pub(crate) fn rebuilds_ann(&self, action: &Action) -> bool {
        self.rebuilds_artifact(action, "ann_graph")
    }

    fn rebuilds_artifact(&self, action: &Action, component: &str) -> bool {
        action.effect == Effect::Rebuild
            && matches!(&action.resource, ResourceId::Artifact(artifact)
                if artifact.project_id == Some(self.reviewed.base.project_id)
                    && artifact.component == component)
    }

    fn supports(&self, action: &Action) -> bool {
        self.config.hybrid_cfg.providers_compiled()
            && (self.replaces_index(action)
                || self.rebuilds_artifact(action, "document_vectors")
                || self.rebuilds_ann(action)
                || (action.effect == Effect::ReconfigureRuntime
                    && matches!(&action.resource, ResourceId::Runtime(resource) if resource == "project_runtime")))
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum AnnObservation {
    Disabled,
    Unknown,
    Resident {
        vectors: usize,
        estimated_bytes: u64,
    },
    Persisted {
        vectors: usize,
        dump_bytes: u64,
    },
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
    project_plan_for_profile(config_dir, cache, root, settings, limits, None)
}

pub(crate) fn project_plan_for_profile(
    config_dir: &Path,
    cache: &Path,
    root: &Path,
    settings: &Resolved<DbSettings>,
    limits: PlanningLimits,
    profile: Option<&DaemonProfile>,
) -> Result<ProjectConfigPlan> {
    project_plan_using(config_dir, cache, root, limits, None, profile, || {
        registry_inspect::inspect_project_recipes(cache, root, settings, limits.registry)
    })
}

pub(crate) fn live_project_plan(
    config_dir: &Path,
    cache: &Path,
    root: &Path,
    limits: PlanningLimits,
    observed: registry_inspect::ProjectRecipeInspection,
    active: ActiveProjectConfig<'_>,
) -> Result<ProjectConfigPlan> {
    project_plan_using(config_dir, cache, root, limits, Some(active), None, || {
        Ok(Ok(observed))
    })
}

type Inspected = std::result::Result<
    registry_inspect::ProjectRecipeInspection,
    registry_inspect::UnavailableReason,
>;

fn project_plan_using(
    config_dir: &Path,
    cache: &Path,
    root: &Path,
    limits: PlanningLimits,
    active: Option<ActiveProjectConfig<'_>>,
    profile: Option<&DaemonProfile>,
    inspect: impl FnOnce() -> Result<Inspected>,
) -> Result<ProjectConfigPlan> {
    let (global, project, mut assessment) =
        capture_plan_sources(config_dir, cache, root, limits, profile)?;

    let mut replacement = None;
    let mut live_change = None;
    let restart_required = active
        .as_ref()
        .map(|active| global_changed(&active.global, &global.observation.state));
    let (full_survey, observation_failure) = if assessment.blockers.is_empty() {
        match crate::config::resolve::resolve_observed_v2(
            global.layer.as_ref(),
            project.layer.as_ref(),
            profile,
            active
                .as_ref()
                .filter(|_| restart_required == Some(false))
                .map(|active| active.daemon),
        ) {
            Ok(Some((global_config, config))) if permits_project(&config, root) => {
                match inspect()? {
                    Ok(inspection) => {
                        let survey = assess_project(&inspection, &config, &mut assessment)?;
                        if let Some(active) = &active {
                            assess_runtime(
                                active,
                                &global.observation.state,
                                &config,
                                &inspection,
                                &mut assessment,
                            )?;
                            if restart_required == Some(false)
                                && let Some(old) = &active.config
                                && let Some(selected) = inspection.selected.as_ref()
                                && selected.seal.is_some()
                                && !assessment.actions.is_empty()
                            {
                                live_change = Some(LiveProjectChange {
                                    active: Arc::clone(old),
                                    config: Arc::new(config),
                                    reviewed: reviewed_replacement(
                                        selected,
                                        [global.observation, project.observation],
                                    )?,
                                });
                            }
                        } else if !assessment.actions.is_empty()
                            && let Some(selected) = inspection.selected.as_ref()
                        {
                            replacement = Some(replacement_for(
                                global_config,
                                config,
                                selected,
                                [global.observation, project.observation],
                            )?);
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
    let plan = compile_project_plan(
        assessment,
        limits,
        replacement.as_ref(),
        live_change.as_ref(),
    )?;
    Ok(ProjectConfigPlan {
        plan,
        full_survey,
        observation_failure,
        basis: active.as_ref().map(|_| "live"),
        restart_required,
        ann: active.map(|active| active.ann),
        replacement,
        live_change,
    })
}

fn capture_plan_sources(
    config_dir: &Path,
    cache: &Path,
    root: &Path,
    limits: PlanningLimits,
    profile: Option<&DaemonProfile>,
) -> Result<(source::Captured, source::Captured, Assessment)> {
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
            &(cache, root, profile),
        )?),
    });
    for captured in [&global, &project] {
        assess_source(&captured.observation, &mut assessment);
    }
    Ok((global, project, assessment))
}

fn compile_project_plan(
    assessment: Assessment,
    limits: PlanningLimits,
    replacement: Option<&ProjectReplacement>,
    live_change: Option<&LiveProjectChange>,
) -> Result<ChangePlan> {
    change::compile_for(
        &[assessment],
        PlanLimits {
            max_actions: 5,
            max_dependencies: 5,
            max_metadata_bytes: limits.max_output_bytes,
        },
        |action| {
            replacement.is_some_and(|replacement| replacement.supports(action))
                || live_change.is_some_and(|change| change.supports(action))
        },
    )
}

fn replacement_for(
    global: Resolved<Config>,
    config: Config,
    selected: &registry_inspect::SelectedIndexEvidence,
    sources: [source::SourceObservation; 2],
) -> Result<ProjectReplacement> {
    Ok(ProjectReplacement {
        global,
        config,
        reviewed: reviewed_replacement(selected, sources)?,
    })
}

fn reviewed_replacement(
    selected: &registry_inspect::SelectedIndexEvidence,
    sources: [source::SourceObservation; 2],
) -> Result<ReviewedReplacement> {
    Ok(ReviewedReplacement {
        base: selected.index,
        head: CatalogHead::from_observed_seal(selected.index, selected.seal)?,
        sources,
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

fn global_changed(
    stamp: &crate::daemon::launch::ConfigStamp,
    source: &source::SourceState,
) -> bool {
    use crate::daemon::launch::ConfigStamp;
    match (stamp, source) {
        (ConfigStamp::Missing, source::SourceState::Missing) => false,
        (ConfigStamp::Loaded(hash), source::SourceState::Loaded { revision, .. }) => {
            *revision != format!("source-bytes-v1:{}", hash.to_hex())
        }
        _ => true,
    }
}

fn assess_daemon_global(
    stamp: &crate::daemon::launch::ConfigStamp,
    source: &source::SourceState,
    index_dependency: Option<ResourceId>,
    assessment: &mut Assessment,
) -> Result<()> {
    use crate::config::schema::ComponentImpact;
    use crate::daemon::launch::ConfigStamp;
    let previous = match stamp {
        ConfigStamp::Missing => Some("missing".to_owned()),
        ConfigStamp::Loaded(hash) => Some(format!("source-bytes-v1:{}", hash.to_hex())),
        ConfigStamp::Unavailable(_) => None,
    };
    let desired = match source {
        source::SourceState::Missing => Some("missing"),
        source::SourceState::Loaded { revision, .. } => Some(revision.as_str()),
        source::SourceState::Unavailable(_) => None,
    };
    let Some((previous, desired)) = previous.as_deref().zip(desired) else {
        let resource = ResourceId::Runtime("daemon_global".into());
        assessment.expected.push(Observation {
            resource: resource.clone(),
            state: ExpectedState::Unknown,
        });
        block(assessment, &resource, BlockerCode::ObservationUnavailable);
        return Ok(());
    };
    let previous = change::policy_identity("daemon-source-revision-v1", &previous)?;
    let desired = change::policy_identity("daemon-source-revision-v1", &desired)?;
    let impact = ComponentImpact::between(Some(&previous), &desired);
    let mut daemon = change::runtime_assessment("daemon_global", Some(previous), desired, impact);
    if let Some(action) = daemon.actions.first_mut() {
        action.dependencies.extend(index_dependency);
    }
    assessment.expected.extend(daemon.expected);
    assessment.actions.extend(daemon.actions);
    Ok(())
}

fn assess_runtime(
    active: &ActiveProjectConfig,
    global: &source::SourceState,
    config: &Config,
    inspection: &registry_inspect::ProjectRecipeInspection,
    assessment: &mut Assessment,
) -> Result<()> {
    use crate::config::schema::ComponentImpact;
    let index = artifact("project_index", inspection.project_id);
    let index_dependency = assessment
        .actions
        .iter()
        .any(|action| action.resource == index)
        .then_some(index);
    assess_daemon_global(&active.global, global, index_dependency.clone(), assessment)?;
    let old = active.config.as_deref();
    if let Some(old) = old {
        let previous = crate::config::view::runtime_identity(old)?;
        let desired = crate::config::view::runtime_identity(config)?;
        let impact = ComponentImpact::between(Some(&previous), &desired);
        let mut runtime =
            change::runtime_assessment("project_runtime", Some(previous), desired, impact);
        if let Some(action) = runtime.actions.first_mut() {
            let daemon = ResourceId::Runtime("daemon_global".into());
            action.dependencies.extend(index_dependency);
            if assessment
                .actions
                .iter()
                .any(|required| required.resource == daemon)
            {
                action.dependencies.push(daemon);
            }
        }
        assessment.expected.extend(runtime.expected);
        assessment.actions.extend(runtime.actions);
    } else {
        assessment.expected.push(Observation {
            resource: ResourceId::Runtime("project_runtime".into()),
            state: ExpectedState::Missing,
        });
    }
    assess_ann(active, config, inspection, assessment)
}

fn assess_ann(
    active: &ActiveProjectConfig,
    config: &Config,
    inspection: &registry_inspect::ProjectRecipeInspection,
    assessment: &mut Assessment,
) -> Result<()> {
    let resource = artifact("ann_graph", inspection.project_id);
    let ann_format = (
        crate::hybrid::ann_store::ANN_FORMAT_VERSION,
        crate::hybrid::ann_store::ANN_RECIPE_VERSION,
        crate::hybrid::ann_store::HNSW_RS_VERSION,
    );
    let confirmed = matches!(
        active.ann,
        AnnObservation::Resident { .. } | AnnObservation::Persisted { .. }
    );
    let previous = active
        .config
        .as_deref()
        .zip(inspection.selected.as_ref())
        .filter(|_| confirmed);
    let state = match previous {
        Some((old, selected)) => ExpectedState::KnownIdentity(change::policy_identity(
            "selected-ann-v1",
            &(
                ann_format,
                selected.index.generation_id,
                selected.index.epoch,
                selected.index.content_revision,
                selected.seal,
                selected
                    .recipes
                    .as_ref()
                    .and_then(|recipes| recipes.embedding),
                old.ann_build.m(),
                old.ann_build.ef_construction(),
            ),
        )?),
        None => ExpectedState::Unknown,
    };
    assessment.expected.push(Observation {
        resource: resource.clone(),
        state,
    });
    if let Some((old, selected)) = previous
        && config.hybrid_cfg.enabled
        && config.hybrid_cfg.ranking == crate::hybrid::Ranking::Rrf
    {
        let index = artifact("project_index", inspection.project_id);
        let rebuild_index = assessment
            .actions
            .iter()
            .find(|action| action.resource == index);
        if rebuild_index.is_some() || old.ann_build != config.ann_build {
            let generation = rebuild_index.is_none().then_some((
                selected.index.epoch,
                selected.index.generation_id,
                selected.index.content_revision,
                selected.seal,
            ));
            let embedding = config
                .hybrid_cfg
                .embedding_model_key()
                .as_deref()
                .map(EmbeddingRecipe::for_model);
            assessment.actions.push(Action {
                resource,
                effect: Effect::Rebuild,
                target: Some(Target::Identity(change::policy_identity(
                    "selected-ann-target-v1",
                    &(
                        ann_format,
                        generation,
                        rebuild_index.and_then(|action| action.target.as_ref()),
                        embedding,
                        config.ann_build.m(),
                        config.ann_build.ef_construction(),
                    ),
                )?)),
                reasons: vec![Reason::SettingsChanged],
                dependencies: if rebuild_index.is_some() {
                    vec![index]
                } else {
                    Vec::new()
                },
                required_grants: Vec::new(),
            });
        }
    }
    Ok(())
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
    use crate::platform::paths::normalize_for_compare;
    match &config.daemon.profile {
        DaemonProfile::Shared => true,
        DaemonProfile::Website { project_root } => project_root
            .canonicalize()
            .is_ok_and(|fixed| normalize_for_compare(&fixed) == normalize_for_compare(root)),
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
