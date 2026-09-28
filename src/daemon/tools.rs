//! Tool implementations shared by the daemon and the direct CLI mode so both
//! return identical payloads (FR-20…FR-26, FR-30).

use std::path::Path;

use serde_json::{Value, json};

use crate::config::Config;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_full};
use crate::index::tantivy_index::{ReadIndex, chunk_id_parts};
use crate::store::models::{Project, ProjectStatus};
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

/// `status`: registry-wide snapshot with runtime counters (FR-26).
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn status(db: &Db, sessions: u64, fd_count: u64) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        let counts = repo::project_counts(db.connection(), project.id)?;
        projects.push(json!({
            "id": project.id,
            "name": project.name,
            "root": project.canonical_root,
            "status": project.status.as_str(),
            "docs": counts.docs,
            "chunks": counts.chunks,
            "last_indexed_at": project.last_indexed_at,
        }));
    }
    Ok(json!({
        "schema_version": db.schema_version()?,
        "projects": projects,
        "sessions": sessions,
        "fd_count": fd_count,
    }))
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
    registry::set_status(db, project.id, ProjectStatus::Indexing)?;
    let result = lifecycle::with_writer_lease(cache, project.id, || {
        let mut index = crate::index::tantivy_index::IndexHandle::open_or_create(
            &lifecycle::index_dir(cache, project.id),
        )?;
        run_full(db, &mut index, project, config)
    });
    match result {
        Ok(stats) => {
            registry::set_status(db, project.id, ProjectStatus::Indexed)?;
            Ok(stats)
        }
        Err(err) => {
            if let Err(status_err) = registry::set_status(db, project.id, ProjectStatus::Error) {
                eprintln!(
                    "warning: cannot mark project {} as error: {status_err}",
                    project.id
                );
            }
            Err(err)
        }
    }
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
    Ok(())
}
