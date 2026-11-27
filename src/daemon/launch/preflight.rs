//! Read-only startup observation of the registered v5 projects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{
    change::{ChangePlan, ExpectedState, ResourceId},
    observe::{self, PlanningLimits},
    schema::{Resolved, SourceId},
};
use crate::daemon::settings::DaemonProfile;
use crate::error::{Error, Result};
use crate::store::{
    GENERATION_SCHEMA_VERSION,
    registry_inspect::{self, RegistryState},
    settings,
};

use super::{LaunchConfig, lexical_only, restart_notice};

pub(crate) struct StartupPlans {
    pub(crate) config: PathBuf,
    pub(crate) replacements: Vec<(PathBuf, ChangePlan)>,
    pub(crate) expectations: StartupExpectations,
}

pub(crate) struct StartupExpectations {
    pub(crate) cache: PathBuf,
    fixed: Option<PathBuf>,
    registry: Option<RegistryState>,
    sources: BTreeMap<SourceId, ExpectedState>,
}

impl StartupExpectations {
    pub(crate) fn verify(&self, launch: &LaunchConfig) -> Result<bool> {
        if !self.verify_sources(launch) {
            return Ok(false);
        }
        let limits = PlanningLimits::standard();
        if let Some(expected) = &self.registry {
            let settings = settings::db_from_config(&launch.config)?;
            let current = observe_registry(&self.cache, self.fixed.as_deref(), &settings, limits)?;
            if current != *expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn verify_sources(&self, launch: &LaunchConfig) -> bool {
        restart_notice(launch.path.as_deref(), &launch.stamp).is_none()
            && fixed_root(
                &launch.config.settings().daemon.profile,
                launch.config.settings(),
            )
            .is_ok_and(|root| root == self.fixed)
            && observe::verify_source_expectations(&self.sources, PlanningLimits::standard())
    }

    pub(super) fn fixed_config(
        &self,
        launch: &LaunchConfig,
        root: &Path,
    ) -> Result<crate::config::Config> {
        let source = SourceId::Project(root.join(".docsbase.toml"));
        if let Some(expected) = self.sources.get(&source) {
            return observe::verified_project_config(&launch.config, source, expected)?
                .ok_or(Error::StartupPlanStale);
        }
        crate::config::Config::for_project(launch.config.settings(), root)
    }

    pub(super) fn fixed_project(
        &self,
        db: &crate::store::Db,
        root: &Path,
    ) -> Result<Option<crate::store::models::Project>> {
        if self.fixed.as_deref() != Some(root) {
            return Err(Error::StartupPlanStale);
        }
        match &self.registry {
            Some(RegistryState::Ready { projects, .. }) => {
                let Some(project) = projects.first() else {
                    return Ok(None);
                };
                let current = crate::daemon::registry::project_by_id(db, project.id)?;
                if current
                    .as_ref()
                    .is_none_or(|row| row.canonical_root != project.root)
                {
                    return Err(Error::StartupPlanStale);
                }
                Ok(current)
            }
            Some(RegistryState::Missing) => Ok(None),
            _ => super::website_project(db, root),
        }
    }

    pub(super) fn verify_opened(
        &self,
        launch: &LaunchConfig,
        db: &crate::store::Db,
    ) -> Result<bool> {
        if let Some(expected) = &self.registry {
            let current = registry_inspect::inspect_open_startup_registry(
                db.connection(),
                &self.cache,
                self.fixed.as_deref(),
                PlanningLimits::standard().registry,
            )?;
            if current != *expected
                && !(matches!(expected, RegistryState::Missing)
                    && matches!(current, RegistryState::Ready { projects, .. } if projects.is_empty()))
            {
                return Ok(false);
            }
        }
        Ok(self.verify_sources(launch))
    }
}

pub(crate) fn autostart(cache: &Path, launch: &LaunchConfig) -> Result<()> {
    if launch.config.settings().vector_store.is_none() {
        return Ok(());
    }
    recover_abandoned_registry(cache, launch)?;
    let plans = survey(cache, launch, false)?;
    if !plans.expectations.verify(launch)? {
        return Err(unavailable(
            "global configuration changed during startup planning; retry",
        ));
    }
    if let Some((root, _)) = plans.replacements.first() {
        return Err(unavailable(format!(
            "configuration changes require action for {}; review docsbase config plan and apply it before autostart",
            root.to_string_lossy().escape_debug()
        )));
    }
    Ok(())
}

pub(crate) fn recover_abandoned_registry(cache: &Path, launch: &LaunchConfig) -> Result<()> {
    if launch.config.settings().vector_store.is_some()
        && registry_inspect::startup_catalog_requires_recovery(cache)
    {
        let schema = crate::daemon::lifecycle::read_state(cache)?
            .filter(|state| state.schema_version == crate::store::migrations::SCHEMA_VERSION)
            .map_or(GENERATION_SCHEMA_VERSION, |state| state.schema_version);
        let _lease = crate::daemon::admission::Lease::acquire(
            cache,
            &crate::ipc::protocol::build_id(),
            schema,
        )?;
        crate::store::Db::recover_catalog_for_startup(
            cache,
            &settings::db_from_config(&launch.config)?,
        )?;
    }
    Ok(())
}

pub(crate) fn survey(
    cache: &Path,
    launch: &LaunchConfig,
    explicit_policy: bool,
) -> Result<StartupPlans> {
    let cwd = std::env::current_dir()
        .map_err(|error| Error::internal_with_source("resolve startup directory", error))?;
    let cache = cwd.join(cache);
    let config = launch
        .path
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| cwd.join(dir))
        .ok_or_else(|| unavailable("startup configuration source is unavailable"))?;
    let limits = PlanningLimits::standard();
    let settings = settings::db_from_config(&launch.config)?;
    let global = launch.config.settings();
    let profile = &global.daemon.profile;
    let fixed = fixed_root(profile, global)?;
    let mut plans = StartupPlans {
        config,
        replacements: Vec::new(),
        expectations: StartupExpectations {
            cache,
            fixed,
            registry: None,
            sources: BTreeMap::new(),
        },
    };
    let expectations = &mut plans.expectations;
    if !explicit_policy && registry_inspect::has_legacy_startup_layout(&expectations.cache) {
        return Ok(plans);
    }
    let registry = observe_registry(
        &expectations.cache,
        expectations.fixed.as_deref(),
        &settings,
        limits,
    )?;
    let projects: &[_] = match &registry {
        RegistryState::Missing => &[][..],
        RegistryState::Ready {
            schema_version,
            projects,
            ..
        } if *schema_version == GENERATION_SCHEMA_VERSION || projects.is_empty() => projects,
        RegistryState::Ready { .. } if !explicit_policy => return Ok(plans),
        RegistryState::Ready { .. } => {
            return Err(unavailable(
                "explicit startup policy requires a v5 catalog for existing indexes",
            ));
        }
        RegistryState::Unavailable(reason) => {
            return Err(unavailable(format!(
                "could not observe the stopped registry ({reason:?}); use explicit Index to recover an interrupted registry, or update docsbase if its format is newer"
            )));
        }
    };
    for project in projects {
        let preview = observe::project_plan_for_profile(
            &plans.config,
            &expectations.cache,
            &project.root,
            &settings,
            limits,
            Some(profile),
        )?;
        if !preview.plan.body.blockers.is_empty() {
            let reasons: Vec<_> = preview
                .plan
                .body
                .blockers
                .iter()
                .map(|blocker| blocker.code)
                .collect();
            return Err(unavailable(format!(
                "startup plan blocked for {} ({reasons:?})",
                project.root.to_string_lossy().escape_debug()
            )));
        }
        for fact in &preview.plan.body.expected {
            if let ResourceId::Source(source) = &fact.resource
                && let Some(prior) = expectations
                    .sources
                    .insert(source.clone(), fact.state.clone())
                && prior != fact.state
            {
                return Err(unavailable(
                    "configuration sources changed during startup planning; retry",
                ));
            }
        }
        if !preview.plan.body.actions.is_empty() {
            plans
                .replacements
                .push((project.root.clone(), preview.plan));
        }
    }
    expectations.registry = Some(registry);
    Ok(plans)
}

fn fixed_root(profile: &DaemonProfile, global: &crate::config::Config) -> Result<Option<PathBuf>> {
    match profile {
        DaemonProfile::Shared => Ok(None),
        DaemonProfile::Website { project_root } => {
            lexical_only(global)?;
            project_root.canonicalize().map(Some).map_err(|error| {
                Error::internal_with_source("resolve fixed Website project", error)
            })
        }
    }
}

fn observe_registry(
    cache: &Path,
    fixed: Option<&Path>,
    settings: &Resolved<crate::store::settings::DbSettings>,
    limits: PlanningLimits,
) -> Result<RegistryState> {
    if let Some(root) = fixed {
        registry_inspect::inspect_startup_project(cache, root, limits.registry)
    } else {
        Ok(registry_inspect::inspect_registry(cache, settings, limits.registry)?.state)
    }
}

fn unavailable(message: impl Into<String>) -> Error {
    Error::Admission {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{launch::LaunchSnapshot, registry, settings::DaemonPatch};

    #[test]
    fn launch_rechecks_sources_of_unchanged_projects_and_the_registry() {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let snapshot = LaunchSnapshot::capture_from(config.path());
        let mut launch = snapshot.legacy(DaemonPatch::default()).unwrap();
        let settings = settings::db_from_config(&launch.config).unwrap();
        let register = |root: &Path| {
            let mut db = crate::store::Db::open_for_index(cache.path(), &settings, true).unwrap();
            registry::ensure_project(&mut db, root).unwrap();
            db.close().unwrap();
        };
        register(first.path());
        let observe_launch = || {
            let mut launch = snapshot.legacy(DaemonPatch::default()).unwrap();
            let plans = survey(cache.path(), &launch, true).unwrap();
            assert_eq!(plans.replacements, []);
            launch.startup = Some(plans.expectations);
            launch
        };
        launch.startup = observe_launch().startup;
        assert!(launch.recheck_startup(None).is_ok());
        let edited = first.path().join(".docsbase.toml");
        std::fs::write(&edited, "config_version=2\n").unwrap();
        let Err(error @ Error::StartupPlanStale) = launch.admit(cache.path(), &settings) else {
            panic!("expected launch admission to refuse a stale plan");
        };
        assert_eq!(crate::cli::error_exit_code(&error.into()), 4);
        assert!(!cache.path().join("state").exists());
        std::fs::remove_file(edited).unwrap();
        let admitted = observe_launch().admit(cache.path(), &settings).unwrap();
        let mut db = crate::store::Db::open_for_index(cache.path(), &settings, true).unwrap();
        assert!(admitted.launch.recheck_startup(Some(&db)).is_ok());
        registry::ensure_project(&mut db, second.path()).unwrap();
        assert!(admitted.launch.recheck_startup(Some(&db)).is_err());
        db.close().unwrap();
        assert!(admitted.launch.recheck_startup(None).is_err());

        let mut limits = PlanningLimits::standard().registry;
        limits.max_projects = 1;
        assert!(matches!(
            registry_inspect::inspect_registry(cache.path(), &settings, limits)
                .unwrap()
                .state,
            RegistryState::Unavailable(registry_inspect::UnavailableReason::LimitExceeded)
        ));
        let observed = registry_inspect::inspect_startup_project(
            cache.path(),
            &first.path().canonicalize().unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(observed, RegistryState::Ready { projects, .. } if projects.len() == 1));
    }
}
