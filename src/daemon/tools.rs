//! Tool implementations shared by the daemon and the direct CLI mode so both
//! return identical payloads (FR-20…FR-26, FR-30).

use std::path::{Component, Path};

use serde_json::{Value, json};

use crate::config::Config;
use crate::daemon::session::Stats;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_full};
use crate::index::tantivy_index::{MAX_HITS, ReadIndex, chunk_id, chunk_id_parts};
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
    // Client-supplied limits are clamped: the top-k collector allocates
    // proportional to the request (final review C1).
    let limit = args.get("limit").and_then(Value::as_u64).map_or(10, |v| {
        usize::try_from(v).unwrap_or(usize::MAX).min(MAX_HITS)
    });

    let index = ReadIndex::open(&lifecycle::index_dir(cache, project.id))?;
    let hits = index.search(query, limit)?;
    let mut rows = Vec::with_capacity(hits.len());
    for hit in hits {
        let (doc_id, seq) = chunk_id_parts(hit.chunk_id);
        if let Some(citation) = repo::citation_for(db.connection(), doc_id, seq)? {
            rows.push(json!({
                "chunk_id": hit.chunk_id,
                "path": citation.path,
                "heading_path": citation.heading_path,
                "lines": [citation.line_start, citation.line_end],
                "score": hit.score,
            }));
        }
    }
    Ok(Value::Array(rows))
}

/// `list_docs`: keyset-paginated documents of one project (FR-25).
///
/// `limit` defaults to 50; `cursor` is the `next_cursor` of the previous page.
/// A stable cursor is the last `rel_path` seen, so late inserts before it do
/// not shift the following pages.
///
/// # Errors
/// Returns [`Error::Project`] when the project is not indexed and
/// [`Error::Protocol`] for malformed arguments.
pub fn list_docs(db: &Db, project: &Project, args: &Value) -> Result<Value> {
    ensure_indexed(project)?;
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 50,
        Some(value) => {
            value
                .as_u64()
                .filter(|limit| *limit > 0)
                .ok_or_else(|| Error::Protocol {
                    message: format!("list_docs: limit must be a positive integer, got {value}"),
                })?
        }
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let cursor = match args.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(other) => {
            return Err(Error::Protocol {
                message: format!("list_docs: cursor must be a string, got {other}"),
            });
        }
    };
    let mut docs =
        repo::docs_overview_page(db.connection(), project.id, cursor, limit.saturating_add(1))?;
    let next_cursor = if docs.len() > limit {
        let last = docs[limit - 1].path.clone();
        docs.truncate(limit);
        Some(last)
    } else {
        None
    };
    let docs: Vec<Value> = docs
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
    Ok(json!({ "project": project.name, "docs": docs, "next_cursor": next_cursor }))
}

/// `get_doc`: full document text for a path strictly inside the project root
/// (FR-23, FR-32; I4).
///
/// # Errors
/// Returns [`Error::Project`] when the path is absolute, escapes the root,
/// resolves through a symlink outside it, is not a markdown file, or cannot
/// be read.
pub fn get_doc(project: &Project, args: &Value) -> Result<Value> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol {
            message: "get_doc: missing path".to_owned(),
        })?;
    let escape = || Error::Project {
        message: format!("path {raw:?} must stay inside the project root"),
        instruction: Some("pass a relative path of a markdown file".to_owned()),
    };
    if raw.is_empty() {
        return Err(escape());
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(escape());
    }
    let outside_root = || Error::Project {
        message: format!(
            "path {raw:?} resolves outside the project root {}",
            project.canonical_root.display()
        ),
        instruction: Some("symlinks leaving the root are rejected (FR-32)".to_owned()),
    };
    let joined = project.canonical_root.join(path);
    let canonical = joined.canonicalize().map_err(|err| Error::Project {
        message: format!("cannot open {raw:?}: {err}"),
        instruction: Some("check the path exists inside the project root".to_owned()),
    })?;
    if !crate::platform::paths::is_under(&canonical, &project.canonical_root) {
        return Err(outside_root());
    }
    if !canonical.is_file() {
        return Err(Error::Project {
            message: format!("{raw:?} is not a file"),
            instruction: None,
        });
    }
    if !canonical
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err(Error::Project {
            message: format!("{raw:?} is not a markdown document"),
            instruction: Some("only `.md` documents are exposed".to_owned()),
        });
    }
    let content = std::fs::read_to_string(&canonical).map_err(|err| Error::Project {
        message: format!("cannot read {}: {err}", canonical.display()),
        instruction: None,
    })?;
    let rel = canonical
        .strip_prefix(&project.canonical_root)
        .unwrap_or(Path::new(raw))
        .to_string_lossy()
        .into_owned();
    Ok(json!({
        "path": rel,
        "content": content,
        "size": content.len(),
        "lines": content.lines().count(),
    }))
}

