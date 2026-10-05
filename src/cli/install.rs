//! `docsbase install` / `docsbase uninstall` (FR-1, FR-5; S4).
//!
//! Install stops the daemon, waits for every process to exit, takes the
//! admission lease so nothing can start mid-copy, atomically replaces the
//! binary and records it in an owned manifest. Uninstall prints the owned
//! artifacts and indexes, and deletes them only with `--yes`; files outside
//! the owned manifest entries (and the wholly owned cache root) are never
//! touched.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;

use crate::config::paths;
use crate::daemon::admission::Lease;
use crate::daemon::{lifecycle, registry};
use crate::error::Error;
use crate::ipc::protocol::{self, PROTOCOL_VERSION};
use crate::store::Db;
use crate::store::migrations::SCHEMA_VERSION;

/// Name of the owned install manifest inside the data directory.
pub const MANIFEST: &str = "install.json";

/// Installs (or updates) the current binary as an owned artifact.
///
/// # Errors
/// Fails when the daemon cannot be stopped, the admission lease is busy, or
/// the copy/rename fails.
pub fn install() -> anyhow::Result<()> {
    let source = std::env::current_exe().context("resolve current executable")?;
    let (target, data) = install_from(&source, Some(&source))?;
    println!("installed {} ({})", target.display(), protocol::build_id());
    println!("manifest: {}", data.join(MANIFEST).display());
    Ok(())
}

/// Shared install/update coordination (FR-5): stop the daemon, wait for every
/// process to exit, take the admission lease, atomically install `source` and
/// record the owned manifest.
///
/// `launcher` is the invoking binary; it is recorded so `uninstall` can remove
/// the copy that lives on the user's `PATH` (skipped when it *is* the owned
/// copy, e.g. `docsbase install` run from the data directory).
///
/// Returns the installed binary path and the canonical data directory.
///
/// # Errors
/// Fails when the daemon cannot be stopped, the admission lease is busy, or
/// the copy/rename fails.
pub(crate) fn install_from(
    source: &Path,
    launcher: Option<&Path>,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let cache = paths::cache_dir()?;
    crate::platform::fs::secure_dir(&cache)
        .with_context(|| format!("create {}", cache.display()))?;
    let cache = cache.canonicalize().context("canonicalize cache dir")?;
    let data = paths::data_dir()?;
    crate::platform::fs::secure_dir(&data).with_context(|| format!("create {}", data.display()))?;
    let data = data.canonicalize().context("canonicalize data dir")?;

    // The lease is held from before the stale-state cleanup until the binary
    // and manifest are in place, so no daemon can start mid-install (FR-5).
    let _lease = acquire_after_stop(&cache)?;
    clear_stale_state(&cache);

    let bin_dir = data.join("bin");
    fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;
    let target = bin_dir.join(format!("docsbase{}", std::env::consts::EXE_SUFFIX));
    swap_binary(source, &target)?;

    let manifest = Manifest {
        build_id: protocol::build_id(),
        protocol_version: PROTOCOL_VERSION,
        schema_version: SCHEMA_VERSION,
        binary: target.clone(),
        launcher: match launcher {
            Some(path) if path != target.as_path() => Some(path.to_path_buf()),
            _ => None,
        },
        socket: crate::platform::daemon_endpoint(&cache)
            .as_path()
            .to_path_buf(),
        cache_root: cache.clone(),
        installed_at: unix_now(),
    };
    write_manifest(&data, &manifest)?;

    Ok((target, data))
}

