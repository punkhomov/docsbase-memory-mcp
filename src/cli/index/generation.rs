//! Direct indexing of a project registered in a v5 catalog.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::config::paths;
use crate::config::schema::Resolved;
use crate::daemon::registry;
use crate::daemon::settings::DaemonProfile;
use crate::error::{Error, Result};
use crate::index::async_job::catalog_sync::{BuildContext, DirectSourcePaths};
use crate::index::{
    generation_job::{GenerationBase, GenerationJob},
    job::{CaptureBudget, JobStats},
    outcome,
};
use crate::search::budget::PayloadBudget;
use crate::search::operation::SearchOperation;
use crate::store::Db;
use crate::store::models::Project;
use crate::store::settings::DbSettings;
use crate::vector_cache::{VectorCacheSource, VectorStore};

/// Shared v2 can initialize an empty catalog. Website additionally requires
/// the fixed project and lexical-only settings.
pub(super) fn initializes_catalog(global: &Config, project: &Config, root: &Path) -> bool {
    match &global.daemon.profile {
        DaemonProfile::Shared => project.vector_store.is_some(),
        DaemonProfile::Website { project_root } => {
            project_root.canonicalize().is_ok_and(|fixed| fixed == root)
                && lexical(global)
                && lexical(project)
        }
    }
}

pub(super) struct Target {
    cache: PathBuf,
    project: Project,
    epoch: [u8; 16],
    config: Arc<Config>,
    settings: Resolved<DbSettings>,
    sources: DirectSourcePaths,
}

impl Target {
    pub(super) fn capture(
        mut db: Db,
        root: &Path,
        config: Config,
        settings: Resolved<DbSettings>,
        profile: &DaemonProfile,
    ) -> Result<Self> {
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
        let sources = DirectSourcePaths {
            config_dir: paths::config_dir()?,
            config_lock_dir: paths::config_lock_dir()?,
        };
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
            sources,
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
        Ok(super::DirectIndex::HybridGeneration(build, self.sources))
    }

    pub(super) fn index(
        self,
        operation: &SearchOperation,
        budget: &PayloadBudget,
    ) -> Result<serde_json::Value> {
        operation.check()?;
        let project_id = self.project.id;
        let recipe = outcome::recipe_id(&self.config);
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
        if let Some(stats) = job.reuse_unchanged(operation, |_, generation| {
            self.with_current_recipe(&recipe, operation, || {
                Ok(JobStats {
                    skipped: generation.skipped,
                    ..JobStats::default()
                })
            })
        })? {
            return Ok(serde_json::json!({ "project_id": project_id, "stats": stats }));
        }
        let prepared = job.stage(&mut capture, GenerationBase::Observe)?;
        operation.check()?;
        let stats = prepared.publish(|commit| {
            self.with_current_recipe(&recipe, operation, || commit.commit(|| operation.check()))
        })?;
        Ok(serde_json::json!({ "project_id": project_id, "stats": stats }))
    }

    fn with_current_recipe<T>(
        &self,
        recipe: &str,
        operation: &SearchOperation,
        finish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let project_root = &self.project.canonical_root;
        paths::with_index_config_write_locks(
            &self.sources.config_lock_dir,
            Some(&self.sources.config_dir.join("config.toml")),
            project_root,
            || {
                operation.check()?;
                let current = Config::load_from(&self.sources.config_dir, Some(project_root))?;
                if !lexical(&current) || outcome::recipe_id(&current) != recipe {
                    return Err(admission(
                        "project index configuration changed; retry index",
                    ));
                }
                finish()
            },
        )
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
