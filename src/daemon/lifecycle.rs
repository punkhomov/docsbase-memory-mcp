//! Daemon lifecycle S1: single instance, autostart, grace shutdown and
//! stale-state recovery (FR-2, FR-6, FR-9; NFR-2; R5; ADR-2).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream as AsyncUnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};

use crate::error::{Error, Result};
use crate::ipc::client::socket_path;
use crate::ipc::protocol::{self, Request, Response};
use crate::store::migrations;

/// How long [`ensure_daemon`] waits for the socket after spawning a child.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Default grace period before the last session's exit stops the daemon
/// (OQ-7: 5 s).
pub const DEFAULT_GRACE_MS: u64 = 5_000;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonState {
    /// Daemon process id.
    pub(crate) pid: u32,
    /// Socket path recorded at startup.
    pub(crate) socket: PathBuf,
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
    if let Ok(mut stream) = UnixStream::connect(socket_path(cache)) {
        let bytes = protocol::encode(&Request::StopDaemon)?;
        stream
            .write_all(&bytes)
            .map_err(|err| Error::internal_with_source("send stop request", err))?;
        stream
            .flush()
            .map_err(|err| Error::internal_with_source("flush stop request", err))?;
    }

    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !is_running(cache) {
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

    let sock = socket_path(cache);
    if sock.exists() {
        fs::remove_file(&sock)
            .map_err(|err| Error::internal_with_source("remove stale socket", err))?;
    }
    let listener = UnixListener::bind(&sock)
        .map_err(|err| Error::internal_with_source(format!("bind {}", sock.display()), err))?;
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))
        .map_err(|err| Error::internal_with_source("chmod 0600 socket", err))?;
    write_state(cache)?;

    let result = accept_loop(&listener, grace).await;

    if let Err(err) = fs::remove_file(&sock) {
        eprintln!("warning: cannot remove {}: {err}", sock.display());
    }
    if let Err(err) = fs::remove_file(state_file(cache)) {
        eprintln!("warning: cannot remove state file: {err}");
    }
    result
}

async fn accept_loop(listener: &UnixListener, grace: Duration) -> Result<()> {
    let sessions = Arc::new(AtomicUsize::new(0));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<()>();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|err| Error::internal_with_source("install SIGTERM handler", err))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|err| Error::internal_with_source("install SIGINT handler", err))?;
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    loop {
        let grace_sleep = async {
            match grace_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            _ = shutdown_rx.changed() => break,
            () = grace_sleep => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted
                    .map_err(|err| Error::internal_with_source("accept connection", err))?;
                sessions.fetch_add(1, Ordering::SeqCst);
                grace_deadline = None;
                let sessions = Arc::clone(&sessions);
                let events = events_tx.clone();
                let shutdown = shutdown_tx.clone();
                tokio::spawn(session_loop(stream, sessions, events, shutdown));
            }
            Some(()) = events_rx.recv() => {
                if sessions.load(Ordering::SeqCst) == 0 {
                    grace_deadline = Some(tokio::time::Instant::now() + grace);
                }
            }
        }
    }
    Ok(())
}

async fn session_loop(
    stream: AsyncUnixStream,
    sessions: Arc<AtomicUsize>,
    events: mpsc::UnboundedSender<()>,
    shutdown: watch::Sender<bool>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match protocol::decode_request(line.as_bytes()) {
            Ok(Request::StopDaemon) => {
                let _ = shutdown.send(true);
                Response::ToolResult {
                    value: serde_json::Value::Null,
                }
            }
            Ok(_) => Response::Error {
                code: -32011,
                message: "tool routing arrives with T19".to_owned(),
            },
            Err(err) => Response::Error {
                code: err.mcp_code(),
                message: err.to_string(),
            },
        };
        let Ok(bytes) = protocol::encode(&response) else {
            break;
        };
        if write_half.write_all(&bytes).await.is_err() {
            break;
        }
    }
    sessions.fetch_sub(1, Ordering::SeqCst);
    let _ = events.send(());
}

fn is_running(cache: &Path) -> bool {
    let Some(state) = read_state(cache).ok().flatten() else {
        return false;
    };
    pid_alive(state.pid) && socket_connectable(cache)
}

fn socket_connectable(cache: &Path) -> bool {
    UnixStream::connect(socket_path(cache)).is_ok()
}

fn cleanup_stale(cache: &Path) -> Result<()> {
    if is_running(cache) {
        return Ok(());
    }
    for path in [state_file(cache), socket_path(cache)] {
        if path.exists() {
            fs::remove_file(&path).map_err(|err| {
                Error::internal_with_source(format!("remove {}", path.display()), err)
            })?;
        }
    }
    Ok(())
}

fn spawn_detached(cache: &Path, exe: Option<&Path>) -> Result<()> {
    let mut command = Command::new(exe.map_or_else(daemon_exe, Path::to_path_buf));
    command
        .arg("serve")
        .arg("--detached")
        .env("DOCSBASE_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
        .spawn()
        .map_err(|err| Error::internal_with_source("spawn daemon", err))?;
    Ok(())
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
        socket: socket_path(cache),
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

fn pid_alive(pid: u32) -> bool {
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
