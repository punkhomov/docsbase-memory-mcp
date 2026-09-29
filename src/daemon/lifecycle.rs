//! Daemon lifecycle S1: single instance, autostart, grace shutdown and
//! stale-state recovery (FR-2, FR-6, FR-9; NFR-2; R5; ADR-2).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};

use crate::error::{Error, Result};
use crate::ipc::protocol::{self, Request};
use crate::platform::{self, Endpoint, Listener};
use crate::store::migrations;

/// How long [`ensure_daemon`] waits for the socket after spawning a child.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Default grace period before the last session's exit stops the daemon
/// (OQ-7: 5 s).
pub const DEFAULT_GRACE_MS: u64 = 5_000;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound for `daemon stop` while the daemon drains and exits; a busy
/// machine can stretch the 3 s grace window well past 5 s (T32 flake).
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// When there is no listener at all the stop request cannot be delivered:
/// wait briefly for a daemon that is already shutting down (it unlinks its
/// socket first), then report success without touching its state while the
/// process may still drain its lease.
const NO_LISTENER_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonState {
    /// Daemon process id.
    pub(crate) pid: u32,
    /// Local endpoint recorded at startup (serialized as its path string).
    pub(crate) socket: Endpoint,
    /// Build identity of the daemon binary (I1).
    pub(crate) build_id: String,
    /// Schema version the daemon was built against (I1).
    pub(crate) schema_version: u32,
    /// Canonical cache root the daemon serves (I1).
    pub(crate) cache_root: PathBuf,
}

/// Ensures a daemon is running for `cache`, starting one when needed (FR-2).
///
/// # Errors
/// Returns [`Error::Internal`] when the start lock or child process cannot be
/// used, or the socket does not appear within [`START_TIMEOUT`].
pub fn ensure_daemon(cache: &Path) -> Result<()> {
    ensure_daemon_with(cache, None)
}

/// [`ensure_daemon`] with an explicit daemon binary (integration tests).
///
/// # Errors
/// Same as [`ensure_daemon`].
pub fn ensure_daemon_with(cache: &Path, exe: Option<&Path>) -> Result<()> {
    if is_running(cache) {
        return Ok(());
    }
    create_state_dir(cache)?;
    let lock_file = open_lock_file(&start_lock_path(cache))?;
    let mut lock = RwLock::new(lock_file);
    let _guard = lock
        .write()
        .map_err(|err| Error::internal_with_source("acquire daemon start lock", err))?;
    if is_running(cache) {
        return Ok(());
    }
    if admission_lock_held(cache) {
        // A daemon is starting (bind/write_state gap) or alive without a
        // responsive socket yet: never unlink its files; just wait (FR-9).
        return wait_for_daemon(cache, START_TIMEOUT);
    }
    cleanup_stale(cache)?;
    spawn_detached(cache, exe)?;
    wait_for_daemon(cache, START_TIMEOUT)
}

