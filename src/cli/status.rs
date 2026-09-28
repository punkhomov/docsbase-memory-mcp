//! `docsbase list` / `docsbase status` — read-only registry snapshots
//! (FR-25, FR-26, FR-30).

use crate::config::paths;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::store::models::Project;
use crate::store::{DB_FILE, Db};

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

/// Runs `docsbase status`, printing registry-wide JSON (FR-26).
///
/// # Errors
/// Returns an error when the read-only snapshot cannot be opened.
pub fn status() -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon("status", serde_json::json!({}), false)? {
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
    let value = crate::daemon::tools::status(&db, 0, 0)?;
    let mut value = value;
    if value["projects"].as_array().is_some_and(Vec::is_empty) {
        value["hint"] = serde_json::json!("run `docsbase index` in your project");
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Runs `docsbase list`, printing documents of the current project (FR-25).
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed or the
/// snapshot cannot be opened.
pub fn list() -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon("list_docs", serde_json::json!({}), true)? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let (db, project) = read_project()?;
    let value = crate::daemon::tools::list_docs(&db, &project)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
