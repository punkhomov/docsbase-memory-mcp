//! Publish an ordinary edit within the selected local generation.

use rusqlite::params;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::job::{self, CaptureBudget, JobInput, JobStats, PreparedPlan};
use crate::index::{outcome, tantivy_index::IndexHandle};
use crate::platform::file_lease::ExclusiveFileLease;
use crate::store::generation_build::sync_directory;
use crate::store::models::Project;
use crate::store::{Db, registry_inspect, repo};
use crate::vector_cache::EmbeddingRecipe;
use std::path::Path;

use super::{
    LOCAL_REVISION_SCHEMA_SQL, LOCAL_SCHEMA_VERSION, LocalMetadata, PairAccess, ProjectIndex,
    SEALED_LOCAL_SCHEMA_VERSION, canonical_cache, invalid_index, project_directory, unavailable,
    with_catalog_pair, with_catalog_pair_using,
};

/// The project lease excludes both readers and head publication. The admission
/// check rejects obsolete watcher settings before using the selected recipe.
/// The recheck after capture stays live until the durable commit settles.
pub(crate) fn apply_lexical_batch<A>(
    registry: &Db,
    project: &Project,
    config: &Config,
    input: &JobInput<'_>,
    capture: Option<&mut CaptureBudget<'_>>,
    admit: impl Fn() -> Result<A>,
) -> Result<JobStats> {
    if !cfg!(unix) || config.hybrid || config.hybrid_cfg.enabled {
        return Err(Error::Admission {
            message: "incremental generation updates require a supported lexical installation"
                .into(),
        });
    }
    with_catalog_pair_using(registry, project, PairAccess::Write, |_, local, index| {
        drop(admit()?);
        let mut local = checked_local(local, config)?;
        require_watch_recipe(&local, None)?;
        let tantivy = local.tantivy_dir()?;
        let (mut plan, scope) = capture_batch(&local.db, project, config, input, capture)?;
        let _admission = admit()?;
        if let Some(scope) = scope {
            publish_batch(
                &mut local.db,
                project,
                config,
                &tantivy,
                index,
                &mut plan,
                scope,
            )?;
        }
        Ok(plan.stats)
    })
}

/// A captured update retains preparation authority, without a metadata
/// connection or project read/write lease during inference.
pub(crate) struct PreparedWatch {
    plan: PreparedPlan,
    scope: Option<outcome::SurveyScope>,
    index: ProjectIndex,
    _preparation: ExclusiveFileLease,
}

impl PreparedWatch {
    pub(crate) fn capture<A>(
        registry: &Db,
        project: &Project,
        config: &Config,
        input: &JobInput<'_>,
        model: Option<&str>,
        capture: &mut CaptureBudget<'_>,
        admit: impl FnOnce() -> Result<A>,
    ) -> Result<Self> {
        if !cfg!(unix) {
            return Err(invalid_index(
                "platform cannot publish incremental generations",
            ));
        }
        let directory = project_directory(&canonical_cache(registry.cache_root())?, project.id);
        registry_inspect::check_directory(&directory)
            .map_err(|reason| unavailable("project generation directory", reason))?;
        let preparation = crate::store::generation_build::acquire_preparation_lease(&directory)?;
        with_catalog_pair(registry, project, |_, local, index| {
            let _admission = admit()?;
            let local = checked_local(local, config)?;
            require_watch_recipe(&local, model)?;
            local.tantivy_dir()?;
            let (plan, scope) = capture_batch(&local.db, project, config, input, Some(capture))?;
            Ok(Self {
                plan,
                scope,
                index,
                _preparation: preparation,
            })
        })
    }

    pub(crate) fn plan(&self) -> &PreparedPlan {
        &self.plan
    }

    pub(crate) fn with_base_metadata<T>(
        &self,
        registry: &Db,
        project: &Project,
        read: impl FnOnce(&Db) -> Result<T>,
    ) -> Result<T> {
        with_catalog_pair(registry, project, |_, local, index| {
            self.require_index(index)?;
            let local = local.ok_or_else(|| invalid_index("watcher has no local generation"))?;
            read(&local.db)
        })
    }

    pub(crate) fn publish<A, Hold>(
        &mut self,
        registry: &Db,
        project: &Project,
        config: &Config,
        model: Option<&str>,
        admit: impl FnOnce() -> Result<A>,
        hold_vectors: impl FnOnce() -> Result<Hold>,
    ) -> Result<JobStats> {
        let Some(scope) = self.scope else {
            return Ok(std::mem::take(&mut self.plan.stats));
        };
        with_catalog_pair_using(
            registry,
            project,
            PairAccess::PreparedWrite,
            |_, local, index| {
                let _admission = admit()?;
                self.require_index(index)?;
                let mut local = checked_local(local, config)?;
                require_watch_recipe(&local, model)?;
                let tantivy = local.tantivy_dir()?;
                let _vectors = hold_vectors()?;
                publish_batch(
                    &mut local.db,
                    project,
                    config,
                    &tantivy,
                    index,
                    &mut self.plan,
                    scope,
                )?;
                Ok(std::mem::take(&mut self.plan.stats))
            },
        )
    }

