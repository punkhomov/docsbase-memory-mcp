//! Direct indexing of a project registered in a v5 catalog.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::config::paths;
use crate::config::schema::Resolved;
use crate::daemon::registry;
use crate::daemon::settings::DaemonProfile;
use crate::error::{Error, Result};
use crate::index::async_job::catalog_sync::BuildContext;
use crate::index::{generation_job::GenerationJob, job::CaptureBudget, outcome};
use crate::search::budget::PayloadBudget;
use crate::search::operation::SearchOperation;
use crate::store::Db;
use crate::store::models::Project;
use crate::store::settings::DbSettings;
use crate::vector_cache::{VectorCacheSource, VectorStore};

/// The explicitly selected Website profile already requires one lexical
/// project. It can create a fresh catalog before any daemon owns that cache.
pub(super) fn create_for_website(global: &Config, project: &Config, root: &Path) -> bool {
    let DaemonProfile::Website { project_root } = &global.daemon.profile else {
        return false;
    };
    if !cfg!(unix) || !project_root.canonicalize().is_ok_and(|fixed| fixed == root) {
        return false;
    }
    lexical(global) && lexical(project)
}

pub(super) struct Target {
    cache: PathBuf,
    project: Project,
    epoch: [u8; 16],
    config: Arc<Config>,
    settings: Resolved<DbSettings>,
}

impl Target {
    pub(super) fn capture(
        mut db: Db,
        root: &Path,
        config: Config,
        settings: Resolved<DbSettings>,
        profile: &DaemonProfile,
    ) -> Result<Self> {
        if !cfg!(unix) {
            return Err(admission(
                "this platform has no qualified generation publication barrier",
            ));
        }
        if config.vector_store.is_none() {
            return Err(admission(
                "v5 index requires version 2 project configuration",
            ));
        }
        if matches!(profile, DaemonProfile::Website { .. }) && !lexical(&config) {
            return Err(admission(
                "website profile requires lexical-only global and project policy",
            ));
        }
        let cache = db.cache_root().canonicalize().map_err(|error| {
            Error::internal_with_source("resolve generation registry cache", error)
        })?;
        let epoch = db.generation_epoch()?;
        let project = registry::ensure_project(&mut db, root)?;
        // Project registration is complete. Preparation uses short registry
        // phases, leaving neither a writer connection nor a lock across staging.
        db.close()?;
        Ok(Self {
            cache,
            project,
            epoch,
            config: Arc::new(config),
            settings,
        })
    }

    pub(super) fn into_direct_index(
        self,
        owner: &super::super::command::Context,
        store: VectorStore,
    ) -> Result<super::DirectIndex> {
        if lexical(&self.config) {
            return Ok(super::DirectIndex::Generation(self));
        }
        let config_dir = paths::config_dir()?;
        let stages = Arc::new(owner.stages(&self.config)?);
        let pin_ttl = self.config.dense.ann_lifetimes.backfill_pin_ttl();
        let build = BuildContext {
            cache: self.cache,
            epoch: self.epoch,
            project: self.project,
            config: self.config,
            settings: self.settings,
            stages,
            active_model: None,
            source: VectorCacheSource::Owned(store),
            pin_ttl,
        };
        Ok(super::DirectIndex::HybridGeneration(build, config_dir))
    }

    pub(super) fn index(
        self,
        operation: &SearchOperation,
        budget: &PayloadBudget,
    ) -> Result<serde_json::Value> {
        operation.check()?;
        let project_id = self.project.id;
        let recipe = outcome::recipe_id(&self.config);
        let config_dir = paths::config_dir()?;
        let project_root = &self.project.canonical_root;
        let account = budget.reserve(0)?;
        let mut capture = CaptureBudget {
            account: &account,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let job = GenerationJob {
            cache: &self.cache,
            project: &self.project,
            epoch: self.epoch,
            settings: &self.settings,
            config: &self.config,
        };
        let prepared = job.stage(&mut capture)?;
        operation.check()?;
        let stats = prepared.publish(|commit| {
            paths::with_index_config_write_locks(
                Some(&config_dir.join("config.toml")),
                project_root,
                || {
                    operation.check()?;
                    let current = Config::load_from(&config_dir, Some(project_root))?;
                    if !lexical(&current) || outcome::recipe_id(&current) != recipe {
                        return Err(admission(
                            "project index configuration changed; retry index",
                        ));
                    }
                    commit.commit(|| operation.check())
                },
            )
        })?;
        Ok(serde_json::json!({ "project_id": project_id, "stats": stats }))
    }
}

fn lexical(config: &Config) -> bool {
    config.vector_store.is_some() && !config.hybrid && !config.hybrid_cfg.enabled
}

fn admission(message: impl Into<String>) -> Error {
    Error::Admission {
        message: message.into(),
    }
}
