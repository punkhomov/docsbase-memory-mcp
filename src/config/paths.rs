//! XDG paths for the runtime layout (design §3).
//!
//! `DOCSBASE_CACHE_DIR` / `DOCSBASE_CONFIG_DIR` override XDG discovery; this is
//! what integration tests use to isolate the daemon from the real user dirs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use directories::ProjectDirs;
use fd_lock::RwLock as FdRwLock;

use crate::error::{Error, Result};

const APP: &str = "docsbase-memory-mcp";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP)
        .ok_or_else(|| Error::internal("cannot determine XDG directories (no home?)"))
}

/// Cache root (`$XDG_CACHE_HOME/docsbase-memory-mcp`), overridable via
/// `DOCSBASE_CACHE_DIR`.
///
/// # Errors
/// Returns [`Error::Internal`] when neither the override nor XDG discovery
/// yields a path.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCSBASE_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    project_dirs().map(|dirs| dirs.cache_dir().to_path_buf())
}

/// Data directory (`$XDG_DATA_HOME/docsbase-memory-mcp`), overridable via
/// `DOCSBASE_DATA_DIR`; owns the installed binary and the install manifest.
///
/// # Errors
/// Returns [`Error::Internal`] when neither the override nor XDG discovery
/// yields a path.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCSBASE_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    project_dirs().map(|dirs| dirs.data_dir().to_path_buf())
}

/// Config directory (`$XDG_CONFIG_HOME/docsbase-memory-mcp`), overridable via
/// `DOCSBASE_CONFIG_DIR`.
///
/// # Errors
/// Returns [`Error::Internal`] when neither the override nor XDG discovery
/// yields a path.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCSBASE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    project_dirs().map(|dirs| dirs.config_dir().to_path_buf())
}

/// Number of retries for the cross-process config-write lock (C13).
const CONFIG_LOCK_RETRIES: u32 = 50;

/// Delay between lock retries.
const CONFIG_LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Serializes read-modify-write of a config file across processes (review
/// panel C13): both `connect` and `models use --global` rewrite the whole
/// global `config.toml` with temp+rename, so without an inter-process lock
/// two concurrent writers can silently drop each other's sections.
///
/// Lock files live in our own data dir (`<data>/locks`), never next to the
/// target (round-4 C1): a project-scoped `docsbase models use` must not
/// leave a stray `.docsbase.toml.lock` in the user's project root, and
/// never-unlinking avoids the classic inode race (a waiter that opened the
/// old file would be bypassed by a fresh inode after removal). The id is a
/// stable `SipHash` of the target path; collisions are negligible and only
/// cause harmless extra serialization. Bounded retry, then an error.
///
/// # Errors
/// IO opening the lock file, or exhaustion of the retry budget (another
/// docsbase process is writing).
pub(crate) fn with_config_write_lock<T>(
    path: &Path,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let lock_dir = data_dir()?.join("locks");
    with_config_write_lock_in(&lock_dir, path, f)
}

/// `with_config_write_lock` with an explicit lock directory (tests pass a
/// `TempDir` instead of touching the real data dir).
fn with_config_write_lock_in<T>(
    lock_dir: &Path,
    path: &Path,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    std::fs::create_dir_all(lock_dir)
        .map_err(|err| anyhow::anyhow!("create {}: {err}", lock_dir.display()))?;
    let lock_path = config_lock_path(lock_dir, path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| anyhow::anyhow!("open {}: {err}", lock_path.display()))?;
    let mut lock = FdRwLock::new(file);
    for _ in 0..CONFIG_LOCK_RETRIES {
        match lock.try_write() {
            Ok(_guard) => return f(),
            Err(_) => std::thread::sleep(CONFIG_LOCK_RETRY_DELAY),
        }
    }
    Err(anyhow::anyhow!(
        "another docsbase process is writing {}; retry",
        path.display()
    ))
}

/// Deterministic lock-file name for `target` inside `lock_dir`: a stable
/// `SipHash` id over the target path (std `DefaultHasher`, fixed keys —
/// deterministic across processes; only used for file naming).
fn config_lock_path(lock_dir: &Path, target: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.as_os_str().hash(&mut hasher);
    lock_dir.join(format!("{:016x}.lock", hasher.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_write_lock_serializes_and_releases() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let lock_dir = dir.path().join("locks");
        // Project-scoped target (C1): the lock must not appear next to it.
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let path = project.join(".docsbase.toml");
        // Another writer (simulated) holds the exclusive lock.
        std::fs::create_dir_all(&lock_dir).expect("lock dir");
        let lock_path = config_lock_path(&lock_dir, &path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("lock open");
        let mut held = FdRwLock::new(file);
        let guard = held.try_write().expect("first writer holds");
        let err = with_config_write_lock_in(&lock_dir, &path, || Ok(()))
            .expect_err("contended write fails after bounded retries");
        assert!(
            err.to_string().contains("another docsbase process"),
            "{err}"
        );
        drop(guard);
        with_config_write_lock_in(&lock_dir, &path, || Ok(())).expect("succeeds after release");
        // C1: no stray sibling lock in the project root.
        assert!(
            !project.join(".docsbase.toml.lock").exists(),
            "no sibling lock file in the project root"
        );
    }
}
