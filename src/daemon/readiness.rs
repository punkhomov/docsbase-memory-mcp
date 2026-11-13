//! Bounded read-only Website readiness over the existing index/writer contract.
use std::str::FromStr;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::job::{JobStats, MAX_WARNINGS};
use crate::index::outcome::{self, IndexOutcome, SurveyScope};
use crate::index::tantivy_index::ReadIndex;
use crate::ipc::panel::{IndexHealth, ReadinessReason, ServerProject, ServerStatus, ServerSync};
use crate::store::models::{Project, SyncState};
use crate::store::read_pool::ReadPool;
use crate::store::repo::db_error;
use crate::store::{Db, migrations};

use super::lifecycle;

/// Minimal durable association, captured while the lexical writer lease is held.
/// `JobStats` remain the existing top-level fields in `sync_jobs.stats_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncAttribution {
    schema_version: u32,
    attempt_id: i64,
    index_generation: i64,
    canonical_root: String,
    recipe_id: String,
    scope: SurveyScope,
}
impl From<&IndexOutcome> for SyncAttribution {
    fn from(outcome: &IndexOutcome) -> Self {
        Self {
            schema_version: outcome.schema_version,
            attempt_id: outcome.attempt_id,
            index_generation: outcome.index_generation,
            canonical_root: outcome.canonical_root.clone(),
            recipe_id: outcome.recipe_id.clone(),
            scope: outcome.scope,
        }
    }
}
impl SyncAttribution {
    pub(crate) fn valid(&self) -> bool {
        self.schema_version == 1
            && self.attempt_id > 0
            && self.index_generation >= 0
            && self
                .recipe_id
                .strip_prefix("index-recipe-v1:")
                .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyncEvidence {
    id: i64,
    state: SyncState,
    started_at: i64,
    finished_at: Option<i64>,
    stats_len: Option<i64>,
    raw_stats: Option<Vec<u8>>,
}
impl SyncEvidence {
    fn read(db: &Db, project: i64, active: bool, bound: i64) -> Result<Option<Self>> {
        let sql = if active {
            "SELECT id, CASE WHEN typeof(state)='text' AND length(CAST(state AS BLOB))<=?2 THEN state END, started_at, finished_at, length(CAST(stats_json AS BLOB)),
                CASE WHEN typeof(stats_json)='text' AND length(CAST(stats_json AS BLOB))<=?2
                    THEN CAST(stats_json AS BLOB) END
             FROM sync_jobs WHERE project_id=?1 AND state IN ('queued','running') ORDER BY id DESC LIMIT 1"
        } else {
            "SELECT id, CASE WHEN typeof(state)='text' AND length(CAST(state AS BLOB))<=?2 THEN state END, started_at, finished_at, length(CAST(stats_json AS BLOB)),
                CASE WHEN typeof(stats_json)='text' AND length(CAST(stats_json AS BLOB))<=?2
                    THEN CAST(stats_json AS BLOB) END
             FROM sync_jobs WHERE project_id=?1 ORDER BY id DESC LIMIT 1"
        };
        let row = db
            .connection()
            .query_row(sql, params![project, bound], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                ))
            })
            .optional()
            .map_err(db_error)?;
        row.map(
            |(id, state, started_at, finished_at, stats_len, raw_stats)| {
                Ok(Self {
                    id,
                    state: SyncState::from_str(&state)
                        .map_err(|err| Error::internal_with_source("invalid sync state", err))?,
                    started_at,
                    finished_at,
                    stats_len,
                    raw_stats,
                })
            },
        )
        .transpose()
    }
    fn json(&self) -> Option<Value> {
        self.raw_stats
            .as_deref()
            .and_then(|raw| serde_json::from_slice(raw).ok())
    }
    fn stats(&self) -> Option<JobStats> {
        let stats: JobStats = serde_json::from_slice(self.raw_stats.as_deref()?).ok()?;
        (stats.warnings.len() <= MAX_WARNINGS).then_some(stats)
    }
    fn attribution(&self) -> Option<SyncAttribution> {
        let identity: SyncAttribution =
            serde_json::from_value(self.json()?.get("index_outcome")?.clone()).ok()?;
        identity.valid().then_some(identity)
    }
    fn known_partial(&self) -> bool {
        self.json()
            .and_then(|json| json.get("errors").and_then(Value::as_u64))
            .is_some_and(|errors| errors > 0)
    }
    fn interrupted(&self) -> bool {
        self.state == SyncState::Error
            && self
                .json()
                .is_some_and(|json| json.get("orphaned") == Some(&Value::Bool(true)))
    }
    fn view(&self) -> ServerSync {
        ServerSync {
            job_id: self.id,
            state: self.state.as_str().into(),
            attempt_id: self.attribution().map(|identity| identity.attempt_id),
            stats: self.stats(),
        }
    }
    fn attributable_success(&self, snapshot: &Snapshot) -> bool {
        let Some(identity) = self.attribution() else {
            return false;
        };
        let Some(stats) = self.stats() else {
            return false;
        };
        self.state == SyncState::Done
            && self.finished_at.is_some()
            && stats.errors == 0
            && identity.scope == SurveyScope::FullSurvey
            && identity.canonical_root == snapshot.project.root
            && identity.attempt_id <= snapshot.attempt
            && identity.index_generation <= snapshot.generation
            && (identity.attempt_id != snapshot.attempt
                || snapshot.outcome.as_ref().is_some_and(|current| {
                    identity == SyncAttribution::from(current) && stats == current.stats
                }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    project: ServerProject,
    schema_version: i64,
    generation: i64,
    attempt: i64,
    outcome_len: Option<i64>,
    raw_outcome: Option<Vec<u8>>,
    outcome: Option<IndexOutcome>,
    active: Option<SyncEvidence>,
    latest: Option<SyncEvidence>,
}
impl Snapshot {
    fn read(db: &Db, project: i64) -> Result<Self> {
        let bound = i64::try_from(crate::limits::MAX_STATE_BYTES)
            .map_err(|_| Error::internal("readiness metadata bound overflow"))?;
        let tx = db.connection().unchecked_transaction().map_err(db_error)?;
        let mut snapshot = tx.query_row(
            "SELECT id,
                CASE WHEN typeof(name)='text' AND length(CAST(name AS BLOB))<=?2 THEN name END,
                CASE WHEN typeof(canonical_root)='text' AND length(CAST(canonical_root AS BLOB))<=?2 THEN canonical_root END,
                CASE WHEN typeof(status)='text' AND length(CAST(status AS BLOB))<=?2 THEN status END,
                schema_version,index_generation,index_attempt,length(CAST(index_outcome_json AS BLOB)),
                CASE WHEN typeof(index_outcome_json)='text' AND length(CAST(index_outcome_json AS BLOB))<=?2
                    THEN CAST(index_outcome_json AS BLOB) END
             FROM projects WHERE id=?1", params![project,bound], |row| {
                Ok(Self { project: ServerProject { id: row.get(0)?, name: row.get(1)?, root: row.get(2)?, status: row.get(3)? },
                    schema_version: row.get(4)?, generation: row.get(5)?, attempt: row.get(6)?,
                    outcome_len: row.get(7)?, raw_outcome: row.get(8)?, outcome: None, active: None, latest: None })
            }).map_err(db_error)?;
        snapshot.outcome = outcome::read(db, project)?;
        snapshot.active = SyncEvidence::read(db, project, true, bound)?;
        snapshot.latest = SyncEvidence::read(db, project, false, bound)?;
        tx.commit().map_err(db_error)?;
        Ok(snapshot)
    }
    fn reason(&self, health: IndexHealth, recipe: &str) -> Option<ReadinessReason> {
        use ReadinessReason as R;
        if self.active.is_some() || self.project.status == "indexing" {
            return Some(R::Indexing);
        }
        if self.latest.as_ref().is_some_and(SyncEvidence::interrupted) {
            return Some(R::Interrupted);
        }
        if self.project.status == "error" {
            return Some(R::Failed);
        }
        if self
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.stats.errors > 0)
        {
            return Some(R::Partial);
        }
        if self
            .latest
            .as_ref()
            .is_some_and(|job| job.state == SyncState::Error || job.known_partial())
        {
            return Some(R::SyncFailed);
        }
        if health != IndexHealth::Healthy {
            return Some(match health {
                IndexHealth::RebuildRequired => R::RebuildRequired,
                _ => R::IndexUnavailable,
            });
        }
        if self.project.status != "indexed" {
            return Some(R::IndexUnavailable);
        }
        if self.schema_version != i64::from(migrations::SCHEMA_VERSION) {
            return Some(R::RebuildRequired);
        }
        let Some(outcome) = &self.outcome else {
            return Some(R::UnknownOutcome);
        };
        if outcome.scope != SurveyScope::FullSurvey {
            return Some(R::IncompleteScope);
        }
        if outcome.recipe_id != recipe {
            return Some(R::RecipeChanged);
        }
        if self
            .latest
            .as_ref()
            .is_some_and(|job| !job.attributable_success(self))
        {
            return Some(R::UnknownOutcome);
        }
        None
    }
}

