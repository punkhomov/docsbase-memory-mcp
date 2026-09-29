//! Tool implementations shared by the daemon and the direct CLI mode so both
//! return identical payloads (FR-20…FR-26, FR-30).

use std::path::Path;

use serde_json::{Value, json};

use crate::config::Config;
use crate::daemon::session::Stats;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_full};
use crate::index::tantivy_index::{ReadIndex, chunk_id_parts};
use crate::ipc::protocol::{PROTOCOL_VERSION, build_id};
use crate::store::migrations;
use crate::store::models::{Project, ProjectStatus, SyncJob, SyncState};
use crate::store::{Db, repo};

/// `search_docs`: top-k chunks with citation (FR-20).
///
/// # Errors
/// Returns [`Error::Query`] for invalid queries and [`Error::Project`] when
/// the project is not indexed.
pub fn search_docs(db: &Db, cache: &Path, project: &Project, args: &Value) -> Result<Value> {
    ensure_indexed(project)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol {
            message: "search_docs: missing query".to_owned(),
        })?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(10, |v| usize::try_from(v).unwrap_or(usize::MAX));

    let index = ReadIndex::open(&lifecycle::index_dir(cache, project.id))?;
    let hits = index.search(query, limit)?;
    let mut rows = Vec::with_capacity(hits.len());
    for hit in hits {
        let (doc_id, seq) = chunk_id_parts(hit.chunk_id);
        if let Some(citation) = repo::citation_for(db.connection(), doc_id, seq)? {
            rows.push(json!({
                "path": citation.path,
                "heading_path": citation.heading_path,
                "lines": [citation.line_start, citation.line_end],
                "score": hit.score,
            }));
        }
    }
    Ok(Value::Array(rows))
}

/// `list_docs`: documents of one project (FR-25).
///
/// # Errors
/// Returns [`Error::Project`] when the project is not indexed.
pub fn list_docs(db: &Db, project: &Project) -> Result<Value> {
    ensure_indexed(project)?;
    let docs: Vec<Value> = repo::docs_overview(db.connection(), project.id)?
        .into_iter()
        .map(|doc| {
            json!({
                "path": doc.path,
                "title": doc.title,
                "size": doc.size,
                "chunks": doc.chunks,
            })
        })
        .collect();
    Ok(json!({ "project": project.name, "docs": docs }))
}

/// `list_projects`: registry entries with statuses (FR-10, FR-25).
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn list_projects(db: &Db) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        projects.push(project_entry(db, &project)?);
    }
    Ok(Value::Array(projects))
}

/// `status`: registry-wide snapshot with runtime counters and versions (FR-26).
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn status(db: &Db, stats: &Stats, watchers: u64) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        projects.push(project_entry(db, &project)?);
    }
    let hint = projects
        .is_empty()
        .then_some("run `docsbase index` in your project");
    Ok(json!({
        "protocol_version": PROTOCOL_VERSION,
        "build_id": build_id(),
        "schema_version": db.schema_version()?,
        "projects": projects,
        "sessions": stats.sessions,
        "fd_count": stats.fd_count,
        "threads": stats.threads,
        "watchers": watchers,
        "hint": hint,
    }))
}

fn project_entry(db: &Db, project: &Project) -> Result<Value> {
    let counts = repo::project_counts(db.connection(), project.id)?;
    Ok(json!({
        "id": project.id,
        "name": project.name,
        "root": project.canonical_root,
        "status": project.status.as_str(),
        "docs": counts.docs,
        "chunks": counts.chunks,
        "last_indexed_at": project.last_indexed_at,
    }))
}

/// Creates a `queued` sync job for `project`; when an active job already
/// exists, returns its id with `false` (FR-17).
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn enqueue_sync(db: &Db, project: &Project) -> Result<(i64, bool)> {
    for _ in 0..3 {
        if let Some(id) = repo::create_sync_job(db.connection(), project.id)? {
            return Ok((id, true));
        }
        // The blocking insert lost to an active job; it may finish before the
        // select (its runner uses another connection), so retry the pair.
        if let Some(active) = repo::active_sync_job(db.connection(), project.id)? {
            return Ok((active.id, false));
        }
    }
    Err(Error::internal(format!(
        "sync jobs for project {} are racing repeatedly",
        project.id
    )))
}