/// `read_neighbors`: chunks around `chunk_id` inside its document (FR-24).
///
/// `before`/`after` default to 1 and are capped at [`NEIGHBOR_CAP`] each.
///
/// # Errors
/// Returns [`Error::Project`] for unknown chunk ids and [`Error::Protocol`]
/// for malformed arguments.
pub fn read_neighbors(db: &Db, project: &Project, args: &Value) -> Result<Value> {
    ensure_indexed(project)?;
    let raw = args
        .get("chunk_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Protocol {
            message: format!(
                "read_neighbors: chunk_id must be a non-negative integer, got {:?}",
                args.get("chunk_id")
            ),
        })?;
    let side = |key: &str| -> Result<i64> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(1),
            Some(value) => {
                let parsed = value.as_u64().ok_or_else(|| Error::Protocol {
                    message: format!(
                        "read_neighbors: {key} must be a non-negative integer, got {value}"
                    ),
                })?;
                Ok(i64::try_from(parsed.min(NEIGHBOR_CAP)).unwrap_or(i64::MAX))
            }
        }
    };
    let before = side("before")?;
    let after = side("after")?;
    let (doc_id, seq) = chunk_id_parts(raw);
    let from = i64::from(seq).saturating_sub(before);
    let to = i64::from(seq).saturating_add(after);
    let rows = repo::chunk_window(db.connection(), project.id, doc_id, from, to)?;
    if !rows.iter().any(|row| row.seq == seq) {
        return Err(Error::Project {
            message: format!("unknown chunk_id {raw} for project '{}'", project.name),
            instruction: Some("use search_docs to obtain chunk ids".to_owned()),
        });
    }
    let chunks: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "chunk_id": chunk_id(doc_id, row.seq),
                "seq": row.seq,
                "path": row.path,
                "heading_path": row.heading_path,
                "lines": [row.line_start, row.line_end],
                "kind": row.kind,
                "text": row.text,
            })
        })
        .collect();
    Ok(Value::Array(chunks))
}

/// Per-side cap for [`read_neighbors`] payloads.
pub const NEIGHBOR_CAP: u64 = 100;

/// `list_projects`: registry entries with statuses (FR-10, FR-25).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
pub fn list_projects(db: &Db) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        projects.push(project_entry(db, &project, None)?);
    }
    Ok(Value::Array(projects))
}

/// `status`: registry-wide snapshot with runtime counters and versions (FR-26).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
pub fn status(
    db: &Db,
    stats: &Stats,
    watchers: u64,
    restart_notice: Option<&str>,
    watching: &dyn Fn(i64) -> bool,
) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        projects.push(project_entry(db, &project, Some(watching(project.id)))?);
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
        "restart_required": restart_notice.is_some(),
        "notice": restart_notice,
    }))
}

fn project_entry(db: &Db, project: &Project, watched: Option<bool>) -> Result<Value> {
    let counts = repo::project_counts(db.connection(), project.id)?;
    // Warnings come from the newest job only when it succeeded: a newer
    // failed job must not leave an older job's warnings looking current.
    let warnings = match repo::latest_job(db.connection(), project.id)? {
        Some((state, Some(raw))) if state == "done" => serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|stats| stats.get("warnings").cloned())
            .unwrap_or_else(|| Value::Array(Vec::new())),
        _ => Value::Array(Vec::new()),
    };
    let mut entry = json!({
        "id": project.id,
        "name": project.name,
        "root": project.canonical_root,
        "status": project.status.as_str(),
        "docs": counts.docs,
        "chunks": counts.chunks,
        "last_indexed_at": project.last_indexed_at,
        "warnings": warnings,
    });
    if let Some(watched) = watched {
        entry["watched"] = json!(watched);
    }
    Ok(entry)
}

/// Creates a `queued` sync job for `project`; when an active job already
/// exists, returns its id with `false` (FR-17).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
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
pub fn execute_sync_job(
    cache: &Path,
    job_id: i64,
    project_id: i64,
    config: &Config,
) -> Result<Project> {
    let mut db = Db::open(cache)?;
    let setup = (|| -> Result<Project> {
        registry::project_by_id(&db, project_id)?.ok_or_else(|| Error::Project {
            message: format!("project {project_id} disappeared from the registry"),
            instruction: None,
        })
    })();
    let outcome = match setup {
        Ok(project) => run_sync_job_in(&mut db, cache, job_id, &project, config),
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
