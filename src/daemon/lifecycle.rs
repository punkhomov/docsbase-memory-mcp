//! Daemon lifecycle S1: single instance, autostart, grace shutdown and
//! stale-state recovery (FR-2, FR-6, FR-9; NFR-2; R5; ADR-2).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::config::bootstrap::{BootstrapContext, EntrypointFacts};
use crate::daemon::launch::{AdmittedLaunch, LaunchConfig, LaunchSnapshot};
use crate::daemon::settings::{DaemonPatch, DaemonSpec};
use crate::error::{Error, Result};
use crate::ipc::protocol::{self, Request};
use crate::platform::{self, Endpoint, Listener};
use crate::store::migrations;

#[cfg(test)]
#[path = "project_read_tests.rs"]
mod project_read_tests;

/// How long [`ensure_daemon`] waits for the socket after spawning a child.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Default grace period before the last session's exit stops the daemon
/// (OQ-7: 5 s).
pub const DEFAULT_GRACE_MS: u64 = DaemonSpec::DEFAULTS.idle_grace_ms;

/// Concurrent connections a daemon accepts (T46); excess clients are shed
/// immediately so fd/memory stay bounded.
pub const MAX_CONNECTIONS: usize = DaemonSpec::DEFAULTS.max_connections;

// Standalone start/stop adapters use the owner's default lifecycle poll policy.
const POLL_INTERVAL: Duration = Duration::from_millis(DaemonSpec::DEFAULTS.poll_interval_ms);
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
    crate::daemon::launch::require_shared_without_daemon()?;
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
        Err(err) => {
            // Fail closed: an unreadable lock (EACCES/EMFILE) must not let a
            // second daemon start against the same cache.
            eprintln!(
                "warning: cannot probe admission lock {}: {err}",
                path.display()
            );
            true
        }
    }
}

/// Runs the daemon in the foreground until stopped or the grace period after
/// the last session expires (FR-6, FR-9).
///
/// # Errors
/// Returns [`Error::Admission`] when another daemon owns the cache, and
/// IO/protocol errors otherwise.
pub fn run_daemon(cache: &Path, grace: Duration) -> Result<()> {
    run_daemon_with_settings(cache, grace, &crate::store::settings::standalone()?)
}

