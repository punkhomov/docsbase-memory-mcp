//! `docsbase index` — direct indexing with the per-project writer lease
//! (FR-11, FR-30; I5).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fd_lock::RwLock;

use crate::config::{Config, paths};
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_full};
use crate::index::tantivy_index::IndexHandle;
use crate::store::Db;
use crate::store::models::{Project, ProjectStatus};

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
    let stats = with_writer_lease(&cache_root, project.id, || {
        registry::set_status(&db, project.id, ProjectStatus::Indexing)?;
        match run_index(&mut db, &project, &config) {
            Ok(stats) => {
                registry::set_status(&db, project.id, ProjectStatus::Indexed)?;
                Ok(stats)
            }
            Err(err) => {
                if let Err(status_err) = registry::set_status(&db, project.id, ProjectStatus::Error)
                {
                    eprintln!(
                        "warning: cannot mark project {} as error: {status_err}",
                        project.id
                    );
                }
                Err(err)
            }
        }
    })?;

    let payload = serde_json::json!({ "project_id": project.id, "stats": stats });
    println!("{payload}");
    Ok(())
}

fn run_index(db: &mut Db, project: &Project, config: &Config) -> Result<JobStats> {
    let mut index = IndexHandle::open_or_create(&index_dir(db.cache_root(), project.id))?;
    run_full(db, &mut index, project, config)
}

/// Per-project index directory (design §3 runtime layout).
pub(crate) fn index_dir(cache_root: &Path, project_id: i64) -> PathBuf {
    cache_root
        .join("projects")
        .join(project_id.to_string())
        .join("tantivy")
}

/// Per-project writer lock file path (design §3 runtime layout).
pub(crate) fn lease_path(cache_root: &Path, project_id: i64) -> PathBuf {
    cache_root
        .join("projects")
        .join(project_id.to_string())
        .join(".writer.lock")
}

/// Runs `f` while holding the per-project writer lease; refuses when another
/// writer (daemon or CLI) owns it (I5).
fn with_writer_lease<T>(
    cache_root: &Path,
    project_id: i64,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let path = lease_path(cache_root, project_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_with_source(format!("create {}: {err}", parent.display()), err)
        })?;
    }
    let file = open_lock_file(&path)?;
    let mut lock = RwLock::new(file);
    let guard = match lock.try_write() {
        Ok(guard) => guard,
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(Error::Project {
                message: format!("another writer holds {}", path.display()),
                instruction: Some(
                    "stop `docsbase serve` or wait for the running index to finish".to_owned(),
                ),
            });
        }
        Err(err) => {
            return Err(Error::internal_with_source(
                format!("lock {}: {err}", path.display()),
                err,
            ));
        }
    };
    let result = f();
    drop(guard);
    result
}

fn open_lock_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|err| Error::internal_with_source(format!("open {}: {err}", path.display()), err))
}