/// Removes every owned artifact; without `yes` it only prints them and the
/// indexed projects (FR-1).
///
/// # Errors
/// Fails when the manifest cannot be read or a removal fails.
pub fn uninstall(yes: bool) -> anyhow::Result<()> {
    let data = paths::data_dir()?;
    let data = if data.exists() {
        data.canonicalize().context("canonicalize data dir")?
    } else {
        data
    };
    let Some(manifest) = read_manifest(&data)? else {
        println!(
            "docsbase is not installed (no {}/{MANIFEST})",
            data.display()
        );
        return Ok(());
    };

    validate_manifest(&data, &manifest)?;
    println!("owned artifacts:");
    println!("  binary: {}", manifest.binary.display());
    if let Some(launcher) = manifest.launcher.as_deref()
        && launcher != manifest.binary.as_path()
    {
        println!("  launcher: {}", launcher.display());
    }
    println!("  manifest: {}", data.join(MANIFEST).display());
    println!("  cache root: {}", manifest.cache_root.display());
    println!("  socket: {}", manifest.socket.display());
    println!(
        "config (not removed): {}",
        paths::config_dir().map_or_else(
            |_| "(unknown)".to_owned(),
            |dir| dir.join("config.toml").display().to_string()
        )
    );
    println!("indexed projects:");
    let projects = list_projects(&manifest.cache_root);
    if projects.is_empty() {
        println!("  (none)");
    }
    for line in &projects {
        println!("  {line}");
    }

    if !yes {
        println!("re-run `docsbase uninstall --yes` to delete these artifacts");
        return Ok(());
    }

    let _lease = acquire_after_stop(&manifest.cache_root)?;
    // The launcher is only removed while it is still our binary (byte-equal to
    // the owned copy), checked before the owned copy disappears: a tampered
    // manifest must not turn uninstall into arbitrary deletion.
    let launcher = manifest
        .launcher
        .as_deref()
        .filter(|path| *path != manifest.binary.as_path())
        .filter(|path| path.is_file() && same_contents(path, &manifest.binary));
    if let Some(launcher) = launcher {
        remove_owned(launcher)?;
    }
    remove_owned(&manifest.binary)?;
    remove_file(&data.join(MANIFEST))?;
    if manifest.cache_root.is_dir() {
        fs::remove_dir_all(&manifest.cache_root)
            .with_context(|| format!("remove {}", manifest.cache_root.display()))?;
    }
    // Leave no empty scaffolding behind; foreign files keep the dir alive.
    if let Some(bin_dir) = manifest.binary.parent() {
        let _ = fs::remove_dir(bin_dir);
    }
    let _ = fs::remove_dir(&data);
    println!("uninstalled docsbase");
    Ok(())
}

/// Stops the daemon and waits for the admission lease, which is only released
/// when the daemon process really exits; retries while a blocking job winds
/// down (FR-5).
fn acquire_after_stop(cache: &Path) -> anyhow::Result<Lease> {
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    let mut cleared_stale = false;
    loop {
        let _ = lifecycle::stop_daemon(cache);
        match Lease::acquire(cache, &protocol::build_id(), SCHEMA_VERSION) {
            Ok(lease) => return Ok(lease),
            Err(err) if crate::daemon::admission::is_lock_busy(&err) => {
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow::Error::new(err))
                        .context("daemon did not release the admission lease");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // `Lease::acquire` only reports a build/schema mismatch after it
            // has taken the exclusive lock, so no daemon was serving when it
            // failed: the state is stale (install is the documented remedy).
            // A racer cannot use the freed lock in this window: any daemon it
            // starts refuses on the same mismatch and `ensure_daemon` clears
            // stale state before spawning.
            Err(err) if matches!(err, Error::Admission { .. }) && !cleared_stale => {
                clear_stale_state(cache);
                cleared_stale = true;
            }
            Err(err) => return Err(anyhow::Error::new(err)),
        }
    }
}

