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
    // Direct mode has no daemon: sessions/fd/threads are not applicable.
    // Direct mode reloads the config every run, so there is no stale snapshot
    // to warn about (OQ-6).
    let value = crate::daemon::tools::status(
        &db,
        &crate::daemon::session::Stats::default(),
        0,
        None,
        &|_| false,
    )?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Runs `docsbase list`, printing documents of the current project (FR-25).
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed or the
/// snapshot cannot be opened.
pub fn list() -> anyhow::Result<()> {
    const PAGE: usize = 500;
    let mut cursor: Option<String> = None;
    let mut docs: Vec<serde_json::Value> = Vec::new();
    let mut project_name: Option<String> = None;
    // Bound the loop so a broken cursor cannot spin forever.
    for _ in 0..10_000 {
        let args = match &cursor {
            Some(cursor) => serde_json::json!({ "limit": PAGE, "cursor": cursor }),
            None => serde_json::json!({ "limit": PAGE }),
        };
        let daemon_value = crate::cli::try_daemon("list_docs", args.clone(), true)?;
        let value = if let Some(value) = daemon_value {
            value
        } else {
            let (db, project) = read_project()?;
            let value = crate::daemon::tools::list_docs(&db, &project, &args)?;
            project_name.get_or_insert_with(|| project.name.clone());
            value
        };
        if let Some(name) = value.get("project").and_then(serde_json::Value::as_str) {
            project_name = Some(name.to_owned());
        }
        if let Some(items) = value.get("docs").and_then(serde_json::Value::as_array) {
            docs.extend(items.iter().cloned());
        }
        cursor = value
            .get("next_cursor")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let value = serde_json::json!({
        "project": project_name,
        "docs": docs,
        "next_cursor": serde_json::Value::Null,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
