//! Attributable latest lexical indexing result (IO1; website FR-S2).
//!
//! Attempt tokens fence metadata publication; they are not search generations,
//! counts of jobs or replacements for the caller's project writer lease.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::store::models::Project;
use crate::store::repo::{self, db_error};
use crate::store::{Db, migrations};

use super::job::{JobStats, MAX_CHUNK_CHARS, MAX_WARNINGS};

/// Maximum serialized outcome/root size accepted by the outcome format.
/// Corrupt larger values are never loaded into Rust strings by the observer.
const MAX_OUTCOME_BYTES: usize = 1_048_576;
const OUTCOME_VERSION: u32 = 1;
// Bump for changes to traversal, metadata extraction or chunk policy that
// affect corpus coverage. Tokenizer changes already carry their own version.
const RECIPE_VERSION: u32 = 1;
const RECIPE_PREFIX: &str = "index-recipe-v1:";

/// Actual scope of the completed pass, including incremental full recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurveyScope {
    /// Every discoverable document was surveyed, including unchanged files.
    FullSurvey,
    /// Only a supplied subset was considered; no corpus completeness claim.
    Subset,
}

/// A completed lexical job, validated against its current project row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOutcome {
    /// Outcome serialization version, independent of the database schema.
    pub schema_version: u32,
    /// Bookkeeping token; gaps from status/wrapper invalidations are allowed.
    pub attempt_id: i64,
    /// Existing generation read after the index commits.
    pub index_generation: i64,
    /// Exact registered root used by the job.
    pub canonical_root: String,
    /// Relevant effective indexing settings and algorithm identity.
    pub recipe_id: String,
    /// Full survey or subset; zero-error subset is still incomplete evidence.
    pub scope: SurveyScope,
    /// Actual counters and bounded warnings from the job, without alteration.
    pub stats: JobStats,
}

impl IndexOutcome {
    /// Whether this pass established complete lexical coverage for its recipe.
    /// This does not prove source freshness after the scan or terminal hybrid
    /// sync success: embedding backfill can still fail after the lexical pass.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.scope == SurveyScope::FullSurvey && self.stats.errors == 0
    }
}

/// Identity of indexing inputs; excludes providers, secrets and query budgets.
/// Ignore files belong to the surveyed source tree, not a freshness guarantee.
#[must_use]
pub fn recipe_id(config: &Config) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"docsbase-index-outcome-recipe\0");
    hash.update(&RECIPE_VERSION.to_le_bytes());
    hash.update(&super::tokenizer::TOKENIZER_VERSION.to_le_bytes());
    hash.update(&(MAX_CHUNK_CHARS as u64).to_le_bytes());
    hash.update(&config.max_file_size.to_le_bytes());
    hash.update(&(config.max_docs_per_project as u64).to_le_bytes());
    for pattern in &config.ignores {
        hash.update(&(pattern.len() as u64).to_le_bytes());
        hash.update(pattern.as_bytes());
    }
    format!("{RECIPE_PREFIX}{}", hash.finalize().to_hex())
}

/// Read bounded evidence in one metadata SELECT without providers or writes.
///
/// Absent, malformed, oversized, incompatible or superseded records are unknown
/// (`None`). Callers compare the recipe to their effective policy. Correlating
/// this result with Tantivy additionally requires the existing project read
/// lease; this observer does not acquire one or create coordination files.
///
/// # Errors
/// Returns database access errors; never opens, migrates or recovers a database.
pub fn read(db: &Db, project_id: i64) -> Result<Option<IndexOutcome>> {
    let byte_bound = i64::try_from(MAX_OUTCOME_BYTES)
        .map_err(|_| Error::internal("outcome byte bound exceeds SQLite integer range"))?;
    let raw = db
        .connection()
        .query_row(
            "SELECT index_attempt, index_generation, schema_version,
                CASE WHEN typeof(canonical_root) = 'text'
                    AND length(CAST(canonical_root AS BLOB)) <= ?2
                    THEN CAST(canonical_root AS BLOB) END,
                CASE WHEN typeof(index_outcome_json) = 'text'
                    AND length(CAST(index_outcome_json AS BLOB)) <= ?2
                    THEN CAST(index_outcome_json AS BLOB) END,
                status = 'indexed',
                EXISTS(SELECT 1 FROM docs WHERE project_id = ?1 AND content_hash = '')
             FROM projects WHERE id = ?1
                AND typeof(index_attempt) = 'integer'
                AND typeof(index_generation) = 'integer'
                AND typeof(schema_version) = 'integer'",
            params![project_id, byte_bound],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?;
    let Some((attempt, generation, stamp, Some(root), Some(json), indexed, pending)) = raw else {
        return Ok(None);
    };
    let Ok(outcome) = serde_json::from_slice::<IndexOutcome>(&json) else {
        return Ok(None);
    };
    let recipe = outcome.recipe_id.strip_prefix(RECIPE_PREFIX);
    if !indexed
        || pending
        || stamp != i64::from(migrations::SCHEMA_VERSION)
        || attempt <= 0
        || generation < 0
        || outcome.schema_version != OUTCOME_VERSION
        || outcome.attempt_id != attempt
        || outcome.index_generation != generation
        || outcome.canonical_root.as_bytes() != root
        || outcome.stats.warnings.len() > MAX_WARNINGS
        || !recipe.is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Ok(None);
    }
    Ok(Some(outcome))
}

