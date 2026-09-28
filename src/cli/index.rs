//! `docsbase index` — direct indexing with the per-project writer lease
//! (FR-11, FR-30; I5).

use std::path::Path;

use anyhow::Context;

use crate::config::{Config, paths};
use crate::daemon::registry;
use crate::daemon::tools;
use crate::store::Db;

/// Runs `docsbase index [path]`, printing `{"project_id":…,"stats":…}`.
///
/// # Errors
/// Returns an error when the path/config/database cannot be opened, another
/// writer holds the project lease (I5), or indexing fails.
pub fn run(path: Option<&Path>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let target = path.map_or(cwd, Path::to_path_buf);
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve {}", target.display()))?;

    let mut db = Db::open(&paths::cache_dir()?)?;
    let project = registry::ensure_project(&mut db, &target)?;
    let config = Config::load(Some(&project.canonical_root))?;
    let cache_root = db.cache_root().to_path_buf();
    let stats = tools::run_project_index(&mut db, &cache_root, &project, &config)?;

    let payload = serde_json::json!({ "project_id": project.id, "stats": stats });
    println!("{payload}");
    Ok(())
}
