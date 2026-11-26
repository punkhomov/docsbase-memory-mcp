//! Publish an ordinary lexical edit within the selected local generation.

use rusqlite::params;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::job::{self, CaptureBudget, JobInput, JobStats};
use crate::index::{outcome, tantivy_index::IndexHandle};
use crate::store::generation_build::sync_directory;
use crate::store::models::Project;
use crate::store::{Db, registry_inspect, repo};

use super::{
    LOCAL_REVISION_SCHEMA_SQL, LOCAL_SCHEMA_VERSION, PairAccess, SEALED_LOCAL_SCHEMA_VERSION,
    invalid_index, unavailable, with_catalog_pair_using,
};

/// The project lease excludes both readers and head publication. The admission
/// guard returned after capture stays live until the durable commit is settled.
pub(crate) fn apply_lexical_batch<A>(
    registry: &Db,
    project: &Project,
    config: &Config,
    input: &JobInput<'_>,
    capture: Option<&mut CaptureBudget<'_>>,
    admit: impl FnOnce() -> Result<A>,
) -> Result<JobStats> {
    if !cfg!(unix) || config.hybrid || config.hybrid_cfg.enabled {
        return Err(Error::Admission {
            message: "incremental generation updates require a supported lexical installation"
                .into(),
        });
    }
    with_catalog_pair_using(registry, project, PairAccess::Write, |_, local, index| {
        let mut local = local.ok_or_else(|| invalid_index("index has no local generation"))?;
        if local.index_recipe != outcome::recipe_id(config) {
            return Err(invalid_index(
                "index recipe changed and requires a staged rebuild",
            ));
        }
        let tantivy = local.tantivy_dir()?;
        let (mut plan, scope) =
            job::capture_input(&local.db, project, config, false, input, capture)?;
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
        let _admission = admit()?;
        let Some(scope) = scope.filter(|_| plan.has_changes()) else {
            return Ok(plan.stats);
        };

        let expected = index
            .content_revision()
            .checked_add(1)
            .ok_or_else(|| invalid_index("index content revision is exhausted"))?;
        begin_mutation(&mut local.db)?;
        let attempt = outcome::begin(&local.db, project)?;
        let mut writer = IndexHandle::open_existing_writer(&tantivy, config.index_writer)?;
        plan.begin(&mut local.db, &mut writer, project.id)?;
        plan.commit(&mut local.db, &mut writer, project.id)?;
        writer.finish()?;
        // Tantivy syncs committed file contents. Its final merged directory
        // entries must reach the barrier before SQLite makes them readable.
        sync_directory(&tantivy)?;
        let manifest = registry_inspect::tantivy_manifest(&tantivy)
            .map_err(|reason| unavailable("updated Tantivy manifest", reason))?;

        let tx = local.db.begin_write()?;
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
        tx.commit().map_err(repo::db_error)?;
        Ok(plan.stats)
    })
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
