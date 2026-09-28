//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use crate::cli::status::read_project;
use crate::config::paths;
use crate::daemon::tools;

/// Runs `docsbase search <query> [--limit N]`, printing a JSON array of
/// `{path, heading_path, lines, score}` (FR-20). Hits whose citation row
/// vanished during an R2 crash window are dropped from the snapshot.
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed, the snapshot
/// cannot be opened, or the query is invalid.
pub fn run(query: &str, limit: usize) -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon(
        "search_docs",
        serde_json::json!({ "query": query, "limit": limit }),
        true,
    )? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let (db, project) = read_project()?;
    let cache = paths::cache_dir()?;
    let value = tools::search_docs(
        &db,
        &cache,
        &project,
        &serde_json::json!({ "query": query, "limit": limit }),
    )?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
