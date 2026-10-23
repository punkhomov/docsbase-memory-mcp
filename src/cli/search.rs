//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use crate::cli::status::read_project;
use crate::config::Config;
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
    // Direct mode honors the project's hybrid configuration exactly like the
    // daemon route: stages resolved up front (explicit admission on failure,
    // FR-14) and the same `search_docs_with` seam (FR-11/12).
    let config = Config::load(Some(&project.canonical_root))?;
    let stages = crate::hybrid::from_config(
        &config.hybrid_cfg,
        cfg!(feature = "hybrid-remote"),
        cfg!(feature = "hybrid-local"),
    )?;
    let window = config
        .hybrid_cfg
        .reranker
        .as_ref()
        .map_or(crate::hybrid::config::DEFAULT_RERANK_WINDOW, |reranker| {
            reranker.window
        });
    // Direct mode loads a persisted ANN index if one exists and never builds
    // one (FR-8, FR-5): a one-shot process must not pay for HNSW build
    // latency.
    let limits = crate::hybrid::dense::DenseCacheLimits {
        vector_cache_max_bytes: config.hybrid_cfg.cache.max_bytes,
        ..crate::hybrid::dense::DenseCacheLimits::default()
    };
    let dense = crate::hybrid::dense::DenseCache::with_policy_limits(
        crate::hybrid::dense::AnnPolicy::LoadOnly,
        &cache,
        limits,
    );
    let value = tools::search_docs_with(
        &stages,
        &dense,
        &db,
        &cache,
        &project,
        &serde_json::json!({ "query": query, "limit": limit }),
        window,
    )?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