/// Runs a daemon with the controlling boundary's resolved storage policy.
///
/// # Errors
/// Invalid policy is rejected before admission/state creation. Other errors
/// match [`run_daemon`].
pub fn run_daemon_with_settings(
    cache: &Path,
    grace: Duration,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> Result<()> {
    run_daemon_with_policies(
        cache,
        grace,
        settings,
        &crate::store::settings::standalone_read_pool()?,
    )
}

/// Runs a daemon with explicit storage and prepared reader policies.
///
/// # Errors
/// Invalid policy is rejected before admission/state creation. Other errors
/// match [`run_daemon_with_settings`].
pub fn run_daemon_with_policies(
    cache: &Path,
    grace: Duration,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
) -> Result<()> {
    run_daemon_with_dense_policy(
        cache,
        grace,
        settings,
        pool_settings,
        crate::hybrid::dense_settings::standalone()?.settings(),
    )
}

/// Run with complete storage, reader and query-cache policy from the boundary.
///
/// # Errors
/// Invalid policy fails before admission/state creation; other errors match
/// [`run_daemon_with_policies`].
pub fn run_daemon_with_resource_policies(
    cache: &Path,
    grace: Duration,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    query_settings: &crate::hybrid::dense_settings::QueryCacheSettings,
) -> Result<()> {
    let mut dense = crate::hybrid::dense_settings::standalone()?.into_settings();
    dense.query_cache_max_entries = query_settings.max_entries;
    dense.query_cache_max_bytes = query_settings.max_bytes;
    run_daemon_with_dense_policy(cache, grace, settings, pool_settings, &dense)
}

/// Run with complete owner-resolved dense/query policy, immutable for this daemon.
///
/// # Errors
/// Invalid component policies fail before admission/state creation; startup
/// failures otherwise match [`run_daemon_with_policies`].
pub fn run_daemon_with_dense_policy(
    cache: &Path,
    grace: Duration,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
) -> Result<()> {
    let grace_ms = u64::try_from(grace.as_millis()).map_err(|_| Error::Admission {
        message: "daemon grace milliseconds overflow".to_owned(),
    })?;
    let launch = LaunchSnapshot::capture().legacy(DaemonPatch {
        idle_grace_ms: Some(grace_ms),
        ..DaemonPatch::default()
    })?;
    run_with_launch(
        cache,
        settings,
        pool_settings,
        dense_settings,
        launch,
        &runtime_bootstrap()?,
    )
}

/// Strict launch boundary. Captured config/daemon policy is validated before
/// admission locks, cache writes, runtime construction or listener publication.
///
/// # Errors
/// Invalid launch/component policy or daemon startup failure.
pub fn run_daemon_with_launch(
    cache: &Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    snapshot: &LaunchSnapshot,
    patch: DaemonPatch,
) -> Result<()> {
    run_with_launch(
        cache,
        settings,
        pool_settings,
        dense_settings,
        snapshot.strict(patch)?,
        &runtime_bootstrap()?,
    )
}

/// Strict launch with already captured bootstrap facts. The exact Tokio worker
/// count is checked before writes and passed to the builder without an env reread.
///
/// # Errors
/// Invalid configuration, executor count overflow or daemon startup failure.
pub fn run_daemon_with_launch_and_bootstrap(
    cache: &Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    snapshot: &LaunchSnapshot,
    patch: DaemonPatch,
    bootstrap: &BootstrapContext,
) -> Result<()> {
    run_with_launch(
        cache,
        settings,
        pool_settings,
        dense_settings,
        snapshot.strict(patch)?,
        bootstrap,
    )
}

// The existing CLI retains its documented warning/default startup adapter.
// S1b will add explicit profile activation at that controlling boundary.
pub(crate) fn run_daemon_with_legacy_launch(
    cache: &Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    snapshot: &LaunchSnapshot,
    patch: DaemonPatch,
    detached: bool,
) -> Result<()> {
    let launch = snapshot.legacy(patch)?;
    if detached
        && matches!(
            launch.config.settings().daemon.profile,
            crate::daemon::settings::DaemonProfile::Website { .. }
        )
    {
        return Err(Error::Admission { message: "website profile requires explicit foreground `docsbase serve`; --detached is inapplicable".to_owned() });
    }
    run_with_launch(
        cache,
        settings,
        pool_settings,
        dense_settings,
        launch,
        &runtime_bootstrap()?,
    )
}

fn runtime_bootstrap() -> Result<BootstrapContext> {
    let cores = std::thread::available_parallelism().map_err(|error| {
        Error::internal_with_source("capture daemon runtime parallelism", error)
    })?;
    BootstrapContext::capture_with(cores.get(), EntrypointFacts::standalone(), std::env::var_os)
}

fn run_with_launch(
    cache: &Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    launch: LaunchConfig,
    bootstrap: &BootstrapContext,
) -> Result<()> {
    use crate::config::schema::SettingsSpec;
    launch.config.settings().validate()?;
    crate::hybrid::dense_settings::DenseSpec::validate(
        dense_settings,
        &crate::config::schema::Capabilities::current(),
    )?;
    crate::store::settings::DbSpec::validate(
        settings.settings(),
        &crate::config::schema::Capabilities::current(),
    )?;
    crate::store::settings::ReadPoolSpec::validate(
        pool_settings.settings(),
        &crate::config::schema::Capabilities::current(),
    )?;
    let workers = bootstrap.tokio_workers.value.get();
    launch
        .config
        .settings()
        .daemon
        .max_blocking_threads
        .checked_add(workers)
        .ok_or_else(|| Error::Admission {
            message: "daemon blocking plus async executor thread capacity overflow".to_owned(),
        })?;
    let launch = launch.admit(cache, settings)?;
    let _lease = crate::daemon::admission::Lease::acquire(
        cache,
        &protocol::build_id(),
        migrations::SCHEMA_VERSION,
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(launch.launch.config.settings().daemon.max_blocking_threads)
        .enable_all()
        .build()
        .map_err(|err| Error::internal_with_source("build tokio runtime", err))?;
    runtime.block_on(serve(
        cache,
        settings,
        pool_settings,
        dense_settings,
        launch,
    ))
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
    let bytes = protocol::encode(&Request::StopDaemon)?;
    // Send the stop request while waiting for the process itself, not just
    // the socket: `serve` unlinks its state and socket before the process
    // (and its admission lease) is gone (FR-5). A daemon at connection
    // capacity sheds new connections before reading, so a single attempt can
    // be dropped silently — retry until the deadline.
    let mut no_listener_since: Option<Instant> = None;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !pid_alive(state.pid) {
            cleanup_stale(cache)?;
            return Ok(());
        }
        match platform::connect_blocking(&endpoint) {
            Ok(mut stream) => {
                no_listener_since = None;
                // A write can fail because the peer is already shutting down
                // and closed the connection; that is a stop under way, not a
                // client error. Keep polling the process below, which is what
                // decides success.
                if stream.write_all(&bytes).is_ok() {
                    let _ = stream.flush();
                }
            }
            Err(_) if !platform::exists(&endpoint) => {
                let since = *no_listener_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= NO_LISTENER_TIMEOUT {
                    // Unix: the socket file is unlinked only once shutdown is
                    // committed, so the stop is under way even if the process
                    // needs longer to release the admission lease
                    // (FR-5/FR-6). Windows pipes have no file to observe, so
                    // fall through and verify the pid really exits.
                    #[cfg(unix)]
                    return Ok(());
                }
            }
            Err(_) => {}
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

async fn serve(
    cache: &Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    launch: AdmittedLaunch,
) -> Result<()> {
    create_state_dir(cache)?;
    if is_running(cache) {
        return Err(Error::Admission {
            message: format!("daemon already running for {}", cache.display()),
        });
    }
    cleanup_stale(cache)?;

    // Prepare resources before advertising a listener or daemon state.
    let shared = crate::daemon::server::Shared::open_with_admitted_launch(
        cache,
        settings,
        pool_settings,
        dense_settings,
        launch,
    )?;

    let endpoint = platform::daemon_endpoint(cache);
    if platform::exists(&endpoint) {
        platform::remove(&endpoint)
            .map_err(|err| Error::internal_with_source("remove stale socket", err))?;
    }
    let listener = platform::bind(&endpoint).map_err(|err| {
        Error::internal_with_source(format!("bind {}", endpoint.as_path().display()), err)
    })?;
    platform::fs::secure_file(endpoint.as_path())
        .map_err(|err| Error::internal_with_source("chmod 0600 socket", err))?;
    write_state(cache)?;

    let result = accept_loop(shared, &listener).await;

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

// Pure clock/event seam used by both session events and janitor observations.
fn idle_deadline(
    profile: &crate::daemon::settings::DaemonProfile,
    current: Option<tokio::time::Instant>,
    is_idle: bool,
    now: tokio::time::Instant,
    grace: Duration,
) -> Option<tokio::time::Instant> {
    match profile {
        crate::daemon::settings::DaemonProfile::Shared if is_idle => {
            current.or_else(|| Some(now + grace))
        }
        _ => None,
    }
}

async fn accept_loop(
    shared: Arc<crate::daemon::server::Shared>,
    listener: &Listener,
) -> Result<()> {
    let settings = shared.daemon_settings().clone();
    let grace = Duration::from_millis(settings.idle_grace_ms);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (events_tx, mut events_rx) = mpsc::channel::<()>(settings.max_events);
    let connections = Arc::new(tokio::sync::Semaphore::new(settings.max_connections));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let mut shutdown_signal = platform::ShutdownSignal::new()
        .map_err(|err| Error::internal_with_source("install signal handlers", err))?;
    let mut grace_deadline: Option<tokio::time::Instant> = None;
    // True when the loop ended because a graceful StopDaemon was requested;
    // signal-driven shutdown keeps the pre-T46 semantics (abandon in-flight).
    let mut graceful_stop = false;

    // Sessions whose frontend died without closing its socket must not keep
    // the daemon alive (NFR-9, C6).
    let mut janitor = tokio::time::interval(Duration::from_millis(settings.janitor_interval_ms));
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
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    // Over MAX_CONNECTIONS: shed the connection immediately
                    // (T46); the client sees EOF and the daemon keeps serving.
                    drop(stream);
                    continue;
                };
                let in_flight = Arc::clone(&in_flight);
                let events = events_tx.clone();
                let shutdown = shutdown_tx.clone();
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    crate::daemon::server::handle_admitted_connection(
                        shared, stream, in_flight, events, shutdown, permit,
                    )
                    .await;
                });
            }
            Some(()) = events_rx.recv() => {
                // Only live frontend sessions keep the daemon alive; plain
                // connections (CLI handshakes) do not. A pending deadline is
                // not extended by connection churn, only reset by a session.
                grace_deadline = idle_deadline(&settings.profile, grace_deadline,
                    idle(&shared, &in_flight), tokio::time::Instant::now(), grace);
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
            _ = shutdown_rx.changed() => {
                graceful_stop = true;
                break;
            }
            () = shutdown_signal.recv() => break,
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
                if idle(&shared, &in_flight) {
                    grace_deadline = idle_deadline(&settings.profile, grace_deadline,
                        true, tokio::time::Instant::now(), grace);
                }
            }
        }
    }
    // Shutdown was requested: the StopDaemon reply itself is bounded by
    // `DRAIN_TIMEOUT` (T46), but running requests must still deliver their
    // replies and publish their state before the runtime drops the handler
    // tasks (T25: install waits for an in-flight index job). A second
    // SIGTERM/SIGINT aborts the wait so a wedged request (a peer that stopped
    // reading its response) cannot pin the process: Tokio keeps the only
    // signal handler installed, so the escalation must be polled here.
    if graceful_stop {
        let drained = async {
            while in_flight.load(Ordering::SeqCst) > 0 {
                tokio::time::sleep(Duration::from_millis(settings.poll_interval_ms)).await;
            }
        };
        tokio::select! {
            () = drained => {}
            () = shutdown_signal.recv() => {
                eprintln!("shutdown: signal while draining; abandoning in-flight requests");
            }
        }
    }
    Ok(())
}

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
    // Idempotent by nature: concurrent stops (and a stop racing the daemon's
    // own cleanup) must not turn a sibling's success into an error. `exists()`
    // + `remove_file()` is a TOCTOU pair, so the NotFound arm is the real
    // guard, not the check above.
    match fs::remove_file(state_file(cache)) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            let state = state_file(cache);
            return Err(Error::internal_with_source(
                format!("remove {}", state.display()),
                err,
            ));
        }
    }
    let endpoint = platform::daemon_endpoint(cache);
    platform::remove(&endpoint).map_err(|err| {
        Error::internal_with_source(format!("remove {}", endpoint.as_path().display()), err)
    })?;
    Ok(())
}