/// Refuses manifests whose paths are not owned locations; a tampered or
/// copied manifest must never turn uninstall into arbitrary deletion.
fn validate_manifest(data: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    if !manifest.binary.is_absolute() || !manifest.cache_root.is_absolute() {
        anyhow::bail!("manifest paths must be absolute");
    }
    if manifest.binary.parent() != Some(data.join("bin").as_path()) {
        anyhow::bail!(
            "manifest binary {} is outside {}",
            manifest.binary.display(),
            data.join("bin").display()
        );
    }
    // A missing cache root means the indexes were already deleted: nothing to
    // remove, but the owned binary and manifest still can be.
    if !manifest.cache_root.exists() {
        return Ok(());
    }
    let configured = paths::cache_dir()
        .ok()
        .and_then(|root| root.canonicalize().ok());
    // A tampered manifest must never redirect the recursive delete: the cache
    // root has to be the configured one when that is resolvable (T48).
    let Some(configured) = configured else {
        // Without a resolvable configured cache, only unmistakable cache
        // layouts pass (a bare empty directory must not be deleted).
        let unmistakable = manifest.cache_root.join(crate::store::DB_FILE).is_file()
            || manifest.cache_root.join("state/daemon.json").exists();
        if !unmistakable {
            anyhow::bail!(
                "manifest cache root {} does not look like a docsbase cache",
                manifest.cache_root.display()
            );
        }
        return Ok(());
    };
    if manifest.cache_root != configured {
        anyhow::bail!(
            "manifest cache root {} is not the configured cache {} (tampered or stale?)",
            manifest.cache_root.display(),
            configured.display()
        );
    }
    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Manifest {
    build_id: String,
    protocol_version: u32,
    schema_version: u32,
    binary: PathBuf,
    /// Copy of the binary that invoked `install`/`update` (usually the
    /// `PATH` launcher placed by the installer script).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launcher: Option<PathBuf>,
    socket: PathBuf,
    cache_root: PathBuf,
    installed_at: i64,
}

fn write_manifest(data: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
    let bytes = serde_json::to_vec_pretty(manifest).context("serialize manifest")?;
    let path = data.join(MANIFEST);
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", tmp.display()))?;
    drop(file);
    crate::platform::fs::secure_file(&tmp).with_context(|| format!("chmod {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("publish {}", path.display()))
}

fn read_manifest(data: &Path) -> anyhow::Result<Option<Manifest>> {
    let path = data.join(MANIFEST);
    if !path.exists() {
        return Ok(None);
    }
    match fs::metadata(&path) {
        Ok(metadata) if metadata.len() > crate::limits::MAX_STATE_BYTES => {
            anyhow::bail!(
                "manifest {} is {} bytes, over the {} byte limit (corrupt?)",
                path.display(),
                metadata.len(),
                crate::limits::MAX_STATE_BYTES
            );
        }
        _ => {}
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parse {}", path.display()))
}

/// Atomically replaces `target` with `source` through a same-directory temp
/// file, tolerating a running `target` (self-update on Windows cannot rename
/// over the live image; it is moved aside first).
pub(crate) fn swap_binary(source: &Path, target: &Path) -> anyhow::Result<()> {
    let tmp = target.with_extension("tmp");
    fs::copy(source, &tmp)
        .with_context(|| format!("copy {} -> {}", source.display(), tmp.display()))?;
    // Windows `FlushFileBuffers` (sync_all) needs write access; open for
    // writing instead of the read-only default.
    let copied = fs::OpenOptions::new()
        .write(true)
        .open(&tmp)
        .with_context(|| format!("open {}", tmp.display()))?;
    copied
        .sync_all()
        .with_context(|| format!("sync {}", tmp.display()))?;
    drop(copied);
    crate::platform::fs::secure_executable(&tmp)
        .with_context(|| format!("chmod {}", tmp.display()))?;
    if let Err(err) = fs::rename(&tmp, target) {
        // Windows refuses to replace a running executable: move it aside and
        // drop the new file in. The running process keeps the old image.
        if !is_current_exe(target) {
            return Err(anyhow::Error::new(err))
                .with_context(|| format!("replace {}", target.display()));
        }
        let old = target.with_extension("old");
        let _ = fs::remove_file(&old);
        fs::rename(target, &old).with_context(|| format!("move aside {}", target.display()))?;
        fs::rename(&tmp, target).with_context(|| format!("replace {}", target.display()))?;
        // Removal may fail on Windows while this process still maps the file;
        // the next self-replace cleans it before moving that binary aside.
        let _ = fs::remove_file(&old);
    }
    Ok(())
}

/// Whether `path` is the executable this process is running from.
fn is_current_exe(path: &Path) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    match (path.canonicalize(), current.canonicalize()) {
        (Ok(path), Ok(current)) => path == current,
        _ => path == current,
    }
}

/// A daemon that crashed may leave `daemon.json` behind; install is the
/// documented remedy for stale state, so clear it before admission.
fn clear_stale_state(cache: &Path) {
    let state = lifecycle::state_dir(cache);
    let _ = fs::remove_file(state.join("daemon.json"));
    let _ = crate::platform::remove(&crate::platform::daemon_endpoint(cache));
}

fn list_projects(cache: &Path) -> Vec<String> {
    if !cache.join(crate::store::DB_FILE).exists() {
        return Vec::new();
    }
    let Ok(db) = Db::open_readonly(cache) else {
        return vec![format!("(cannot read {})", cache.display())];
    };
    match registry::list_projects(&db) {
        Ok(projects) => projects
            .into_iter()
            .map(|project| {
                format!(
                    "{} ({}, status {})",
                    project.name,
                    project.canonical_root.display(),
                    project.status.as_str()
                )
            })
            .collect(),
        Err(err) => vec![format!("(cannot list projects: {err})")],
    }
}

fn remove_file(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(anyhow::Error::new(err)).with_context(|| format!("remove {}", path.display()))
        }
    }
}

/// Removes an owned executable, moving the running image aside on Windows,
/// where a locked executable cannot be deleted. A leftover `.old` is cleaned
/// by the next install.
fn remove_owned(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) if is_current_exe(path) => {
            let old = path.with_extension("old");
            let _ = fs::remove_file(&old);
            fs::rename(path, &old).with_context(|| format!("move aside {}", path.display()))?;
            let _ = fs::remove_file(&old);
            Ok(())
        }
        Err(err) => {
            Err(anyhow::Error::new(err)).with_context(|| format!("remove {}", path.display()))
        }
    }
}

/// Whether two files have identical contents (used to re-verify ownership of
/// the recorded launcher before deleting it).
fn same_contents(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}
