//! Direct indexing of a project registered in a v5 catalog.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::config::paths;
use crate::config::schema::Resolved;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::index::{job::CaptureBudget, outcome};
use crate::search::budget::PayloadBudget;
use crate::search::operation::SearchOperation;
use crate::store::Db;
use crate::store::generation_build::{LexicalBuildRequest, with_prepared_lexical_generation};
use crate::store::models::Project;
use crate::store::registry_inspect::{ProjectIndexObservation, inspect_project_index};
use crate::store::settings::DbSettings;

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
    ) -> Result<Self> {
        if !cfg!(unix) {
            return Err(admission(
                "this platform has no qualified generation publication barrier",
            ));
        }
        if !lexical(&config) {
            return Err(admission("v5 index requires lexical project configuration"));
        }
        let cache = db.cache_root().canonicalize().map_err(|error| {
            Error::internal_with_source("resolve generation registry cache", error)
        })?;
        let epoch = db.generation_epoch()?;
        let project = registry::ensure_project(&mut db, root)?;
        // The writer's native close retires any recoverable SQLite sidecars
        // before the stopped preparation inspector takes its no-write lease.
        db.close()?;
        Ok(Self {
            cache,
            project,
            epoch,
            config: Arc::new(config),
            settings,
        })
    }

    pub(super) fn index(
        self,
        operation: &SearchOperation,
        budget: &PayloadBudget,
    ) -> Result<serde_json::Value> {
        operation.check()?;
        let limits = crate::config::observe::PlanningLimits::standard().registry;
        let project_id = self.project.id;
        let base = match inspect_project_index(&self.cache, project_id, &self.settings, limits)? {
            ProjectIndexObservation::Missing => None,
            ProjectIndexObservation::Present(base) => Some(base),
            ProjectIndexObservation::Unavailable(reason) => {
                return Err(admission(format!(
                    "generation base unavailable ({reason:?})"
                )));
            }
        };
        let recipe = outcome::recipe_id(&self.config);
        let reserved_disk_bytes = self.config.index_writer.staging_disk_bytes();
        let mut plan = blake3::Hasher::new_derive_key("docsbase.index.direct.lexical.v1");
        plan.update(&self.epoch);
        plan.update(&project_id.to_le_bytes());
        // An explicit index invocation owns work in this installation and
        // project even if the user changes a setting before retrying. The
        // staged receipt and CAS separately bind base, reservation and recipe.
        let request = LexicalBuildRequest {
            project_id,
            epoch: self.epoch,
            base,
            plan_digest: *plan.finalize().as_bytes(),
            reserved_disk_bytes,
        };
        let config_dir = paths::config_dir()?;
        let project_root = &self.project.canonical_root;
        let account = budget.reserve(0)?;
        let mut capture = CaptureBudget {
            account: &account,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let stats = with_prepared_lexical_generation(
            &self.cache,
            request,
            &self.settings,
            limits,
            &self.config,
            &mut capture,
            |prepared| {
                operation.check()?;
                prepared.publish(|commit| {
                    with_config_guards(&config_dir, project_root, || {
                        operation.check()?;
                        let current = Config::load_from(&config_dir, Some(project_root))?;
                        if !lexical(&current) || outcome::recipe_id(&current) != recipe {
                            return Err(admission(
                                "project index configuration changed; retry index",
                            ));
                        }
                        commit.commit(|| operation.check())
                    })
                })
            },
        )?;
        Ok(serde_json::json!({ "project_id": project_id, "stats": stats }))
    }
}

fn lexical(config: &Config) -> bool {
    config.vector_store.is_some() && !config.hybrid && !config.hybrid_cfg.enabled
}

fn with_config_guards<T>(
    config_dir: &Path,
    root: &Path,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    paths::with_config_write_lock(&config_dir.join("config.toml"), || {
        paths::with_config_write_lock(&root.join(".docsbase.toml"), || Ok(work()?))
    })
    .map_err(|error| match error.downcast::<Error>() {
        Ok(error) => error,
        Err(error) => admission(format!("could not lock the index configuration: {error}")),
    })
}

fn admission(message: impl Into<String>) -> Error {
    Error::Admission {
        message: message.into(),
    }
}
