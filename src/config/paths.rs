//! XDG paths for the runtime layout (design §3).
//!
//! `DOCSBASE_CACHE_DIR` / `DOCSBASE_CONFIG_DIR` override XDG discovery; this is
//! what integration tests use to isolate the daemon from the real user dirs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use directories::ProjectDirs;
use fd_lock::RwLock as FdRwLock;

use super::bootstrap::{BootstrapContext, BootstrapValue};
use super::schema::SourceId;
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

pub(crate) fn config_lock_dir() -> Result<PathBuf> {
    lock_dir_in(&data_dir()?)
}

pub(crate) fn config_lock_dir_from(bootstrap: &BootstrapContext) -> Result<PathBuf> {
    lock_dir_in(&data_dir_from(bootstrap)?.value)
}

fn lock_dir_in(data_dir: &Path) -> Result<PathBuf> {
    std::path::absolute(data_dir.join("locks"))
        .map_err(|error| Error::internal_with_source("resolve configuration lock directory", error))
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

fn captured_dir(
    value: Option<&BootstrapValue<PathBuf>>,
    select: impl FnOnce(&ProjectDirs) -> &Path,
) -> Result<BootstrapValue<PathBuf>> {
    if let Some(value) = value {
        return Ok(value.clone());
    }
    Ok(BootstrapValue {
        value: select(&project_dirs()?).to_path_buf(),
        source: SourceId::Os,
    })
}

pub(crate) fn cache_dir_from(bootstrap: &BootstrapContext) -> Result<BootstrapValue<PathBuf>> {
    captured_dir(bootstrap.cache_dir.as_ref(), ProjectDirs::cache_dir)
}

pub(crate) fn data_dir_from(bootstrap: &BootstrapContext) -> Result<BootstrapValue<PathBuf>> {
    captured_dir(bootstrap.data_dir.as_ref(), ProjectDirs::data_dir)
}

pub(crate) fn config_dir_from(bootstrap: &BootstrapContext) -> Result<BootstrapValue<PathBuf>> {
    captured_dir(bootstrap.config_dir.as_ref(), ProjectDirs::config_dir)
}

pub(crate) fn daemon_exe_from(
    override_path: Option<&BootstrapValue<PathBuf>>,
) -> BootstrapValue<PathBuf> {
    if let Some(path) = override_path {
        return path.clone();
    }
    match std::env::current_exe() {
        Ok(value) => BootstrapValue {
            value,
            source: SourceId::Os,
        },
        Err(_) => BootstrapValue {
            value: PathBuf::from("docsbase"),
            source: SourceId::Defaults,
        },
    }
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
/// stable hash of the target's directory entry; collisions are negligible and only
/// cause harmless extra serialization. Bounded retry, then an error.
///
/// # Errors
/// IO opening the lock file, or exhaustion of the retry budget (another
/// docsbase process is writing).
pub(crate) fn with_config_write_lock<T>(
    path: &Path,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    with_config_write_lock_with_wait(path, f, std::thread::sleep)
}

/// Hold source guards in global-to-project order during index publication.
pub(crate) fn with_index_config_write_locks<T>(
    lock_dir: &Path,
    global_file: Option<&Path>,
    project_root: &Path,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let project_file = project_root.join(".docsbase.toml");
    let lock_project =
        || with_config_write_lock_in(lock_dir, &project_file, || Ok(work()?), std::thread::sleep);
    let result = if let Some(global_file) = global_file {
        with_config_write_lock_in(lock_dir, global_file, lock_project, std::thread::sleep)
    } else {
        lock_project()
    };
    result.map_err(|error| match error.downcast::<Error>() {
        Ok(error) => error,
        Err(error) => Error::Admission {
            message: format!("could not lock the index configuration: {error}"),
        },
    })
}

/// Per-call retry seam; production uses the same real backoff as before.
/// `wait` runs only after a failed OS-lock attempt, never while holding it.
pub(crate) fn with_config_write_lock_with_wait<T>(
    path: &Path,
    f: impl FnOnce() -> anyhow::Result<T>,
    wait: impl FnMut(Duration),
) -> anyhow::Result<T> {
    let lock_dir = config_lock_dir()?;
    with_config_write_lock_in(&lock_dir, path, f, wait)
}

/// `with_config_write_lock` with an explicit lock directory (tests pass a
/// `TempDir` instead of touching the real data dir).
fn with_config_write_lock_in<T>(
    lock_dir: &Path,
    path: &Path,
    f: impl FnOnce() -> anyhow::Result<T>,
    mut wait: impl FnMut(Duration),
) -> anyhow::Result<T> {
    std::fs::create_dir_all(lock_dir)
        .map_err(|err| anyhow::anyhow!("create {}: {err}", lock_dir.display()))?;
    let target = canonical_config_target(path)?;
    // Keep the original path key for an older editor using this spelling.
    // New callers with different spellings also share the canonical key.
    let mut keys = [
        config_lock_path(lock_dir, path),
        config_lock_path(lock_dir, &target),
    ];
    keys.sort_unstable();
    with_config_lock_file(&keys[0], path, &mut wait, |wait| {
        if keys[0] == keys[1] {
            f()
        } else {
            with_config_lock_file(&keys[1], path, wait, |_| f())
        }
    })
}

fn with_config_lock_file<T, W: FnMut(Duration)>(
    lock_path: &Path,
    target: &Path,
    wait: &mut W,
    work: impl FnOnce(&mut W) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|err| anyhow::anyhow!("open {}: {err}", lock_path.display()))?;
    let mut lock = FdRwLock::new(file);
    for _ in 0..CONFIG_LOCK_RETRIES {
        match lock.try_write() {
            Ok(_guard) => return work(wait),
            Err(_) => wait(CONFIG_LOCK_RETRY_DELAY),
        }
    }
    Err(anyhow::anyhow!(
        "another docsbase process is writing {}; retry",
        target.display()
    ))
}

/// Canonicalize the directory, since editors replace the file by name. For a
/// first write, keep the missing directory names below the nearest real parent.
fn canonical_config_target(path: &Path) -> anyhow::Result<PathBuf> {
    use std::path::Component;

    let absolute = std::path::absolute(path)?;
    let Some(parent) = absolute.parent() else {
        anyhow::bail!("configuration file has no parent: {}", path.display());
    };
    for ancestor in parent.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(mut directory) => {
                for name in absolute.strip_prefix(ancestor)?.components() {
                    match name {
                        Component::Normal(name) => directory.push(name),
                        Component::ParentDir => {
                            directory.pop();
                        }
                        Component::CurDir => {}
                        _ => anyhow::bail!("invalid configuration path: {}", path.display()),
                    }
                }
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("configuration directory is unavailable: {}", path.display())
}

/// Deterministic lock-file name for `target` inside `lock_dir`: FNV-1a over
/// the raw path bytes. The algorithm is pinned HERE (not `DefaultHasher`,
/// whose output std may change between releases) so two docsbase binaries
/// built on different toolchains — the daemon and a freshly installed CLI
/// during an upgrade — always contest the SAME lock file. Only used for
/// file naming, so a hash collision merely adds harmless serialization.
fn config_lock_path(lock_dir: &Path, target: &Path) -> PathBuf {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    // Path bytes come from the OS seam (ADR-9): raw bytes on Unix, the
    // lossy UTF-8 form elsewhere — consistent for any single installation.
    let mut hash = FNV_OFFSET_BASIS;
    for byte in crate::platform::paths::as_bytes(target) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    lock_dir.join(format!("{hash:016x}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_write_lock_serializes_and_releases() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let lock_dir = dir.path().join("locks");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let path = project.join(".docsbase.toml");
        std::fs::create_dir_all(&lock_dir).expect("lock dir");
        let lock_path = config_lock_path(&lock_dir, &canonical_config_target(&path).unwrap());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("lock open");
        let mut held = FdRwLock::new(file);
        let guard = held.try_write().expect("first writer holds");
        let mut waits = Vec::new();
        let mut writes = 0;
        let err = with_config_write_lock_in(
            &lock_dir,
            &path,
            || {
                writes += 1;
                Ok(())
            },
            |delay| waits.push(delay),
        )
        .expect_err("contended write fails after bounded retries");
        assert!(
            err.to_string().contains("another docsbase process"),
            "{err}"
        );
        assert_eq!(writes, 0, "exhausted lock must not execute the write");
        assert_eq!(
            waits,
            vec![CONFIG_LOCK_RETRY_DELAY; CONFIG_LOCK_RETRIES as usize],
            "every failed attempt must request the configured retry delay"
        );
        drop(guard);
        waits.clear();
        with_config_write_lock_in(
            &lock_dir,
            &path,
            || {
                writes += 1;
                Ok(())
            },
            |delay| waits.push(delay),
        )
        .expect("succeeds after release");
        assert_eq!(writes, 1);
        assert!(waits.is_empty(), "uncontended acquisition must not wait");

        // A wait callback can release a real lock without consuming backoff.
        let mut guard = Some(held.try_write().expect("hold for retry handoff"));
        let value = with_config_write_lock_in(
            &lock_dir,
            &path,
            || {
                writes += 1;
                Ok(17)
            },
            |delay| {
                waits.push(delay);
                drop(guard.take().expect("only the first attempt is contended"));
            },
        )
        .expect("acquires after the first retry releases the lock");
        assert_eq!(value, 17);
        assert_eq!(writes, 2, "successful retry must execute the write once");
        assert_eq!(waits, vec![CONFIG_LOCK_RETRY_DELAY]);
        assert!(guard.is_none());
        assert!(
            !project.join(".docsbase.toml.lock").exists(),
            "no sibling lock file"
        );
    }

    #[test]
    fn config_lock_path_is_stable_and_distinct() {
        // Round-5 F3: the id must not depend on std's `DefaultHasher`
        // algorithm (which may change between releases — two docsbase
        // binaries on different toolchains would then stop excluding each
        // other). Pinned by construction, and stable across calls.
        let dir = Path::new("/locks");
        let a = config_lock_path(dir, Path::new("/project/.docsbase.toml"));
        let a2 = config_lock_path(dir, Path::new("/project/.docsbase.toml"));
        let b = config_lock_path(dir, Path::new("/project/.docsbase2.toml"));
        assert_eq!(a, a2, "same target path yields the same lock file");
        assert_ne!(a, b, "different targets get different lock files");
        assert_eq!(
            a.file_name().and_then(|name| name.to_str()).map(str::len),
            Some(21),
            "16 hex digits + .lock"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_directory_aliases_share_a_lock_before_the_config_exists() {
        let root = tempfile::tempdir().unwrap();
        let lock_dir = root.path().join("locks");
        let physical = root.path().join("physical");
        let alias = root.path().join("alias");
        std::fs::create_dir(&physical).unwrap();
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let actual = physical.join("new/config.toml");
        let aliased = alias.join("new/config.toml");
        let mut alias_entered = false;
        let mut retries = 0;
        with_config_write_lock_in(
            &lock_dir,
            &actual,
            || {
                let result = with_config_write_lock_in(
                    &lock_dir,
                    &aliased,
                    || {
                        alias_entered = true;
                        Ok(())
                    },
                    |_| retries += 1,
                );
                assert!(
                    result.is_err(),
                    "an alias must contend with the canonical directory"
                );
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert!(!alias_entered);
        assert_eq!(retries, CONFIG_LOCK_RETRIES);

        // Simulate an older editor that hashes the original spelling only.
        let file = std::fs::File::create(config_lock_path(&lock_dir, &aliased)).unwrap();
        let mut legacy = FdRwLock::new(file);
        let _held = legacy.try_write().unwrap();
        let result = with_config_write_lock_in(&lock_dir, &aliased, || Ok(()), |_| {});
        assert!(result.is_err(), "an old editor still excludes a new writer");
    }

    #[test]
    fn config_write_lock_wrapper_routes_locks_to_data_dir() {
        // Re-exec ourselves in a child with a sandboxed `DOCSBASE_DATA_DIR`
        // so env mutation cannot race parallel tests (round-4 C1: the real
        // wrapper must route locks to `<data>/locks`, never next to the
        // target — the `..._in` variant above cannot pin that).
        //
        // The guard matters in the `hybrid-local` build, where runtime tests
        // mutate the same variable through `TestDataDir`: without it this
        // test can enter the re-exec branch with no override and then read a
        // sandbox value at the assert (round-6 deepseek).
        #[cfg(feature = "hybrid-local")]
        let _env_guard = crate::hybrid::runtime::test_env::runtime_test_guard();
        if std::env::var_os("DOCSBASE_DATA_DIR").is_none() {
            let exe = std::env::current_exe().expect("current exe");
            let tmp = tempfile::TempDir::new().expect("tmp");
            let status = std::process::Command::new(exe)
                .arg("--exact")
                .arg("config::paths::tests::config_write_lock_wrapper_routes_locks_to_data_dir")
                .env("DOCSBASE_DATA_DIR", tmp.path())
                .status()
                .expect("spawn child");
            assert!(status.success(), "child verification failed");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tmp");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let path = project.join(".docsbase.toml");
        with_config_write_lock(&path, || Ok(())).expect("wrapper write");
        assert!(
            !project.join(".docsbase.toml.lock").exists(),
            "no sibling lock file in the project root"
        );
        // The lock landed in our own data dir, not beside the target.
        let data = crate::config::paths::data_dir().expect("data dir");
        let lock_dir = data.join("locks");
        assert!(
            lock_dir.is_dir(),
            "locks dir exists: {}",
            lock_dir.display()
        );
        let actual: std::collections::BTreeSet<_> = std::fs::read_dir(&lock_dir)
            .expect("read locks")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "lock"))
            .map(|entry| entry.path())
            .collect();
        let target = canonical_config_target(&path).unwrap();
        let expected = [
            config_lock_path(&lock_dir, &path),
            config_lock_path(&lock_dir, &target),
        ]
        .into_iter()
        .collect();
        assert_eq!(actual, expected);
    }
}