/// Runs a queued job to completion on its own database connection (daemon
/// background task, FR-17); job state survives restarts.
///
/// Setup failures (missing project, unreadable config) also record `error`,
/// so the project never keeps a stuck `queued` job that would block FR-17
/// syncs; only an unopenable registry leaves the row untouched.
///
/// # Errors
/// Propagates setup and indexing errors after persisting them.
pub fn execute_sync_job(cache: &Path, job_id: i64, project_id: i64) -> Result<Project> {
    let mut db = Db::open(cache)?;
    let setup = (|| -> Result<(Project, Config)> {
        let project = registry::project_by_id(&db, project_id)?.ok_or_else(|| Error::Project {
            message: format!("project {project_id} disappeared from the registry"),
            instruction: None,
        })?;
        let config = Config::load(Some(&project.canonical_root))?;
        Ok((project, config))
    })();
    let outcome = match setup {
        Ok((project, config)) => run_sync_job_in(&mut db, cache, job_id, &project, &config),
        Err(err) => {
            let stats = json!({ "error": err.to_string() }).to_string();
            if let Err(record) =
                repo::finish_sync_job(db.connection(), job_id, SyncState::Error, Some(&stats))
            {
                eprintln!("warning: cannot record sync job {job_id} failure: {record}");
            }
            Err(err)
        }
    };
    let final_project =
        registry::project_by_id(&db, project_id)?.ok_or_else(|| Error::Project {
            message: format!("project {project_id} disappeared after job {job_id}"),
            instruction: None,
        })?;
    outcome.map(|_| final_project)
}

/// Marks `job_id` running, runs the full index and records the terminal
/// state with statistics (FR-17).
///
/// # Errors
/// Propagates indexing errors after persisting them in `sync_jobs`.
pub fn run_sync_job_in(
    db: &mut Db,
    cache: &Path,
    job_id: i64,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    repo::mark_sync_running(db.connection(), job_id)?;
    let outcome = run_project_index(db, cache, project, config);
    let (state, stats_json) = match &outcome {
        Ok(stats) => (SyncState::Done, serde_json::to_string(stats).ok()),
        Err(err) => (
            SyncState::Error,
            Some(json!({ "error": err.to_string() }).to_string()),
        ),
    };
    if let Err(finish_err) =
        repo::finish_sync_job(db.connection(), job_id, state, stats_json.as_deref())
    {
        eprintln!("warning: cannot finish sync job {job_id}: {finish_err}");
    }
    outcome
}

/// `sync_status(job_id)`: persistent job state and statistics (FR-17).
///
/// # Errors
/// Returns [`Error::Protocol`] when `job_id` is missing and
/// [`Error::Project`] for an unknown id.
pub fn sync_status(db: &Db, args: &Value) -> Result<Value> {
    let job_id = match args.get("job_id") {
        Some(value) => value.as_i64().ok_or_else(|| Error::Protocol {
            message: format!("sync_status: job_id must be an integer, got {value}"),
        })?,
        None => {
            return Err(Error::Protocol {
                message: "sync_status: missing job_id".to_owned(),
            });
        }
    };
    let job = repo::sync_job(db.connection(), job_id)?.ok_or_else(|| Error::Project {
        message: format!("unknown sync job id {job_id}"),
        instruction: None,
    })?;
    Ok(sync_job_value(&job))
}

/// JSON view of a sync job row.
#[must_use]
pub fn sync_job_value(job: &SyncJob) -> Value {
    json!({
        "job_id": job.id,
        "project_id": job.project_id,
        "state": job.state.as_str(),
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "stats": job
            .stats_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
    })
}

/// Registers (by the caller) and indexes `project` under the per-project
/// writer lease, updating statuses (FR-11, I5).
///
/// # Errors
/// Propagates indexing and status errors.
pub fn run_project_index(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    lifecycle::with_writer_lease(cache, project.id, || {
        registry::set_status(db, project.id, ProjectStatus::Indexing)?;
        let outcome = (|| -> Result<JobStats> {
            let mut index = crate::index::tantivy_index::IndexHandle::open_or_create(
                &lifecycle::index_dir(cache, project.id),
            )?;
            run_full(db, &mut index, project, config)
        })();
        match outcome {
            Ok(stats) => {
                registry::mark_indexed(db, project.id)?;
                Ok(stats)
            }
            Err(err) => {
                if let Err(status_err) = registry::set_status(db, project.id, ProjectStatus::Error)
                {
                    eprintln!(
                        "warning: cannot mark project {} as error: {status_err}",
                        project.id
                    );
                }
                Err(err)
            }
        }
    })
}

fn ensure_indexed(project: &Project) -> Result<()> {
    if project.status != ProjectStatus::Indexed {
        return Err(Error::Project {
            message: format!(
                "project '{}' is not indexed (status: {})",
                project.name,
                project.status.as_str()
            ),
            instruction: Some("run `docsbase index`".to_owned()),
        });
    }
    if project.schema_version != migrations::SCHEMA_VERSION {
        return Err(Error::Project {
            message: format!(
                "project '{}' indexes use schema {} while this build expects {}",
                project.name,
                project.schema_version,
                migrations::SCHEMA_VERSION
            ),
            instruction: Some("run `docsbase index` to rebuild".to_owned()),
        });
    }
    Ok(())
}