/// Revoke proof without changing status, before watcher filtering/index opens.
pub(crate) fn invalidate(db: &Db, project_id: i64) -> Result<()> {
    let count = db
        .connection()
        .execute(
            "UPDATE projects SET index_attempt = index_attempt + 1,
                index_outcome_json = NULL WHERE id = ?1",
            [project_id],
        )
        .map_err(db_error)?;
    if count != 1 {
        return Err(Error::internal(format!(
            "unknown indexing project {project_id}"
        )));
    }
    Ok(())
}

/// One actual core pass. The caller owns its existing writer lease; no SQL
/// transaction crosses traversal, provider execution or Tantivy work.
pub(crate) fn run(
    db: &mut Db,
    project: &Project,
    config: &Config,
    scope: SurveyScope,
    work: impl FnOnce(&mut Db) -> Result<JobStats>,
) -> Result<JobStats> {
    let root = project.canonical_root.to_string_lossy();
    let attempt: i64 = db
        .connection()
        .query_row(
            "UPDATE projects SET index_attempt = index_attempt + 1,
                index_outcome_json = NULL WHERE id = ?1 AND canonical_root = ?2
             RETURNING index_attempt",
            params![project.id, root.as_ref()],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    let stats = work(db)?;
    let tx = db.begin_write()?;
    let generation = repo::index_generation(&tx, project.id)?;
    let outcome = IndexOutcome {
        schema_version: OUTCOME_VERSION,
        attempt_id: attempt,
        index_generation: generation,
        canonical_root: root.into_owned(),
        recipe_id: recipe_id(config),
        scope,
        stats: stats.clone(),
    };
    let json = serde_json::to_string(&outcome)
        .map_err(|err| Error::internal_with_source("serialize indexing outcome", err))?;
    if json.len() > MAX_OUTCOME_BYTES {
        return Err(Error::internal("indexing outcome exceeds its format bound"));
    }
    let updated = tx
        .execute(
            "UPDATE projects SET index_outcome_json = ?1, status = 'indexed',
                last_indexed_at = ?2,
                schema_version = CASE WHEN ?3 THEN ?4 ELSE schema_version END
             WHERE id = ?5 AND index_attempt = ?6 AND canonical_root = ?7",
            params![
                json,
                super::job::unix_now(),
                scope == SurveyScope::FullSurvey,
                migrations::SCHEMA_VERSION,
                project.id,
                attempt,
                outcome.canonical_root,
            ],
        )
        .map_err(db_error)?;
    if updated != 1 {
        return Err(Error::internal(
            "indexing outcome was superseded before publication",
        ));
    }
    tx.commit().map_err(db_error)?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::registry;
    use crate::store::models::ProjectStatus;

    #[test]
    fn late_publication_cannot_resurrect_a_lifecycle_invalidated_result() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let error = run(
            &mut db,
            &project,
            &Config::default(),
            SurveyScope::FullSurvey,
            |db| {
                registry::set_status(db, project.id, ProjectStatus::Error)?;
                Ok(JobStats::default())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("superseded"), "{error}");
        assert!(read(&db, project.id).unwrap().is_none());
        assert_eq!(
            registry::project_by_id(&db, project.id)
                .unwrap()
                .unwrap()
                .status,
            ProjectStatus::Error
        );
    }

    #[test]
    fn stale_publication_preserves_a_newer_completed_result() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let config = Config::default();
        let error = run(&mut db, &project, &config, SurveyScope::Subset, |db| {
            run(db, &project, &config, SurveyScope::FullSurvey, |_| {
                Ok(JobStats {
                    skipped: 42,
                    ..JobStats::default()
                })
            })?;
            Ok(JobStats::default())
        })
        .unwrap_err();
        assert!(error.to_string().contains("superseded"), "{error}");
        let result = read(&db, project.id).unwrap().unwrap();
        assert!(result.is_complete());
        assert_eq!(result.stats.skipped, 42);
    }

    #[test]
    fn exhausted_attempt_token_refuses_before_running_the_job() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        db.connection()
            .execute(
                "UPDATE projects SET index_attempt=?1 WHERE id=?2",
                params![i64::MAX, project.id],
            )
            .unwrap();
        let ran = std::cell::Cell::new(false);
        let error = run(
            &mut db,
            &project,
            &Config::default(),
            SurveyScope::FullSurvey,
            |_| {
                ran.set(true);
                Ok(JobStats::default())
            },
        );
        assert!(error.is_err());
        assert!(!ran.get());
        let attempt: i64 = db
            .connection()
            .query_row(
                "SELECT index_attempt FROM projects WHERE id=?1",
                [project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attempt, i64::MAX);
    }
}