/// True when some process holds `state/admission.lock` (FR-4, FR-9).
fn admission_lock_held(cache: &Path) -> bool {
    let path = state_dir(cache).join("admission.lock");
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    let mut lock = RwLock::new(file);
    match lock.try_write() {
        Ok(guard) => {
            drop(guard);
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => false,
    }
}

/// Runs the daemon in the foreground until stopped or the grace period after
/// the last session expires (FR-6, FR-9).
///
/// # Errors
/// Returns [`Error::Admission`] when another daemon owns the cache, and
/// IO/protocol errors otherwise.
pub fn run_daemon(cache: &Path, grace: Duration) -> Result<()> {
    let _lease = crate::daemon::admission::Lease::acquire(
        cache,
        &protocol::build_id(),
        migrations::SCHEMA_VERSION,
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| Error::internal_with_source("build tokio runtime", err))?;
    runtime.block_on(serve(cache, grace))
}

/// Asks a running daemon to stop and waits for it to exit (FR-6).
///
/// Stopping when no daemon runs is a no-op.
///
/// # Errors
/// Returns [`Error::Internal`] when the stop request cannot be delivered or
/// the daemon does not exit within five seconds.
pub fn stop_daemon(cache: &Path) -> Result<()> {
    let Some(state) = read_state(cache)? else {
        return Ok(());
    };
    if !pid_alive(state.pid) {
        cleanup_stale(cache)?;
        return Ok(());
    }
    let endpoint = platform::daemon_endpoint(cache);
    let sent = if let Ok(mut stream) = platform::connect_blocking(&endpoint) {
        let bytes = protocol::encode(&Request::StopDaemon)?;
        stream
            .write_all(&bytes)
            .map_err(|err| Error::internal_with_source("send stop request", err))?;
        stream
            .flush()
            .map_err(|err| Error::internal_with_source("flush stop request", err))?;
        true
    } else {
        false
    };

    if !sent && !platform::exists(&endpoint) {
        let deadline = Instant::now() + NO_LISTENER_TIMEOUT;
        while Instant::now() < deadline {
            if !pid_alive(state.pid) {
                cleanup_stale(cache)?;
                return Ok(());
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        // Already unlinked its socket: the stop is under way even if the
        // process needs longer to release the admission lease (FR-5/FR-6).
        return Ok(());
    }

    // Wait for the process itself, not just the socket: `serve` unlinks its
    // state and socket before the process (and its admission lease) is gone
    // (FR-5).
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !pid_alive(state.pid) {
            cleanup_stale(cache)?;
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Err(Error::internal(format!(
        "daemon (pid {}) did not stop in time",
        state.pid
    )))
}

/// PID of the running daemon, when one is alive.
#[must_use]
pub fn daemon_pid(cache: &Path) -> Option<u32> {
    let state = read_state(cache).ok().flatten()?;
    (pid_alive(state.pid) && socket_connectable(cache)).then_some(state.pid)
}

async fn serve(cache: &Path, grace: Duration) -> Result<()> {
    create_state_dir(cache)?;
    if is_running(cache) {
        return Err(Error::Admission {
            message: format!("daemon already running for {}", cache.display()),
        });
    }
    cleanup_stale(cache)?;

    let endpoint = platform::daemon_endpoint(cache);
    if platform::exists(&endpoint) {
        platform::remove(&endpoint)
            .map_err(|err| Error::internal_with_source("remove stale socket", err))?;
    }
    let listener = platform::bind(&endpoint).map_err(|err| {
        Error::internal_with_source(format!("bind {}", endpoint.as_path().display()), err)
    })?;
    fs::set_permissions(endpoint.as_path(), fs::Permissions::from_mode(0o600))
        .map_err(|err| Error::internal_with_source("chmod 0600 socket", err))?;
    write_state(cache)?;

    let shared = crate::daemon::server::Shared::open(cache)?;
    let result = accept_loop(shared, &listener, grace).await;

    if let Err(err) = platform::remove(&endpoint) {
        eprintln!(
            "warning: cannot remove {}: {err}",
            endpoint.as_path().display()
        );
    }
    if let Err(err) = fs::remove_file(state_file(cache)) {
        eprintln!("warning: cannot remove state file: {err}");
    }
    result
}

async fn accept_loop(
    shared: Arc<crate::daemon::server::Shared>,
    listener: &Listener,
    grace: Duration,
) -> Result<()> {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<()>();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|err| Error::internal_with_source("install SIGTERM handler", err))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|err| Error::internal_with_source("install SIGINT handler", err))?;
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    // Sessions whose frontend died without closing its socket must not keep
    // the daemon alive (NFR-9, C6).
    let mut janitor = tokio::time::interval(JANITOR_INTERVAL);
    janitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    janitor.tick().await;

    loop {
        let grace_sleep = async {
            match grace_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            accepted = listener.accept() => {
                let stream = accepted
                    .map_err(|err| Error::internal_with_source("accept connection", err))?;
                let in_flight = Arc::clone(&in_flight);
                let events = events_tx.clone();
                let shutdown = shutdown_tx.clone();
                let shared = Arc::clone(&shared);
                tokio::spawn(crate::daemon::server::handle_connection(
                    shared, stream, in_flight, events, shutdown,
                ));
            }
            Some(()) = events_rx.recv() => {
                // Only live frontend sessions keep the daemon alive; plain
                // connections (CLI handshakes) do not. A pending deadline is
                // not extended by connection churn, only reset by a session.
                if idle(&shared, &in_flight) {
                    if grace_deadline.is_none() {
                        grace_deadline = Some(tokio::time::Instant::now() + grace);
                    }
                } else {
                    grace_deadline = None;
                }
            }
            // An armed deadline must never cut off a session-less request or
            // a registration that is still running. The check happens in the
            // body: the timeout may resolve in the same instant a request
            // arrives, after the select already polled the other branches.
            () = grace_sleep => {
                if idle(&shared, &in_flight) {
                    break;
                }
                grace_deadline = None;
            },
            _ = shutdown_rx.changed() => break,
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            _ = janitor.tick() => {
                let shared_for_prune = Arc::clone(&shared);
                let pruned = tokio::task::spawn_blocking(move || {
                    shared_for_prune.sessions.prune_dead()
                })
                .await
                .unwrap_or_default();
                if !pruned.is_empty() {
                    eprintln!("cleanup: pruned {} dead session(s)", pruned.len());
                }
                if idle(&shared, &in_flight) && grace_deadline.is_none() {
                    grace_deadline = Some(tokio::time::Instant::now() + grace);
                }
            }
        }
    }
    Ok(())
}

/// How often dead frontends are reaped (NFR-9: cleanup within 2 s).
const JANITOR_INTERVAL: Duration = Duration::from_millis(500);

/// No live sessions and no request in flight.
fn idle(shared: &crate::daemon::server::Shared, in_flight: &AtomicUsize) -> bool {
    shared.sessions.count() == 0 && in_flight.load(Ordering::SeqCst) == 0
}

fn is_running(cache: &Path) -> bool {
    let Some(state) = read_state(cache).ok().flatten() else {
        return false;
    };
    pid_alive(state.pid) && socket_connectable(cache)
}

fn socket_connectable(cache: &Path) -> bool {
    platform::connect_probe(&platform::daemon_endpoint(cache))
}

fn cleanup_stale(cache: &Path) -> Result<()> {
    if is_running(cache) {
        return Ok(());
    }
    let state = state_file(cache);
    if state.exists() {
        fs::remove_file(&state).map_err(|err| {
            Error::internal_with_source(format!("remove {}", state.display()), err)
        })?;
    }
    let endpoint = platform::daemon_endpoint(cache);
    platform::remove(&endpoint).map_err(|err| {
        Error::internal_with_source(format!("remove {}", endpoint.as_path().display()), err)
    })?;
    Ok(())
}

fn spawn_detached(cache: &Path, exe: Option<&Path>) -> Result<()> {
    let mut command = Command::new(exe.map_or_else(daemon_exe, Path::to_path_buf));
    command
        .arg("serve")
        .arg("--detached")
        .env("DOCSBASE_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .process_group(0);
    // NFR-8: the detached daemon's diagnostics belong in logs/daemon.log
    // (0600), not /dev/null; a missing log file is a setup error.
    let log = open_daemon_log(cache)?;
    let log_err = log
        .try_clone()
        .map_err(|err| Error::internal_with_source("clone daemon log", err))?;
    command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if let Some(config_dir) = std::env::var_os("DOCSBASE_CONFIG_DIR") {
        command.env("DOCSBASE_CONFIG_DIR", config_dir);
    }
    command
        .spawn()
        .map_err(|err| Error::internal_with_source("spawn daemon", err))?;
    Ok(())
}

/// Opens (creating if needed) `logs/daemon.log` with owner-only permissions.
fn open_daemon_log(cache: &Path) -> Result<File> {
    let dir = cache.join("logs");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(&dir)
        .map_err(|err| Error::internal_with_source("create logs dir", err))?;
    let path = dir.join("daemon.log");
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))?;
    Ok(file)
}

fn daemon_exe() -> PathBuf {
    std::env::var_os("DOCSBASE_DAEMON_EXE").map_or_else(
        || std::env::current_exe().unwrap_or_else(|_| PathBuf::from("docsbase")),
        PathBuf::from,
    )
}

fn wait_for_daemon(cache: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_running(cache) {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Err(Error::internal(format!(
        "daemon did not start within {}s",
        timeout.as_secs()
    )))
}

fn write_state(cache: &Path) -> Result<()> {
    let state = DaemonState {
        pid: std::process::id(),
        socket: platform::daemon_endpoint(cache),
        build_id: protocol::build_id(),
        schema_version: migrations::SCHEMA_VERSION,
        cache_root: cache.to_path_buf(),
    };
    let json = serde_json::to_vec_pretty(&state)
        .map_err(|err| Error::internal_with_source("serialize daemon state", err))?;
    let path = state_file(cache);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|err| Error::internal_with_source("write daemon state", err))?;
    fs::rename(&tmp, &path).map_err(|err| Error::internal_with_source("publish daemon state", err))
}

pub(crate) fn read_state(cache: &Path) -> Result<Option<DaemonState>> {
    let path = state_file(cache);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).map_err(|err| Error::internal_with_source("read daemon state", err))?;
    let state = serde_json::from_slice(&bytes)
        .map_err(|err| Error::internal_with_source("parse daemon state", err))?;
    Ok(Some(state))
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    if !Path::new("/proc").is_dir() {
        // No /proc (unusual on Linux): keep the daemon considered alive and
        // rely on the socket check in `is_running`.
        return true;
    }
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    })
}