fn spawn_detached(cache: &Path, exe: Option<&Path>) -> Result<()> {
    let mut command = Command::new(exe.map_or_else(daemon_exe, Path::to_path_buf));
    platform::process::detach(&mut command);
    command
        .arg("serve")
        .arg("--detached")
        .env("DOCSBASE_CACHE_DIR", cache)
        .stdin(Stdio::null());
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
    platform::fs::secure_dir(&dir)
        .map_err(|err| Error::internal_with_source("create logs dir", err))?;
    let path = dir.join("daemon.log");
    // The daemon holds this file as stdout/stderr for its whole life, so it
    // can only be rotated between processes — here, before spawning (T48).
    match crate::limits::rotate_log(&path) {
        Ok(true) => eprintln!("rotated {}", path.display()),
        Ok(false) => {}
        Err(err) => eprintln!("warning: cannot rotate {}: {err}", path.display()),
    }
    platform::fs::open_private_log(&path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))
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
    let mut file = fs::File::create(&tmp)
        .map_err(|err| Error::internal_with_source("create daemon state", err))?;
    file.write_all(&json)
        .map_err(|err| Error::internal_with_source("write daemon state", err))?;
    file.sync_all()
        .map_err(|err| Error::internal_with_source("sync daemon state", err))?;
    fs::rename(&tmp, &path).map_err(|err| Error::internal_with_source("publish daemon state", err))
}