    fn require_index(&self, index: ProjectIndex) -> Result<()> {
        if self.index != index {
            return Err(Error::internal(
                "retry watcher against the current index revision",
            ));
        }
        Ok(())
    }
}

fn require_watch_recipe(local: &LocalMetadata, model: Option<&str>) -> Result<()> {
    if local.embedding_recipe != model.map(EmbeddingRecipe::for_model) {
        return Err(invalid_index(
            "embedding recipe changed and requires a staged rebuild",
        ));
    }
    Ok(())
}

fn checked_local(local: Option<LocalMetadata>, config: &Config) -> Result<LocalMetadata> {
    let local = local.ok_or_else(|| invalid_index("index has no local generation"))?;
    if local.index_recipe != outcome::recipe_id(config) {
        return Err(invalid_index(
            "index recipe changed and requires a staged rebuild",
        ));
    }
    Ok(local)
}

fn capture_batch(
    db: &Db,
    project: &Project,
    config: &Config,
    input: &JobInput<'_>,
    capture: Option<&mut CaptureBudget<'_>>,
) -> Result<(PreparedPlan, Option<outcome::SurveyScope>)> {
    let (plan, scope) = job::capture_input(db, project, config, false, input, capture)?;
    if scope == Some(outcome::SurveyScope::FullSurvey) && plan.stats.errors != 0 {
        let detail = plan
            .stats
            .warnings
            .first()
            .map_or_else(String::new, |warning| {
                format!(" ({}: {})", warning.path, warning.message)
            });
        return Err(Error::Index {
            path: Some(project.canonical_root.clone()),
            message: format!("could not capture a complete index batch{detail}"),
        });
    }
    let scope = scope.filter(|_| plan.has_changes());
    Ok((plan, scope))
}

fn publish_batch(
    db: &mut Db,
    project: &Project,
    config: &Config,
    tantivy: &Path,
    index: ProjectIndex,
    plan: &mut PreparedPlan,
    scope: outcome::SurveyScope,
) -> Result<()> {
    let expected = index
        .content_revision()
        .checked_add(1)
        .ok_or_else(|| invalid_index("index content revision is exhausted"))?;
    begin_mutation(db)?;
    let attempt = outcome::begin(db, project)?;
    let mut writer = IndexHandle::open_existing_writer(tantivy, config.index_writer)?;
    plan.begin(db, &mut writer, project.id)?;
    plan.commit(db, &mut writer, project.id)?;
    writer.finish()?;
    // Tantivy syncs committed file contents. Its final merged directory
    // entries must reach the barrier before SQLite makes them readable.
    sync_directory(tantivy)?;
    let manifest = registry_inspect::tantivy_manifest(tantivy)
        .map_err(|reason| unavailable("updated Tantivy manifest", reason))?;
    let tx = db.begin_write()?;
    if repo::index_generation(&tx, project.id)? != expected {
        return Err(invalid_index(
            "index content revision did not advance exactly once",
        ));
    }
    outcome::publish_in(&tx, project, config, scope, attempt, &plan.stats)?;
    let updated = tx
        .execute(
            "UPDATE generation_state
             SET content_revision=?1, tantivy_manifest=?2, mutation_pending=0
             WHERE singleton=1 AND mutation_pending=1 AND content_revision=?3",
            params![expected, manifest.as_slice(), index.content_revision()],
        )
        .map_err(repo::db_error)?;
    if updated != 1 {
        return Err(invalid_index("index mutation intent was superseded"));
    }
    tx.commit().map_err(repo::db_error)
}

fn begin_mutation(metadata: &mut Db) -> Result<()> {
    let journal: String = metadata
        .connection()
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(repo::db_error)?;
    if journal != "delete" {
        return Err(invalid_index(
            "local index requires the DELETE journal mode",
        ));
    }
    metadata
        .connection()
        .pragma_update(None, "synchronous", "EXTRA")
        .map_err(repo::db_error)?;
    metadata
        .connection()
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(repo::db_error)?;
    let version = metadata.schema_version()?;
    let tx = metadata.begin_write()?;
    if version == SEALED_LOCAL_SCHEMA_VERSION {
        tx.execute_batch(LOCAL_REVISION_SCHEMA_SQL)
            .map_err(repo::db_error)?;
        tx.pragma_update(None, "user_version", LOCAL_SCHEMA_VERSION)
            .map_err(repo::db_error)?;
    }
    let updated = tx
        .execute(
            "UPDATE generation_state SET mutation_pending=1
             WHERE singleton=1 AND mutation_pending=0",
            [],
        )
        .map_err(repo::db_error)?;
    if updated != 1 {
        return Err(invalid_index("index update is already pending"));
    }
    tx.commit().map_err(repo::db_error)
}