pub(crate) fn create_state_dir(cache: &Path) -> Result<()> {
    let dir = state_dir(cache);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(&dir)
        .map_err(|err| Error::internal_with_source("create state dir", err))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| Error::internal_with_source("chmod 0700 state dir", err))
}

fn open_lock_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))
}

/// Per-project index directory (design §3 runtime layout).
pub(crate) fn index_dir(cache_root: &Path, project_id: i64) -> PathBuf {
    cache_root
        .join("projects")
        .join(project_id.to_string())
        .join("tantivy")
}

/// True when `err` is the per-project writer-lease conflict (I5); callers
/// may wait and retry their job instead of dropping it.
#[must_use]
pub(crate) fn is_lease_conflict(err: &Error) -> bool {
    matches!(
        err,
        Error::Project { message, .. } if message.starts_with("another writer holds")
    )
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
pub(crate) fn with_writer_lease<T>(
    cache_root: &Path,
    project_id: i64,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let path = lease_path(cache_root, project_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_with_source(format!("create {}: {err}", parent.display()), err)
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| {
            Error::internal_with_source(format!("open {}: {err}", path.display()), err)
        })?;
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

pub(crate) fn state_dir(cache: &Path) -> PathBuf {
    cache.join("state")
}

fn state_file(cache: &Path) -> PathBuf {
    state_dir(cache).join("daemon.json")
}

fn start_lock_path(cache: &Path) -> PathBuf {
    state_dir(cache).join("daemon.start.lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_lives_under_state_dir() {
        let cache = Path::new("/tmp/cache");
        assert_eq!(
            state_file(cache),
            PathBuf::from("/tmp/cache/state/daemon.json")
        );
        assert_eq!(
            start_lock_path(cache).file_name().expect("name"),
            "daemon.start.lock"
        );
    }
}