/// Inspect the current fixed project using prepared readers and its existing
/// read/mutation admission. No registration, providers, repair, writes or retry.
///
/// # Errors
/// Returns existing admission/metadata errors, including a busy writer or pool.
pub fn observe(
    pool: &ReadPool,
    project: &Project,
    config: &Config,
    notice: Option<String>,
) -> Result<ServerStatus> {
    lifecycle::with_existing_project_read_lease(pool.cache_root(), project.id, || {
        observe_with(
            project,
            &outcome::recipe_id(config),
            notice,
            || {
                let reader = pool.try_acquire()?;
                Snapshot::read(&reader, project.id)
            },
            || {
                let dir = lifecycle::index_dir(pool.cache_root(), project.id);
                match ReadIndex::open(&dir) {
                    Ok(_) => IndexHealth::Healthy,
                    Err(Error::Project { .. }) if dir.join("meta.json").exists() => {
                        IndexHealth::RebuildRequired
                    }
                    Err(_) => IndexHealth::IndexUnavailable,
                }
            },
        )
    })
}

fn observe_with(
    project: &Project,
    recipe: &str,
    notice: Option<String>,
    mut metadata: impl FnMut() -> Result<Snapshot>,
    health: impl FnOnce() -> IndexHealth,
) -> Result<ServerStatus> {
    let first = metadata()?;
    let index_health = health();
    let current = metadata()?;
    let reason = if first != current
        || current.project.id != project.id
        || current.project.root != project.canonical_root.to_string_lossy()
    {
        Some(ReadinessReason::StateChanged)
    } else {
        current.reason(index_health, recipe)
    };
    Ok(ServerStatus {
        schema_version: 1,
        profile: "website".into(),
        project: current.project,
        index_generation: current.generation,
        index_attempt: current.attempt,
        index_health,
        outcome: current.outcome,
        active_sync: current.active.as_ref().map(SyncEvidence::view),
        latest_sync: current.latest.as_ref().map(SyncEvidence::view),
        ready: reason.is_none(),
        reason,
        restart_required: notice.is_some(),
        notice,
    })
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