pub(crate) fn read_state(cache: &Path) -> Result<Option<DaemonState>> {
    let path = state_file(cache);
    // Bound the read: an oversized state file is corrupt input, not something
    // to load into memory (T48).
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.len() > crate::limits::MAX_STATE_BYTES => {
            return Err(Error::internal(format!(
                "daemon state {} is {} bytes, over the {} byte limit",
                path.display(),
                metadata.len(),
                crate::limits::MAX_STATE_BYTES
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::internal_with_source("stat daemon state", err)),
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::internal_with_source("read daemon state", err)),
    };
    let state = serde_json::from_slice(&bytes)
        .map_err(|err| Error::internal_with_source("parse daemon state", err))?;
    Ok(Some(state))
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    platform::process::process_alive(pid)
}

pub(crate) fn create_state_dir(cache: &Path) -> Result<()> {
    let dir = state_dir(cache);
    platform::fs::secure_dir(&dir)
        .map_err(|err| Error::internal_with_source("create state dir", err))
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

/// Runs short search preparation under a shared project lease. Cooperating
/// index/watcher writers use the same lock file and cannot update either store
/// until this scope ends. Other readers remain admitted.
///
/// The caller must capture owned data and release this scope before invoking
/// providers. This file lease alone does not validate project status, schema or
/// crash markers and does not make arbitrary uncoordinated writers safe.
///
/// # Errors
/// Refuses immediately with a retry hint when a writer owns the lease; returns
/// filesystem/lock errors or the work error unchanged. Unwind releases the lease.
pub fn with_project_read_lease<T>(
    cache_root: &Path,
    project_id: i64,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let path = lease_path(cache_root, project_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_with_source(format!("create {}", parent.display()), err)
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))?;
    let lock = RwLock::new(file);
    let _guard = lock.try_read().map_err(|err| {
        if err.kind() == std::io::ErrorKind::WouldBlock {
            Error::Project {
                message: format!("another writer holds {}", path.display()),
                instruction: Some(
                    "wait for the index update to finish, then retry search".to_owned(),
                ),
            }
        } else {
            Error::internal_with_source(format!("read-lock {}", path.display()), err)
        }
    })?;
    work()
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
    fn website_never_arms_an_idle_deadline_and_shared_keeps_original_deadline() {
        use crate::daemon::settings::DaemonProfile;
        let now = tokio::time::Instant::now();
        let grace = Duration::from_millis(5_000);
        let website = DaemonProfile::Website {
            project_root: PathBuf::from("/fixed"),
        };
        assert_eq!(idle_deadline(&website, None, true, now, grace), None);
        assert_eq!(idle_deadline(&website, Some(now), true, now, grace), None);
        let deadline = idle_deadline(&DaemonProfile::Shared, None, true, now, grace);
        assert_eq!(deadline, Some(now + grace));
        assert_eq!(
            idle_deadline(&DaemonProfile::Shared, deadline, true, now + grace, grace),
            deadline
        );
        assert_eq!(
            idle_deadline(&DaemonProfile::Shared, deadline, false, now, grace),
            None
        );
        assert_eq!(
            idle_deadline(&DaemonProfile::Shared, None, true, now, Duration::ZERO),
            Some(now)
        );
    }

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

#[cfg(test)]
mod store_policy_tests {
    use super::*;

    #[test]
    fn invalid_pool_policy_leaves_admission_and_state_absent() {
        let root = tempfile::tempdir().expect("root");
        let cache = root.path().join("cache");
        let db = crate::store::settings::standalone().expect("db policy");
        let valid = crate::store::settings::standalone_read_pool().expect("pool policy");
        let mut raw = valid.settings().clone();
        raw.max_connections = 0;
        let imported = crate::config::schema::Resolved::new(raw, valid.provenance().clone());
        assert!(run_daemon_with_policies(&cache, Duration::ZERO, &db, &imported).is_err());
        assert!(!cache.exists());
    }

    #[test]
    fn invalid_store_policy_leaves_admission_and_state_absent() {
        let root = tempfile::tempdir().expect("root");
        let cache = root.path().join("cache");
        let valid = crate::store::settings::standalone().expect("policy");
        let mut raw = valid.settings().clone();
        raw.wal_attempts = 0;
        let imported = crate::config::schema::Resolved::new(raw, valid.provenance().clone());
        let error = run_daemon_with_settings(&cache, Duration::ZERO, &imported)
            .expect_err("invalid settings must fail before admission");
        assert!(matches!(error, Error::Admission { .. }));
        assert!(!cache.exists());
    }
}
