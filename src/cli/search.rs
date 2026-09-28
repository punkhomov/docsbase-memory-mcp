//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use anyhow::Context;

use crate::cli::index::index_dir;
use crate::cli::status::{ensure_indexed, read_project};
use crate::config::paths;
use crate::index::tantivy_index::{ReadIndex, chunk_id_parts};
use crate::store::repo;

/// Runs `docsbase search <query> [--limit N]`, printing a JSON array of
/// `{path, heading_path, lines, score}` (FR-20).
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed, the snapshot
/// cannot be opened, or the query is invalid.
pub fn run(query: &str, limit: usize) -> anyhow::Result<()> {
    let (db, project) = read_project()?;
    ensure_indexed(&project)?;
    let cache = paths::cache_dir()?;
    let index =
        ReadIndex::open(&index_dir(&cache, project.id)).context("open read-only index snapshot")?;
    let hits = index.search(query, limit)?;

    let mut rows = Vec::with_capacity(hits.len());
    for hit in hits {
        let (doc_id, seq) = chunk_id_parts(hit.chunk_id);
        if let Some(citation) = repo::citation_for(db.connection(), doc_id, seq)? {
            rows.push(serde_json::json!({
                "path": citation.path,
                "heading_path": citation.heading_path,
                "lines": [citation.line_start, citation.line_end],
                "score": hit.score,
            }));
        }
    }
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
