//! `docsbase list` / `docsbase status` — read-only registry snapshots
//! (FR-25, FR-26, FR-30).

use crate::config::paths;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::store::models::{Project, ProjectStatus};
use crate::store::{DB_FILE, Db, repo};

/// Resolves the current directory to a registered project, read-only.
///
/// # Errors
/// Returns [`Error::Project`] when the registry is missing or the cwd is not
/// covered by a registered root (C8).
pub(crate) fn read_project() -> Result<(Db, Project)> {
    let cache = paths::cache_dir()?;
    if !cache.join(DB_FILE).exists() {
        return Err(Error::Project {
            message: "no projects are registered yet".to_owned(),
            instruction: Some("run `docsbase index` in your project".to_owned()),
        });
    }
    let db = Db::open_readonly(&cache)?;
    let cwd = std::env::current_dir().map_err(|err| {
        Error::internal_with_source(format!("resolve current directory: {err}"), err)
    })?;
    let project = registry::resolve_by_cwd(&db, &cwd)?;
    Ok((db, project))
}

/// Refuses read commands for projects that have no usable index (C8).
pub(crate) fn ensure_indexed(project: &Project) -> Result<()> {
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

/// Runs `docsbase status`, printing registry-wide JSON (FR-26).
///
/// # Errors
/// Returns an error when the read-only snapshot cannot be opened.
pub fn status() -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon("status", serde_json::json!({}))? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let cache = paths::cache_dir()?;
    if !cache.join(DB_FILE).exists() {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": null,
                "projects": [],
                "hint": "run `docsbase index` in your project",
            })
        );
        return Ok(());
    }
    let db = Db::open_readonly(&cache)?;
    let schema_version = db.schema_version()?;
    let mut projects = Vec::new();
    for project in registry::list_projects(&db)? {
        let counts = repo::project_counts(db.connection(), project.id)?;
        projects.push(serde_json::json!({
            "id": project.id,
            "name": project.name,
            "root": project.canonical_root,
            "status": project.status.as_str(),
            "docs": counts.docs,
            "chunks": counts.chunks,
            "last_indexed_at": project.last_indexed_at,
        }));
    }
    let hint = projects
        .is_empty()
        .then_some("run `docsbase index` in your project");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": schema_version,
            "projects": projects,
            "hint": hint,
        }))?
    );
    Ok(())
}

/// Runs `docsbase list`, printing documents of the current project (FR-25).
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed or the
/// snapshot cannot be opened.
pub fn list() -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon("list_docs", serde_json::json!({}))? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let (db, project) = read_project()?;
    ensure_indexed(&project)?;
    let docs: Vec<serde_json::Value> = repo::docs_overview(db.connection(), project.id)?
        .into_iter()
        .map(|doc| {
            serde_json::json!({
                "path": doc.path,
                "title": doc.title,
                "size": doc.size,
                "chunks": doc.chunks,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "project": project.name,
            "docs": docs,
        }))?
    );
    Ok(())
}
