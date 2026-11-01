//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use crate::cli::status::read_project;
use crate::config::Config;
use crate::config::paths;
use crate::daemon::tools;

/// Runs `docsbase search <query> [--limit N] [--explain] [--snippet] [--dedupe …]`,
/// printing a JSON array
/// of `{path, heading_path, lines, score}` (FR-20) or, with `--explain`, the
/// `{hits, meta}` envelope (spec `search-telemetry`, FR-1). Hits whose citation row
/// vanished during an R2 crash window are dropped from the snapshot.
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed, the snapshot
/// cannot be opened, or the query is invalid.
pub struct Options {
    /// Maximum number of hits.
    pub limit: usize,
    /// Return `{hits, meta}` instead of the bare array.
    pub explain: bool,
    /// Attach a text fragment to each hit.
    pub snippet: bool,
    /// `none` or `path`.
    pub dedupe: String,
    /// Chunks kept per document (dedupe only).
    pub n_per_doc: Option<u64>,
    /// Project-relative path prefix.
    pub path_prefix: Option<String>,
    /// Required frontmatter tags (AND).
    pub tags: Vec<String>,
}

impl Options {
    /// The MCP argument object, so both seams send exactly the same request.
    fn args(&self, query: &str) -> serde_json::Value {
        let mut args = serde_json::json!({
            "query": query,
            "limit": self.limit,
            "explain": self.explain,
            "snippet": self.snippet,
            "dedupe": self.dedupe,
        });
        if let Some(n) = self.n_per_doc {
            args["n_per_doc"] = serde_json::json!(n);
        }
        if let Some(prefix) = &self.path_prefix {
            args["path_prefix"] = serde_json::json!(prefix);
        }
        if !self.tags.is_empty() {
            args["tags"] = serde_json::json!(self.tags);
        }
        args
    }
}

///
/// # Errors
/// Returns an error when the project is unregistered or not indexed, the
/// snapshot cannot be opened, or the query or an argument is invalid.
pub fn run(query: &str, options: &Options) -> anyhow::Result<()> {
    if let Some(value) = crate::cli::try_daemon("search_docs", options.args(query), true)? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let (db, project) = read_project()?;
    let cache = paths::cache_dir()?;
    // Direct mode honors the project's hybrid configuration exactly like the
    // daemon route: stages resolved up front (explicit admission on failure,
    // FR-14) and the same `search_docs_with` seam (FR-11/12).
    let config = Config::load(Some(&project.canonical_root))?;
    let stages = crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)?;
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
        //
        // Direct mode forwards every search argument, `explain` included: it used
        // to drop it, so `--explain` worked only with a daemon running
        // (search-ux FR-8, T0).
        &options.args(query),
        window,
    )?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
