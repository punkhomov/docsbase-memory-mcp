//! Debounced per-project watcher (FR-15, FR-16, NFR-4; R4, SC-6).
//!
//! [`spawn_watcher`] watches a project root, coalesces bursts using the
//! project policy (by default a 1.5 s quiet window and 2 s maximum) and forwards
//! batches of changed markdown paths over a channel; [`Watchers`] owns one
//! watcher plus an async consumer per project inside the daemon. Events below
//! the cache root (tantivy index state) are ignored so indexing cannot feed
//! itself (R4).
//!
//! The daemon expands directory events on admitted job CPU. Blocking callers
//! receive expanded batches at flush time. Deleted paths may denote directories;
//! the incremental job purges whole subtrees for those (FR-16).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::Config;
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_incremental_checked};
use crate::index::tantivy_index::IndexHandle;
use crate::index::walk;
use crate::search::operation::SearchOperation;
use crate::store::Db;
use crate::store::models::{Project, ProjectStatus};
use crate::store::settings::{DbSettings, DbSpec};
use serde_json::json;

pub(crate) mod settings;
pub use settings::{DEBOUNCE, MAX_DEBOUNCE, WatchSettings};

/// Watcher tracing: set `DOCSBASE_WATCH_DEBUG=1` to log raw events and
/// batches to stderr (diagnostics; no behavior change).
fn watch_debug() -> bool {
    std::env::var_os("DOCSBASE_WATCH_DEBUG").is_some()
}

/// Canonical spelling of `path`, tolerating a missing leaf (Windows verbatim
/// paths would otherwise disagree with the configured spelling).
fn canonicalize_allow_missing(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match path.parent().and_then(|parent| parent.canonicalize().ok()) {
        Some(parent) => path
            .file_name()
            .map_or_else(|| parent.clone(), |name| parent.join(name)),
        None => path.to_path_buf(),
    }
}

/// Stops the underlying watcher when dropped.
pub struct WatcherGuard {
    _caller_native: Option<RecommendedWatcher>,
    forwarder: Option<std::thread::JoinHandle<()>>,
    retirement: ProducerRetirement,
}
struct ProducerRetirement {
    cancel: crate::hybrid::work::Cancellation,
    wake: std::sync::mpsc::SyncSender<Option<PathBuf>>,
    activity: Option<Arc<crate::daemon::request::Activity>>,
    ticket: Option<tokio::sync::oneshot::Sender<Arc<crate::daemon::request::Activity>>>,
}
impl ProducerRetirement {
    fn begin(&mut self) {
        if let (Some(ticket), Some(activity)) = (self.ticket.take(), self.activity.take()) {
            activity.activate();
            let _ = ticket.send(activity);
        }
        self.cancel.cancel();
        let _ = self.wake.try_send(None);
    }
}
impl WatcherGuard {
    fn retiring(&self) -> bool {
        self.retirement.cancel.is_cancelled()
    }
    fn finished(&self) -> bool {
        self.forwarder
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    fn begin_retirement(&mut self) {
        self.retirement.begin();
        if let Some(forwarder) = &self.forwarder {
            forwarder.thread().unpark();
        }
    }
}
impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.begin_retirement();
    }
}
enum ProducerOwner {
    Caller,
    Managed,
    Daemon {
        source: crate::daemon::request::ActivitySource,
        #[cfg(test)]
        before_native_drop: Option<Arc<dyn Fn() + Send + Sync>>,
        #[cfg(test)]
        manual_events: bool,
    },
}

enum BatchFilter {
    ExpandDirectories(Box<IndexFilter>),
    RetainDirectoryEvents { root: PathBuf, cache: PathBuf },
}

struct ProducerNative {
    _watcher: RecommendedWatcher,
    #[cfg(test)]
    before_drop: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Drop for ProducerNative {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(observer) = &self.before_drop {
            observer();
        }
    }
}

// Field drop order also applies when thread creation fails. Drop the
// receiver before dropping the native owner, then release the activity.
struct ProducerResources {
    events: Receiver<Option<PathBuf>>,
    _native: Option<ProducerNative>,
    _retired: Option<tokio::sync::oneshot::Receiver<Arc<crate::daemon::request::Activity>>>,
}

/// Watches `project.canonical_root` and sends deduplicated batches of
/// changed paths through `tx` (FR-15). `config` is a swappable open-time
/// configuration slot: reopens replace it without restarting the watcher
/// (OQ-6). A caller-supplied rendezvous channel retains blocking send
/// semantics. Dropping the guard stops native notifications. A batch already
/// waiting in that channel can still be received or released by the caller.
///
/// # Errors
/// Returns [`Error::Internal`] when the notify backend cannot be created or
/// the root cannot be watched.
pub fn spawn_watcher(
    project: &Project,
    cache: &Path,
    config: Arc<Mutex<Arc<Config>>>,
    tx: std::sync::mpsc::SyncSender<Vec<PathBuf>>,
) -> Result<WatcherGuard> {
    spawn_watcher_sink_owned(
        project,
        cache,
        move || {
            config
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        },
        move |batch| tx.send(batch).map_err(|error| error.0),
        ProducerOwner::Caller,
        crate::hybrid::work::Cancellation::new(),
    )
}

fn spawn_watcher_using(
    project: &Project,
    cache: &Path,
    current_config: impl Fn() -> Arc<Config> + Send + 'static,
    tx: std::sync::mpsc::SyncSender<Vec<PathBuf>>,
) -> Result<WatcherGuard> {
    let cancel = crate::hybrid::work::Cancellation::new();
    let sender_cancel = cancel.clone();
    spawn_watcher_sink_owned(
        project,
        cache,
        current_config,
        move |batch| send_cancellable(&tx, batch, &sender_cancel),
        ProducerOwner::Managed,
        cancel,
    )
}

// std::sync::mpsc has no cancellable blocking send. Park only while a
// consumer's queue is full. The guard wakes this park immediately on stop.
const BLOCKING_SEND_POLL: Duration = Duration::from_millis(10);

fn send_cancellable<T>(
    tx: &std::sync::mpsc::SyncSender<T>,
    mut item: T,
    cancel: &crate::hybrid::work::Cancellation,
) -> std::result::Result<(), T> {
    use std::sync::mpsc::TrySendError;
    while !cancel.is_cancelled() {
        match tx.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(item)) => return Err(item),
            Err(TrySendError::Full(pending)) => {
                item = pending;
                std::thread::park_timeout(BLOCKING_SEND_POLL);
            }
        }
    }
    Err(item)
}

fn native_watcher(
    root: &Path,
    cache: &Path,
    event_tx: std::sync::mpsc::SyncSender<Option<PathBuf>>,
    stop: Option<crate::hybrid::work::Cancellation>,
) -> Result<RecommendedWatcher> {
    let watch_cache = cache.to_path_buf();
    let watch_root = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let Ok(event) = result else {
            eprintln!("warning: watcher event: {result:?}");
            return;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if watch_debug() {
            eprintln!("watch-debug: event {:?} {:?}", event.kind, event.paths);
        }
        for path in event.paths {
            if crate::platform::paths::is_under(&path, &watch_cache)
                || walk::is_pruned(&watch_root, &path)
            {
                continue;
            }
            let relevant = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => false,
                Ok(meta) => meta.is_dir() || walk::is_markdown(&path),
                // Deleted paths may need exact or subtree purges (FR-16).
                Err(_) => true,
            };
            if relevant {
                let sent = match &stop {
                    Some(stop) => send_cancellable(&event_tx, Some(path), stop).is_ok(),
                    None => event_tx.send(Some(path)).is_ok(),
                };
                if !sent {
                    return;
                }
            }
        }
    })
    .map_err(|err| Error::internal_with_source("create watcher", err))?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|err| {
            Error::internal_with_source(format!("watch {}: {err}", root.display()), err)
        })?;

    Ok(watcher)
}

fn spawn_watcher_sink_owned(
    project: &Project,
    cache: &Path,
    current_config: impl Fn() -> Arc<Config> + Send + 'static,
    send: impl Fn(Vec<PathBuf>) -> std::result::Result<(), Vec<PathBuf>> + Send + 'static,
    owner: ProducerOwner,
    cancel: crate::hybrid::work::Cancellation,
) -> Result<WatcherGuard> {
    let root = project.canonical_root.clone();
    // Canonicalize once: notify reports verbatim paths on Windows, while the
    // configured cache dir may use the short/drive spelling. The dir itself
    // may not exist yet (tests watch an in-root cache), so tolerate that.
    let cache = canonicalize_allow_missing(cache);
    // Bounded with blocking send: the notify callback applies backpressure
    // instead of growing without bound; no path is dropped (T46).
    let initial = current_config();
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(initial.watch.native_queue_capacity);
    let wake = event_tx.clone();
    let caller = matches!(owner, ProducerOwner::Caller);
    let filter_in_producer = !matches!(owner, ProducerOwner::Daemon { .. });
    #[cfg(test)]
    let use_native = !matches!(
        owner,
        ProducerOwner::Daemon {
            manual_events: true,
            ..
        }
    );
    #[cfg(not(test))]
    let use_native = true;
    let watcher = use_native
        .then(|| native_watcher(&root, &cache, event_tx, caller.then(|| cancel.clone())))
        .transpose()?;

    // A caller-owned sender can still be rendezvousing after stop. Its native
    // watcher stays with the guard and has a separately cancellable event send.
    let (caller_native, native) = if caller {
        (watcher, None)
    } else {
        (None, watcher)
    };
    #[cfg(test)]
    let before_native_drop = match &owner {
        ProducerOwner::Daemon {
            before_native_drop, ..
        } => before_native_drop.clone(),
        ProducerOwner::Caller | ProducerOwner::Managed => None,
    };
    let (activity, ticket, retired) = if let ProducerOwner::Daemon { source, .. } = owner {
        let (ticket, receiver) = tokio::sync::oneshot::channel();
        (
            Some(crate::daemon::request::Activity::dormant(&source)),
            Some(ticket),
            Some(receiver),
        )
    } else {
        (None, None, None)
    };
    let retirement = ProducerRetirement {
        cancel: cancel.clone(),
        wake,
        activity,
        ticket,
    };
    let project_id = project.id;
    let filter_root = root;
    let filter_cache = cache;
    let resources = ProducerResources {
        events: event_rx,
        _native: native.map(|watcher| ProducerNative {
            _watcher: watcher,
            #[cfg(test)]
            before_drop: before_native_drop,
        }),
        _retired: retired,
    };
    let forwarder = std::thread::Builder::new()
        .name(format!("docsbase-debounce-{project_id}"))
        .spawn(move || {
            let filter = if filter_in_producer {
                BatchFilter::ExpandDirectories(Box::new(IndexFilter::new(
                    filter_root,
                    filter_cache,
                    &initial,
                )))
            } else {
                BatchFilter::RetainDirectoryEvents {
                    root: filter_root,
                    cache: filter_cache,
                }
            };
            drop(initial);
            forward_batches(
                filter,
                current_config,
                |timeout| match timeout {
                    Some(timeout) => resources.events.recv_timeout(timeout),
                    None => resources
                        .events
                        .recv()
                        .map_err(|_| RecvTimeoutError::Disconnected),
                },
                send,
                Some(&cancel),
                Instant::now,
            );
            drop(resources);
        })
        .map_err(|err| Error::internal_with_source("spawn debounce thread", err))?;
    Ok(WatcherGuard {
        _caller_native: caller_native,
        forwarder: Some(forwarder),
        retirement,
    })
}

fn forward_batches(
    mut filter: BatchFilter,
    current_config: impl Fn() -> Arc<Config>,
    mut receive: impl FnMut(Option<Duration>) -> std::result::Result<Option<PathBuf>, RecvTimeoutError>,
    send: impl Fn(Vec<PathBuf>) -> std::result::Result<(), Vec<PathBuf>>,
    cancel: Option<&crate::hybrid::work::Cancellation>,
    now: impl Fn() -> Instant,
) {
    while let Ok(Some(first)) = receive(None) {
        if cancel.is_some_and(crate::hybrid::work::Cancellation::is_cancelled) {
            return;
        }
        let started = now();
        let timing = current_config().watch;
        let mut raw = Batch::new();
        raw.push(first);
        let mut disconnected = false;
        while let Some(timeout) = timing.next_event_timeout(now().duration_since(started)) {
            match receive(Some(timeout)) {
                Ok(Some(path)) => raw.push(path),
                Ok(None) => return,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let config = current_config();
        let batch = match &mut filter {
            BatchFilter::ExpandDirectories(filter) => {
                filter.set_config(&config);
                finalize(filter, raw)
            }
            BatchFilter::RetainDirectoryEvents { root, cache } => {
                retain_daemon_events(root, cache, &config, raw, cancel)
            }
        };
        if (!batch.is_empty() && send(batch).is_err()) || disconnected {
            return;
        }
    }
}

fn retain_daemon_events(
    root: &Path,
    cache: &Path,
    config: &Config,
    raw: Batch,
    cancel: Option<&crate::hybrid::work::Cancellation>,
) -> Vec<PathBuf> {
    let mut matcher = match walk::markdown_builder(root, root, config) {
        Ok(builder) => builder.build_matchers().pop(),
        Err(error) => {
            eprintln!("warning: watcher filter: {error}");
            None
        }
    };
    let mut paths = raw.into_vec();
    // Suppress already-ignored notifications without expanding directories.
    // A fresh matcher's cache lives for one debounce window. The daemon still
    // refilters admitted paths under the revision it will use for indexing.
    paths.retain(|path| {
        if cancel.is_some_and(crate::hybrid::work::Cancellation::is_cancelled)
            || walk::is_pruned(root, path)
            || crate::platform::paths::is_under(path, cache)
        {
            return false;
        }
        let kind = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata.file_type(),
            Err(error) => {
                return matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                );
            }
        };
        if !(kind.is_dir() || (kind.is_file() && walk::is_markdown(path)))
            || !path
                .canonicalize()
                .is_ok_and(|canonical| crate::platform::paths::is_under(&canonical, root))
        {
            return false;
        }
        let (Ok(relative), Some(matcher)) = (path.strip_prefix(root), &mut matcher) else {
            return false;
        };
        let (decision, error) = matcher.matched_with_errors(relative, kind.is_dir());
        if let Some(error) = error {
            eprintln!("warning: watcher filter: {error}");
        }
        !decision.is_ignore()
    });
    paths
}

/// Deduplicated batch under construction.
#[derive(Default)]
pub struct Batch {
    paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
}

impl Batch {
    /// Empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `path` once, keeping arrival order.
    pub fn push(&mut self, path: PathBuf) {
        if self.seen.insert(path.clone()) {
            self.paths.push(path);
        }
    }

    /// Paths in arrival order.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    fn into_vec(self) -> Vec<PathBuf> {
        self.paths
    }
}

/// Retry policy for one pending watcher batch.
#[derive(Debug, Clone, Copy)]
pub struct BatchRetryPolicy {
    /// Bounded retries for transient (internal) batch failures.
    pub max_transient_attempts: u32,
    /// Wait between lease-conflict retries. A full index job holds the lease;
    /// those retries are unbounded and never consume the transient budget
    /// (FR-16).
    pub lease_retry: Duration,
    /// Base delay between transient retries; multiplied by 3 per attempt.
    pub transient_backoff: Duration,
}

impl Default for BatchRetryPolicy {
    fn default() -> Self {
        settings::WatchSpec::DEFAULTS.retry_policy()
    }
}

/// Events reported while a batch is retried (tests assert them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchEvent {
    /// A full index job holds the writer lease; retried without counting.
    LeaseRetry { attempt: u32 },
    /// Transient failure; retried after a backoff.
    TransientRetry { attempt: u32, error: String },
    /// Transient retries exhausted; the batch is dropped and surfaced.
    DroppedTransient { attempts: u32, error: String },
    /// Permanent failure; the batch is dropped.
    DroppedPermanent { error: String },
    /// The project root vanished; the watcher must stop.
    RootMissing,
}

/// Terminal outcome of one pending batch.
#[derive(Debug)]
pub enum BatchOutcome {
    /// The batch was indexed.
    Done(JobStats),
    /// The batch was dropped after `attempts` transient retries.
    Dropped { attempts: u32, error: String },
    /// The project root disappeared; stop the watcher.
    RootMissing,
}

fn transient_batch_error(err: &Error) -> bool {
    matches!(err, Error::Internal { .. })
}

fn transient_delay(policy: &BatchRetryPolicy, attempt: u32) -> Duration {
    let factor = 3_u32.saturating_pow(attempt.saturating_sub(1));
    policy.transient_backoff.saturating_mul(factor)
}

/// Runs `pending` to a terminal state. Newly arrived paths are drained into
/// `pending` before every attempt, so retries coalesce instead of queueing
/// extra runs (events still in the channel are never lost: they are picked up
/// by the next outer receive). Lease conflicts retry unbounded (FR-16) and
/// outside the transient budget; transient failures retry within it; anything
/// else is dropped. Every step is reported through `report`.
pub fn run_pending_batch<R, D, S, M>(
    pending: &mut Batch,
    policy: &BatchRetryPolicy,
    mut run: R,
    mut drain: D,
    mut sleep: S,
    mut root_missing: M,
    report: &mut dyn FnMut(BatchEvent),
) -> BatchOutcome
where
    R: FnMut(&[PathBuf]) -> Result<JobStats>,
    D: FnMut(&mut Batch),
    S: FnMut(Duration),
    M: FnMut() -> bool,
{
    let mut transient_attempts = 0_u32;
    let mut lease_attempts = 0_u32;
    loop {
        drain(pending);
        if root_missing() {
            report(BatchEvent::RootMissing);
            return BatchOutcome::RootMissing;
        }
        match run(pending.paths()) {
            Ok(stats) => return BatchOutcome::Done(stats),
            Err(err) if lifecycle::is_lease_conflict(&err) => {
                lease_attempts = lease_attempts.saturating_add(1);
                report(BatchEvent::LeaseRetry {
                    attempt: lease_attempts,
                });
                sleep(policy.lease_retry);
            }
            Err(err) if transient_batch_error(&err) => {
                if transient_attempts >= policy.max_transient_attempts {
                    let error = err.to_string();
                    report(BatchEvent::DroppedTransient {
                        attempts: transient_attempts,
                        error: error.clone(),
                    });
                    return BatchOutcome::Dropped {
                        attempts: transient_attempts,
                        error,
                    };
                }
                transient_attempts += 1;
                report(BatchEvent::TransientRetry {
                    attempt: transient_attempts,
                    error: err.to_string(),
                });
                sleep(transient_delay(policy, transient_attempts));
            }
            Err(err) => {
                let error = err.to_string();
                report(BatchEvent::DroppedPermanent {
                    error: error.clone(),
                });
                return BatchOutcome::Dropped { attempts: 0, error };
            }
        }
    }
}

/// Remaining injected faults for the current fault-test root (test seam).
static INJECTED_FAULT_REMAINING: Mutex<Option<(String, usize)>> = Mutex::new(None);

/// Prefix that already armed its one-shot commit fault (see
/// [`injected_watch_fault`]).
static INJECTED_COMMIT_ARMED: Mutex<Option<String>> = Mutex::new(None);

/// Test seam (see `tests/watcher.rs`): with `DOCSBASE_WATCH_FAULT=internal:N`
/// and `DOCSBASE_WATCH_FAULT_ROOT=<prefix>` set, the first `N` batch runs of
/// a project whose root starts with the prefix fail with a synthetic
/// transient error. Inert unless both variables are set.
///
/// `DOCSBASE_WATCH_FAULT=commit:N` arms N injected *commit* failures for the
/// batch that follows (one-shot): the batch runs for real, but its commit
/// dies mid-way - the recovery path (`tests/mcp_registry.rs::
/// watcher_recovers_from_a_commit_failure`) needs the index marked for a
/// rebuild and rebuilt by the retried batch.
pub(crate) fn injected_watch_fault(root: &Path, cache: &Path) -> Option<Error> {
    let spec = std::env::var("DOCSBASE_WATCH_FAULT").ok()?;
    let prefix = std::env::var("DOCSBASE_WATCH_FAULT_ROOT").ok()?;
    if !root.starts_with(&prefix) {
        return None;
    }
    if let Some(budget) = spec
        .strip_prefix("commit:")
        .and_then(|raw| raw.parse::<usize>().ok())
    {
        let mut armed = INJECTED_COMMIT_ARMED
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if armed.as_deref() == Some(prefix.as_str()) {
            return None;
        }
        *armed = Some(prefix);
        crate::index::tantivy_index::inject_commit_faults(cache, budget);
        return None;
    }
    let budget = spec.strip_prefix("internal:")?.parse::<usize>().ok()?;
    let mut guard = INJECTED_FAULT_REMAINING
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let entry = guard.get_or_insert_with(|| (prefix.clone(), budget));
    if entry.0 != prefix {
        *entry = (prefix.clone(), budget);
    }
    if entry.1 == 0 {
        return None;
    }
    entry.1 -= 1;
    Some(Error::internal(
        "injected watcher batch fault (DOCSBASE_WATCH_FAULT)",
    ))
}

/// Diagnostics for batch failures: a compact listing of the index directory
/// (entry count and the newest files) so CI logs carry the state the tantivy
/// error alone hides (flake #28 follow-up).
fn log_index_state(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("watch: index dir {} is unreadable", dir.display());
        return;
    };
    let mut files: Vec<(String, u64, u64)> = Vec::new();
    let mut total = 0_usize;
    for entry in entries.flatten() {
        total += 1;
        if files.len() >= 200 {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |delta| delta.as_secs());
        files.push((
            entry.file_name().to_string_lossy().into_owned(),
            meta.len(),
            modified,
        ));
    }
    files.sort_by_key(|(_, _, modified)| std::cmp::Reverse(*modified));
    let newest: Vec<String> = files
        .iter()
        .take(5)
        .map(|(name, size, modified)| format!("{name} ({size}B, mtime {modified})"))
        .collect();
    eprintln!(
        "watch: index dir {}: {total} entries; newest: {}",
        dir.display(),
        newest.join(", ")
    );
}

/// Logs one batch event (kept out of the consumer loop for readability).
fn report_batch_event(
    project_id: i64,
    policy: &BatchRetryPolicy,
    index_dir: &Path,
    event: BatchEvent,
) {
    match event {
        BatchEvent::LeaseRetry { attempt } => {
            if watch_debug() {
                eprintln!(
                    "watch: project {project_id} batch waits for the writer lease (attempt {attempt})"
                );
            }
        }
        BatchEvent::TransientRetry { attempt, error } => {
            eprintln!(
                "warning: watcher batch for project {project_id}: {error}; retrying ({attempt}/{})",
                policy.max_transient_attempts
            );
            log_index_state(index_dir);
        }
        BatchEvent::DroppedTransient { attempts, error } => {
            eprintln!(
                "warning: watcher batch for project {project_id} dropped after {attempts} transient retries: {error}"
            );
            log_index_state(index_dir);
        }
        BatchEvent::DroppedPermanent { error } => {
            eprintln!("warning: watcher batch for project {project_id}: {error}");
            log_index_state(index_dir);
        }
        BatchEvent::RootMissing => {
            eprintln!("watch: project {project_id} root is missing; stopping watcher");
        }
    }
}

/// Records a dropped batch as a terminal `error` job row so `status` surfaces
/// the lost edits next to per-file warnings (FR-19 pattern).
fn record_dropped_batch(db: &mut Db, project_id: i64, pending: &Batch, attempts: u32, error: &str) {
    record_dropped_paths(db, project_id, pending.paths(), attempts, error);
}

fn record_dropped_paths(
    db: &mut Db,
    project_id: i64,
    paths: &[PathBuf],
    attempts: u32,
    error: &str,
) {
    let warnings: Vec<serde_json::Value> = paths
        .iter()
        .map(|path| {
            json!({
                "path": path.display().to_string(),
                "message": format!(
                    "watcher batch dropped after {attempts} transient retries: {error}"
                ),
            })
        })
        .collect();
    let stats = json!({ "error": error, "warnings": warnings }).to_string();
    if let Err(record) =
        crate::store::repo::record_failed_batch(db.connection(), project_id, &stats)
    {
        eprintln!(
            "warning: cannot record dropped watcher batch for project {project_id}: {record}"
        );
    }
}

/// Existing failure diagnostics/status effects, called only on admitted CPU.
pub(crate) fn report_async_event(
    cache: &Path,
    project: &Project,
    event: BatchEvent,
    policy: BatchRetryPolicy,
    pending: Option<&WatchPayload>,
    open_registry: impl FnOnce() -> Result<Db>,
) -> Result<()> {
    let dropped = match &event {
        BatchEvent::DroppedTransient { attempts, error } => Some((*attempts, error.as_str())),
        BatchEvent::DroppedPermanent { error } => Some((0, error.as_str())),
        _ => None,
    };
    if let (Some((attempts, error)), Some(pending)) = (dropped, pending) {
        // Warnings, paths, Value nodes and concurrent encoded JSON are live
        // together. Reserve their conservative escaped peak before copying.
        let bytes = pending
            .paths
            .iter()
            .try_fold(0_u64, |bytes, path| {
                (path.as_os_str().len() as u64)
                    .checked_add(error.len() as u64)
                    .and_then(|n| n.checked_mul(48))
                    .and_then(|n| n.checked_add(2048))
                    .and_then(|n| bytes.checked_add(n))
            })
            .and_then(|n| n.checked_add(pending.account.bytes()))
            .ok_or_else(|| Error::Admission {
                message: "watcher warning capture overflow".into(),
            })?;
        pending.account.grow_shared(bytes)?;
        let mut db = open_registry()?;
        record_dropped_paths(&mut db, project.id, &pending.paths, attempts, error);
    }
    report_batch_event(
        project.id,
        &policy,
        &lifecycle::index_dir(cache, project.id),
        event,
    );
    Ok(())
}

/// Filters candidate paths through the same ignore stack as [`walk`],
/// caching one directory listing per parent (FR-15; R4).
struct IndexFilter {
    root: PathBuf,
    cache: PathBuf,
    config: Config,
    dirs: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl IndexFilter {
    /// Swaps the config after a reopen; cached directory verdicts were made
    /// under the old ignores and must be dropped (OQ-6).
    fn set_config(&mut self, config: &Config) {
        if self.config != *config {
            self.config = config.clone();
            self.dirs.clear();
        }
    }

    fn new(root: PathBuf, cache: PathBuf, config: &Config) -> Self {
        Self {
            root,
            cache,
            config: config.clone(),
            dirs: HashMap::new(),
        }
    }

    /// Canonical containment check: rejects paths that reach outside the
    /// root through a symlinked ancestor (FR-32).
    fn inside_root(&self, path: &Path) -> bool {
        path.canonicalize()
            .is_ok_and(|canonical| crate::platform::paths::is_under(&canonical, &self.root))
    }

    fn indexable(&mut self, path: &Path) -> bool {
        if !self.inside_root(path) || crate::platform::paths::is_under(path, &self.cache) {
            return false;
        }
        let Some(parent) = path.parent() else {
            return false;
        };
        if self
            .dirs
            .get(parent)
            .is_some_and(|files| files.contains(path))
        {
            return true;
        }
        // Cache miss: refresh the directory once so files created after the
        // first listing (or ignore-file edits) are still admitted (FR-15).
        let files = match walk::indexable_files(parent, &self.root, &self.config) {
            Ok(files) => files,
            Err(err) => {
                eprintln!("warning: watcher filter: {err}");
                HashSet::new()
            }
        };
        let indexable = files.contains(path);
        self.dirs.insert(parent.to_path_buf(), files);
        indexable
    }
}

/// Turns the raw event paths of one debounce window into a job batch:
/// directories are expanded now (not on receipt) so files that raced the
/// recursive watch registration are still found; missing paths stay as purge
/// markers, resolved by the incremental job (FR-16).
fn finalize(filter: &mut IndexFilter, raw: Batch) -> Vec<PathBuf> {
    let raw_paths = raw.paths.clone();
    let mut batch = Batch::new();
    for path in raw.into_vec() {
        if path.is_dir() {
            expand_dir(filter, &mut batch, &path);
        } else if path.exists() {
            if walk::is_markdown(&path) && filter.indexable(&path) {
                batch.push(path);
            }
        } else {
            batch.push(path);
        }
    }
    let finished = batch.into_vec();
    if watch_debug() {
        eprintln!("watch-debug: finalize raw={raw_paths:?} batch={finished:?}");
    }
    finished
}

pub(crate) struct WatchSelection {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) expansion_limited: bool,
    pub(crate) directories_only: bool,
}

/// Recheck raw pending paths under the current revision on admitted job CPU.
/// Stream the existing walker and charge retained selections before their copies.
pub(crate) fn refilter_accounted(
    project: &Project,
    cache: &Path,
    paths: &[PathBuf],
    config: &Config,
    mut capture: Option<&mut crate::index::job::CaptureBudget<'_>>,
) -> Result<WatchSelection> {
    if let Some(budget) = capture.as_deref_mut() {
        budget.reserve((paths.len() as u64).checked_mul(128).ok_or_else(|| {
            Error::Admission {
                message: "watch filter references overflow".into(),
            }
        })?)?;
    }
    let root = &project.canonical_root;
    let builder = walk::markdown_builder(root, root, config)?;
    let allowed = allowed_file_events(&builder, root, cache, paths, capture.as_deref_mut())?;
    let mut filtered = Batch::new();
    let mut expansion_limited = false;
    let mut directories_only = true;
    for path in paths {
        if let Some(budget) = capture.as_deref_mut() {
            budget.operation.check()?;
        }
        if path.is_dir() {
            expansion_limited |= expand_pending_directory(
                &builder,
                root,
                cache,
                path,
                config.watch,
                &mut filtered,
                capture.as_deref_mut(),
            )?;
        } else {
            directories_only = false;
            if !path.exists() || allowed.contains(path.as_path()) {
                // Missing files/directories keep their admitted position too;
                // the incremental planner resolves exact/subtree purge markers.
                push_filtered(path, &mut filtered, capture.as_deref_mut())?;
            }
        }
    }
    Ok(WatchSelection {
        paths: filtered.into_vec(),
        expansion_limited,
        directories_only,
    })
}

fn allowed_file_events(
    base: &ignore::WalkBuilder,
    root: &Path,
    cache: &Path,
    paths: &[PathBuf],
    mut capture: Option<&mut crate::index::job::CaptureBudget<'_>>,
) -> Result<HashSet<PathBuf>> {
    let mut wanted = Vec::new();
    for path in paths {
        if let Some(budget) = capture.as_deref_mut() {
            budget.operation.check()?;
        }
        if walk::is_markdown(path) {
            if let Some(budget) = capture.as_deref_mut() {
                budget.path(path)?;
            }
            wanted.push(path.clone());
        }
    }
    let mut allowed = HashSet::new();
    if wanted.is_empty() {
        return Ok(allowed);
    }
    wanted.sort_unstable();
    wanted.dedup();
    let expected = wanted.len();
    let operation = capture.as_ref().map(|budget| budget.operation);
    let scope_operation = operation.cloned();
    let mut builder = base.clone();
    // The first requested path at or after this entry must be underneath it
    // for the entry to lead to a requested file. Preserve raw event order
    // separately when assembling the actual batch.
    builder.filter_entry(move |entry| {
        if operation_stopped(scope_operation.as_ref()) {
            return true;
        }
        let path = entry.path();
        let directory = entry.file_type().is_some_and(|kind| kind.is_dir());
        let first = wanted.partition_point(|request| {
            request.as_path() < path || (directory && request.as_path() == path)
        });
        wanted
            .get(first)
            .is_some_and(|request| request.starts_with(path))
    });
    for entry in watch_walk(&builder, root, operation) {
        let path = match entry {
            Ok(path) => path,
            Err(error) => {
                eprintln!("warning: watcher filter: {error}");
                continue;
            }
        };
        if !crate::platform::paths::is_under(&path, cache) {
            if let Some(budget) = capture.as_deref_mut() {
                budget.path(&path)?;
            }
            allowed.insert(path);
            if allowed.len() == expected {
                break;
            }
        }
    }
    if let Some(operation) = operation {
        operation.check()?;
    }
    Ok(allowed)
}

fn operation_stopped(operation: Option<&SearchOperation>) -> bool {
    operation.is_some_and(|operation| operation.check().is_err())
}

fn watch_walk<'a>(
    builder: &ignore::WalkBuilder,
    root: &Path,
    operation: Option<&'a SearchOperation>,
) -> impl Iterator<Item = Result<PathBuf>> + use<'a> {
    let entries = builder
        .build()
        .take_while(move |_| !operation_stopped(operation));
    walk::markdown_paths(entries, root)
}

fn expand_pending_directory(
    base: &ignore::WalkBuilder,
    root: &Path,
    cache: &Path,
    dir: &Path,
    policy: WatchSettings,
    batch: &mut Batch,
    mut capture: Option<&mut crate::index::job::CaptureBudget<'_>>,
) -> Result<bool> {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut builder = base.clone();
    let depth_reached = Arc::new(AtomicBool::new(false));
    let reached = depth_reached.clone();
    let scope = dir.to_path_buf();
    let cache = cache.to_path_buf();
    let project = root.to_path_buf();
    let operation = capture.as_ref().map(|budget| budget.operation);
    let scope_operation = operation.cloned();
    // Start at the project root so ignore rules can exclude the event
    // directory itself. Skip enumeration of unrelated subtrees.
    builder.filter_entry(move |entry| {
        if operation_stopped(scope_operation.as_ref()) {
            return true;
        }
        let path = entry.path();
        if !scope.starts_with(path) && !path.starts_with(&scope) {
            return false;
        }
        let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
        if (is_directory && walk::is_pruned(&project, path))
            || crate::platform::paths::is_under(path, &cache)
        {
            return false;
        }
        let Ok(relative) = path.strip_prefix(&scope) else {
            return true;
        };
        if is_directory && relative.components().count() > policy.max_expand_depth {
            reached.store(true, Ordering::Relaxed);
            return false;
        }
        true
    });
    for entry in watch_walk(&builder, root, operation) {
        let path = match entry {
            Ok(path) => path,
            Err(error) => {
                eprintln!("warning: watcher filter: {error}");
                continue;
            }
        };
        if !batch.seen.contains(&path) && batch.paths.len() >= policy.max_expand_files {
            return Ok(true);
        }
        push_filtered(&path, batch, capture.as_deref_mut())?;
    }
    if let Some(operation) = operation {
        operation.check()?;
    }
    Ok(depth_reached.load(Ordering::Relaxed))
}

fn push_filtered(
    path: &Path,
    batch: &mut Batch,
    capture: Option<&mut crate::index::job::CaptureBudget<'_>>,
) -> Result<()> {
    if batch.seen.contains(path) {
        return Ok(());
    }
    if let Some(budget) = capture {
        budget.path(path)?;
    }
    batch.push(path.to_path_buf());
    Ok(())
}

/// Adds every indexable markdown file under `dir` (recursively, without
/// following symlinks) to `batch`, bounded by the captured watcher policy.
fn expand_dir(filter: &mut IndexFilter, batch: &mut Batch, dir: &Path) {
    expand_dir_inner(filter, batch, dir, 0);
}

fn expand_dir_inner(filter: &mut IndexFilter, batch: &mut Batch, dir: &Path, depth: usize) {
    let policy = filter.config.watch;
    if !filter.inside_root(dir) || crate::platform::paths::is_under(dir, &filter.cache) {
        return;
    }
    if depth > policy.max_expand_depth {
        eprintln!(
            "warning: watcher directory expansion reached depth {}",
            policy.max_expand_depth
        );
        return;
    }
    if batch.paths.len() >= policy.max_expand_files {
        eprintln!(
            "warning: watcher directory expansion reached {} files",
            policy.max_expand_files
        );
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if batch.paths.len() >= policy.max_expand_files {
            eprintln!(
                "warning: watcher directory expansion truncated at {} files",
                policy.max_expand_files
            );
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if !walk::is_pruned(&filter.root, &path) {
                expand_dir_inner(filter, batch, &path, depth + 1);
            }
        } else if walk::is_markdown(&path) && filter.indexable(&path) {
            batch.push(path);
        }
    }
}

/// Immutable provider and vector-source snapshot supplied by the composition root.
/// Config selects indexing limits; the prepared stages select embedding.
pub struct WatchRuntime {
    pub(crate) config: Arc<Config>,
    pub(crate) stages: Arc<crate::hybrid::HybridStages>,
    pub(crate) source: crate::vector_cache::VectorCacheSource,
}

impl WatchRuntime {
    /// Captures prepared dependencies without constructing providers or storage.
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        stages: Arc<crate::hybrid::HybridStages>,
        source: crate::vector_cache::VectorCacheSource,
    ) -> Self {
        Self {
            config,
            stages,
            source,
        }
    }
}

fn captured_retry_policy(context: &Mutex<WatchContext>) -> BatchRetryPolicy {
    context
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .config()
        .watch
        .retry_policy()
}

#[derive(Clone)]
enum WatchContext {
    Configured(Arc<Config>),
    Prepared(Arc<WatchRuntime>),
}

impl WatchContext {
    fn config(&self) -> &Arc<Config> {
        match self {
            Self::Configured(config) => config,
            Self::Prepared(runtime) => &runtime.config,
        }
    }

    fn validate_root(&self, cache: &Path) -> Result<()> {
        match self {
            Self::Configured(_) => Ok(()),
            Self::Prepared(runtime) => runtime.source.validate_root(cache),
        }
    }

    fn same_revision(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Configured(a), Self::Configured(b)) => Arc::ptr_eq(a, b),
            (Self::Prepared(a), Self::Prepared(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// One running watcher plus its incremental consumer.
struct WatcherEntry {
    guard: WatcherGuard,
    _consumer: ConsumerHandle,
    /// Open-time config, swappable on reopen so batch filtering follows
    /// `.docsbase.toml` edits without restarting notify/threads (OQ-6).
    context: Arc<Mutex<WatchContext>>,
    cache: PathBuf,
}

enum ConsumerHandle {
    Blocking { _join: std::thread::JoinHandle<()> },
    Async { _join: tokio::task::JoinHandle<()> },
}

#[path = "async_runtime.rs"]
mod async_runtime;
pub(crate) use async_runtime::{WatchPayload, WatchPrepared, merge_pending};
struct AsyncOwner {
    shared: Weak<crate::daemon::server::Shared>,
    runtime: tokio::runtime::Handle,
}

#[cfg(test)]
type BatchObserver = Arc<dyn Fn(BatchEvent) + Send + Sync>;
#[cfg(test)]
type CopyObserver = Arc<dyn Fn(&Arc<crate::search::budget::PayloadPermit>) + Send + Sync>;

/// Per-project watchers owned by the daemon (design §5).
pub struct Watchers {
    inner: Mutex<HashMap<i64, WatcherEntry>>,
    disabled: bool,
    async_owner: OnceLock<AsyncOwner>,
    close: crate::hybrid::work::Cancellation,
    #[cfg(test)]
    producer_retirement: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    manual_events: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    batch_observer: Mutex<Option<BatchObserver>>,
    #[cfg(test)]
    pending_copy: Mutex<Option<CopyObserver>>,
}

impl Default for Watchers {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            disabled: false,
            async_owner: OnceLock::new(),
            close: crate::hybrid::work::Cancellation::new(),
            #[cfg(test)]
            producer_retirement: Mutex::new(None),
            #[cfg(test)]
            manual_events: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            batch_observer: Mutex::new(None),
            #[cfg(test)]
            pending_copy: Mutex::new(None),
        }
    }
}

impl Watchers {
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn observe_pending_copy(&self, observer: CopyObserver) {
        *self.pending_copy.lock().unwrap() = Some(observer);
    }
    #[cfg(test)]
    fn pending_copy_entered(&self, account: &Arc<crate::search::budget::PayloadPermit>) {
        let observer = self.pending_copy.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer(account);
        }
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn consumer_finished(&self, project: i64) -> bool {
        self.map().get(&project).is_none_or(
            |WatcherEntry {
                 _consumer: consumer,
                 ..
             }| match consumer {
                ConsumerHandle::Blocking { _join: handle } => handle.is_finished(),
                ConsumerHandle::Async { _join: handle } => handle.is_finished(),
            },
        )
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn observe_batches(&self, observer: Arc<dyn Fn(BatchEvent) + Send + Sync>) {
        *self.batch_observer.lock().unwrap() = Some(observer);
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn use_manual_events(&self) {
        self.manual_events
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn deliver_test_path(&self, project: i64, path: PathBuf) {
        let sender = self.map()[&project].guard.retirement.wake.clone();
        sender.try_send(Some(path)).unwrap();
    }
    #[cfg(test)]
    pub(crate) fn emit_batch(&self, event: BatchEvent) {
        let observer = self.batch_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer(event);
        }
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn observe_producer_retirement(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        *self.producer_retirement.lock().unwrap() = Some(observer);
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn stop_project(&self, project: i64) {
        if let Some(entry) = self.map().get_mut(&project) {
            entry.guard.begin_retirement();
        }
    }
    pub(crate) fn close(&self) {
        self.close.cancel();
        for entry in self.map().values_mut() {
            entry.guard.begin_retirement();
        }
    }
    /// Called by the synchronous outer root only, after async/physical drain.
    pub(crate) fn join_retired(&self) {
        let entries = std::mem::take(&mut *self.map());
        for (_, mut entry) in entries {
            entry.guard.begin_retirement();
            if let Some(join) = entry.guard.forwarder.take() {
                let _ = join.join();
            }
        }
    }
    fn retire_matching(&self, project: i64, context: &Arc<Mutex<WatchContext>>) {
        if let Some(entry) = self.map().get_mut(&project)
            && Arc::ptr_eq(&entry.context, context)
        {
            entry.guard.begin_retirement();
        }
    }

    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Immutable owner policy. Disabled owners cannot start or revise watchers.
    #[must_use]
    pub fn with_enabled(enabled: bool) -> Self {
        Self {
            disabled: !enabled,
            ..Self::default()
        }
    }

    pub(crate) fn activate_async(&self, shared: Weak<crate::daemon::server::Shared>) -> Result<()> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| Error::internal_with_source("watcher runtime owner absent", error))?;
        self.async_owner
            .set(AsyncOwner { shared, runtime })
            .map_err(|_| Error::internal("watcher async owner already installed"))
    }

    /// Number of watched projects (`status`).
    #[must_use]
    pub fn count(&self) -> u64 {
        u64::try_from(
            self.map()
                .values()
                .filter(|entry| !entry.guard.finished())
                .count(),
        )
        .unwrap_or(u64::MAX)
    }

    /// True while a watcher thread is alive for `project_id`; a dead watcher
    /// is visible in `status` instead of silently dropping edits.
    #[must_use]
    pub fn watching(&self, project_id: i64) -> bool {
        self.map()
            .get(&project_id)
            .is_some_and(|entry| !entry.guard.finished())
    }

    /// Starts a watcher for `project` when it is indexed and not yet watched;
    /// returns `true` when a new watcher was started. `config` is the project
    /// configuration as of open time (OQ-6).
    /// Compatibility adapter; runtime callers use [`Self::ensure_with_settings`].
    ///
    /// # Errors
    /// Returns spawn errors; a failed start is not fatal to callers.
    pub fn ensure(
        self: &Arc<Self>,
        cache: &Path,
        project: &Project,
        config: Arc<Config>,
    ) -> Result<bool> {
        if self.disabled {
            return Ok(false);
        }
        self.ensure_with_settings(
            cache,
            project,
            config,
            &crate::store::settings::standalone()?,
        )
    }

    /// Starts a watcher with the owning process's complete storage policy.
    ///
    /// # Errors
    /// Invalid policy is rejected before watcher resources are created. Other
    /// errors match [`Self::ensure`].
    pub fn ensure_with_settings(
        self: &Arc<Self>,
        cache: &Path,
        project: &Project,
        config: Arc<Config>,
        db_settings: &Resolved<DbSettings>,
    ) -> Result<bool> {
        self.ensure_using(
            cache,
            project,
            WatchContext::Configured(config),
            db_settings,
        )
    }

    /// Starts a watcher using a complete, already prepared runtime snapshot.
    ///
    /// # Errors
    /// Source/root or storage policy admission before watcher resources, then
    /// the spawn failures of [`Self::ensure_with_settings`].
    pub fn ensure_with_runtime(
        self: &Arc<Self>,
        cache: &Path,
        project: &Project,
        runtime: Arc<WatchRuntime>,
        db_settings: &Resolved<DbSettings>,
    ) -> Result<bool> {
        self.ensure_using(cache, project, WatchContext::Prepared(runtime), db_settings)
    }

    fn ensure_using(
        self: &Arc<Self>,
        cache: &Path,
        project: &Project,
        context: WatchContext,
        db_settings: &Resolved<DbSettings>,
    ) -> Result<bool> {
        if self.disabled {
            return Ok(false);
        }
        context.validate_root(cache)?;
        DbSpec::validate(db_settings.settings(), &Capabilities::current())?;
        if project.status != ProjectStatus::Indexed {
            return Ok(false);
        }
        // A missing root is surfaced in `status`; watching it would only
        // produce retry noise (T50).
        if crate::daemon::registry::root_state(&project.canonical_root)
            == crate::daemon::registry::RootState::Missing
        {
            return Ok(false);
        }
        if self.close.is_cancelled() {
            return Err(Error::Admission {
                message: "watcher owner is shutting down".into(),
            });
        }
        {
            let mut map = self.map();
            if map
                .get(&project.id)
                .is_some_and(|entry| !entry.guard.finished())
            {
                return Ok(false);
            }
            map.remove(&project.id);
        }
        // Native setup runs outside the registry lock; same-project publication
        // is still serialized by the daemon's existing project reload authority.
        let batch_queue_capacity = context.config().watch.batch_queue_capacity;
        let slot = Arc::new(Mutex::new(context));
        let producer_slot = Arc::clone(&slot);
        let current = move || {
            producer_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .config()
                .clone()
        };
        let (guard, consumer) = if let Some(owner) = self.async_owner.get() {
            let (tx, rx) = tokio::sync::mpsc::channel(batch_queue_capacity);
            let shared = owner
                .shared
                .upgrade()
                .ok_or_else(|| Error::internal("watcher owner retired"))?;
            let cancel = crate::hybrid::work::Cancellation::new();
            let guard = spawn_watcher_sink_owned(
                project,
                cache,
                current,
                move |batch| tx.blocking_send(batch).map_err(|error| error.0),
                ProducerOwner::Daemon {
                    source: shared.watch_activity_source(),
                    #[cfg(test)]
                    before_native_drop: self.producer_retirement.lock().unwrap().clone(),
                    #[cfg(test)]
                    manual_events: self
                        .manual_events
                        .load(std::sync::atomic::Ordering::Relaxed),
                },
                cancel.clone(),
            )?;
            let consumer = owner.runtime.spawn(async_runtime::consume(
                owner.shared.clone(),
                project.clone(),
                slot.clone(),
                rx,
                Arc::downgrade(self),
                self.close.clone(),
                cancel,
            ));
            (guard, ConsumerHandle::Async { _join: consumer })
        } else {
            let (tx, rx) = std::sync::mpsc::sync_channel(batch_queue_capacity);
            let guard = spawn_watcher_using(project, cache, current, tx)?;
            let consumer = spawn_consumer(
                cache.to_path_buf(),
                project.clone(),
                slot.clone(),
                db_settings.clone(),
                rx,
                Arc::downgrade(self),
            )?;
            (guard, ConsumerHandle::Blocking { _join: consumer })
        };
        let mut map = self.map();
        if self.close.is_cancelled() || map.contains_key(&project.id) {
            return Ok(false);
        }
        map.insert(
            project.id,
            WatcherEntry {
                guard,
                _consumer: consumer,
                context: slot,
                cache: canonicalize_allow_missing(cache),
            },
        );
        Ok(true)
    }

    /// Updates a configured compatibility watcher after reopen.
    /// Returns false if absent or prepared; a prepared watcher must be revised
    /// through [`Self::revise_with_runtime`] so its owner cannot be discarded.
    pub fn revise(&self, project_id: i64, config: Arc<Config>) -> bool {
        if self.disabled {
            return false;
        }
        let slot = self
            .map()
            .get(&project_id)
            .map(|entry| entry.context.clone());
        let Some(slot) = slot else {
            return false;
        };
        let mut context = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*context, WatchContext::Prepared(_)) {
            return false;
        }
        *context = WatchContext::Configured(config);
        true
    }

    /// Atomically replaces the complete prepared revision of an existing watcher.
    ///
    /// # Errors
    /// A foreign source/cache root is rejected without replacing the old revision.
    pub fn revise_with_runtime(
        &self,
        cache: &Path,
        project_id: i64,
        runtime: Arc<WatchRuntime>,
    ) -> Result<bool> {
        if self.disabled {
            return Ok(false);
        }
        runtime.source.validate_root(cache)?;
        let existing = self
            .map()
            .get(&project_id)
            .filter(|entry| !entry.guard.retiring() && !entry.guard.finished())
            .map(|entry| (entry.context.clone(), entry.cache.clone()));
        let Some((slot, root)) = existing else {
            return Ok(false);
        };
        if canonicalize_allow_missing(cache) != root {
            return Err(Error::Admission {
                message: "watcher belongs to a different cache root".to_owned(),
            });
        }
        runtime.source.validate_root(&root)?;
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = WatchContext::Prepared(runtime);
        Ok(true)
    }

    fn remove(&self, project_id: i64) {
        self.map().remove(&project_id);
    }

    #[cfg(test)]
    pub(crate) fn runtime_for_test(
        &self,
        project_id: i64,
    ) -> Option<(
        Arc<Config>,
        Arc<crate::hybrid::HybridStages>,
        crate::vector_cache::VectorCacheSource,
    )> {
        let slot = self.map().get(&project_id)?.context.clone();
        let context = slot.lock().unwrap_or_else(PoisonError::into_inner);
        match &*context {
            WatchContext::Configured(_) => None,
            WatchContext::Prepared(runtime) => Some((
                runtime.config.clone(),
                runtime.stages.clone(),
                runtime.source.clone(),
            )),
        }
    }

    fn map(&self) -> MutexGuard<'_, HashMap<i64, WatcherEntry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn spawn_consumer(
    cache: PathBuf,
    project: Project,
    context: Arc<Mutex<WatchContext>>,
    db_settings: Resolved<DbSettings>,
    rx: Receiver<Vec<PathBuf>>,
    watchers: Weak<Watchers>,
) -> Result<std::thread::JoinHandle<()>> {
    let project_id = project.id;
    std::thread::Builder::new()
        .name(format!("docsbase-watch-{project_id}"))
        .spawn(move || {
            let mut db = match Db::open_with_settings(&cache, &db_settings) {
                Ok(db) => db,
                Err(err) => {
                    eprintln!(
                        "warning: watcher for project {project_id} cannot open db: {err}"
                    );
                    if let Some(watchers) = watchers.upgrade() {
                        watchers.remove(project_id);
                    }
                    return;
                }
            };
            let mut pending = Batch::new();
            while let Ok(batch) = rx.recv() {
                for path in batch {
                    pending.push(path);
                }
                let policy = captured_retry_policy(&context);
                let index_dir = lifecycle::index_dir(&cache, project_id);
                let mut report =
                    |event: BatchEvent| report_batch_event(project_id, &policy, &index_dir, event);
                let outcome = run_pending_batch(
                    &mut pending,
                    &policy,
                    |paths| {
                        if let Some(err) = injected_watch_fault(&project.canonical_root, &cache) {
                            return Err(err);
                        }
                        run_batch_using(&mut db, &cache, &project, paths, || {
                            // Hold the slot only while cloning the complete snapshot.
                            context.lock().unwrap_or_else(PoisonError::into_inner).clone()
                        })
                    },
                    |pending| {
                        while let Ok(extra) = rx.try_recv() {
                            for path in extra {
                                pending.push(path);
                            }
                        }
                    },
                    std::thread::sleep,
                    || {
                        crate::daemon::registry::root_state(&project.canonical_root)
                            == crate::daemon::registry::RootState::Missing
                    },
                    &mut report,
                );
                match outcome {
                    BatchOutcome::Done(stats) => {
                        eprintln!(
                            "watcher: project {project_id} refreshed {} doc(s), {} chunk(s), {} error(s)",
                            stats.docs, stats.chunks, stats.errors
                        );
                        pending = Batch::new();
                    }
                    BatchOutcome::Dropped { attempts, error } => {
                        record_dropped_batch(&mut db, project.id, &pending, attempts, &error);
                        pending = Batch::new();
                    }
                    BatchOutcome::RootMissing => {
                        if let Some(watchers) = watchers.upgrade() {
                            watchers.remove(project_id);
                        }
                        return;
                    }
                }
            }
            if let Some(watchers) = watchers.upgrade() {
                watchers.remove(project_id);
            }
        })
        .map_err(|err| Error::internal_with_source("spawn watcher thread", err))
}

#[cfg(test)]
fn run_batch(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    batch: &[PathBuf],
    mut current_config: impl FnMut() -> Arc<Config>,
) -> Result<JobStats> {
    run_batch_using(db, cache, project, batch, || {
        WatchContext::Configured(current_config())
    })
}

pub(crate) fn admit_incremental(db: &Db, project: &Project) -> Result<()> {
    // Incremental chunks are only valid for an index built with the current
    // schema; a stale or errored project must be rebuilt by a full index
    // first (T24). Full outcome publication (or the legacy mark_indexed helper)
    // advances the stamp; incremental completion leaves it unchanged.
    let current = registry::project_by_id(db, project.id)?.ok_or_else(|| Error::Project {
        message: format!("project {} disappeared from the registry", project.id),
        instruction: None,
    })?;
    if current.status != ProjectStatus::Indexed
        || current.schema_version != crate::store::migrations::PROJECT_SCHEMA_VERSION
    {
        return Err(Error::Project {
            message: format!(
                "project {} needs a full rebuild before incremental updates",
                project.id
            ),
            instruction: Some("run `docsbase index` to rebuild".to_owned()),
        });
    }
    Ok(())
}

fn run_batch_using(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    batch: &[PathBuf],
    mut current_context: impl FnMut() -> WatchContext,
) -> Result<JobStats> {
    admit_incremental(db, project)?;
    // The writer handle is opened per batch: holding it between batches would
    // block full index jobs on the tantivy lock.
    lifecycle::with_writer_lease(cache, project.id, || {
        let context = current_context();
        context.validate_root(cache)?;
        context.validate_root(db.cache_root())?;
        // Revoke historical completeness before filtering or a possibly
        // recreating/failing index open. Keep status for the existing retries.
        crate::index::outcome::invalidate(db, project.id)?;
        let config = context.config();
        // The producer may have admitted these paths before a reopen. Retry
        // from the original pending paths, with no cached directory verdicts.
        let mut raw = Batch::new();
        for path in batch {
            raw.push(path.clone());
        }
        let filtered = finalize(
            &mut IndexFilter::new(
                project.canonical_root.clone(),
                canonicalize_allow_missing(cache),
                config,
            ),
            raw,
        );
        let mut check_config = || {
            if context.same_revision(&current_context()) {
                Ok(())
            } else {
                // Existing bounded transient retry keeps the raw pending
                // paths and releases the lease before preparing them again.
                Err(Error::internal(
                    "watcher config changed while preparing batch",
                ))
            }
        };
        let mut index = IndexHandle::open_or_create_with_settings(
            &lifecycle::index_dir(cache, project.id),
            config.index_writer,
        )?;
        // A rejected stale attempt may have recreated the index. Even an
        // ignored-only retry must finish that full rebuild for allowed files
        // outside its raw batch before reporting success.
        if filtered.is_empty() && !index.was_recreated() {
            check_config()?;
            return crate::index::outcome::run(
                db,
                project,
                config,
                crate::index::outcome::SurveyScope::Subset,
                |_| Ok(JobStats::default()),
            );
        }
        let stats = match &context {
            WatchContext::Configured(_) => {
                run_incremental_checked(db, &mut index, project, &filtered, config, check_config)?
            }
            WatchContext::Prepared(runtime) => {
                let embedding = crate::index::job::IndexEmbedding::new(
                    &runtime.source,
                    runtime.stages.embedder.as_deref(),
                );
                crate::index::job::run_incremental_checked_with_embedding(
                    db,
                    &mut index,
                    project,
                    &filtered,
                    config,
                    &embedding,
                    check_config,
                )?
            }
        };
        Ok(stats)
    })
}

#[cfg(test)]
mod runtime_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::job::run_full;
    use crate::store::repo;
    use std::cell::Cell;
    use std::fs;
    use std::sync::mpsc::sync_channel;
    use tempfile::TempDir;

    struct Env {
        db: Db,
        cache: TempDir,
        _root: TempDir,
        project: Project,
    }

    impl Env {
        fn new() -> Self {
            let (env, index) = Self::with_writer();
            drop(index); // Windows needs the writer released before consumer open.
            env
        }

        fn with_writer() -> (Self, IndexHandle) {
            let cache = TempDir::new().expect("cache");
            let root = TempDir::new().expect("root");
            fs::create_dir(root.path().join(".git")).expect("project boundary");
            fs::create_dir(root.path().join("bbb")).expect("bbb");
            fs::write(root.path().join("bbb/b.md"), "betawidget").expect("seed");
            let mut db = Db::open(cache.path()).expect("db");
            let project = registry::ensure_project(&mut db, root.path()).expect("project");
            let mut index =
                IndexHandle::open_or_create(&lifecycle::index_dir(cache.path(), project.id))
                    .expect("index");
            run_full(&mut db, &mut index, &project, &Config::default()).expect("seed index");
            registry::mark_indexed(&db, project.id).expect("indexed");
            (
                Self {
                    cache,
                    _root: root,
                    db,
                    project,
                },
                index,
            )
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.project.canonical_root.join(rel)
        }

        fn producer_batch(&self, paths: &[PathBuf], config: &Config) -> Vec<PathBuf> {
            let mut raw = Batch::new();
            for path in paths {
                raw.push(path.clone());
            }
            finalize(
                &mut IndexFilter::new(
                    self.project.canonical_root.clone(),
                    self.cache.path().canonicalize().expect("cache root"),
                    config,
                ),
                raw,
            )
        }

        fn assert_search_count(&self, query: &str, expected: usize) {
            let index = crate::index::tantivy_index::ReadIndex::open(&lifecycle::index_dir(
                self.cache.path(),
                self.project.id,
            ))
            .expect("open search index");
            assert_eq!(index.search(query, 10).expect("search").len(), expected);
        }

        fn run(&mut self, paths: &[PathBuf], config: &Arc<Config>) -> JobStats {
            run_batch(
                &mut self.db,
                self.cache.path(),
                &self.project,
                paths,
                || Arc::clone(config),
            )
            .expect("batch")
        }

        fn states(&self) -> Vec<repo::DocState> {
            repo::doc_states(self.db.connection(), self.project.id).expect("states")
        }

        fn generation(&self) -> i64 {
            repo::index_generation(self.db.connection(), self.project.id).expect("generation")
        }
    }

    fn ignoring_bbb() -> Arc<Config> {
        Arc::new(Config {
            ignores: vec!["bbb/**".to_owned()],
            ..Config::default()
        })
    }

    #[test]
    fn daemon_forwarding_keeps_directories_and_discards_already_ignored_events() {
        let env = Env::new();
        fs::create_dir(env.path("aaa")).unwrap();
        fs::write(env.path(".docsbaseignore"), "aaa/\nignored.md\n").unwrap();
        fs::write(env.path("ignored.md"), "# ignored").unwrap();
        fs::write(env.path("replaced-directory"), "file now").unwrap();
        let mut events = [
            Ok(Some(env.path("aaa"))),
            Ok(Some(env.path("ignored.md"))),
            Ok(Some(env.path("bbb"))),
            Ok(Some(env.path("removed"))),
            Ok(Some(env.path("replaced-directory/child.md"))),
            Err(RecvTimeoutError::Disconnected),
        ]
        .into_iter();
        let received = std::cell::RefCell::new(Vec::new());
        let config = Arc::new(Config::default());
        forward_batches(
            BatchFilter::RetainDirectoryEvents {
                root: env.project.canonical_root.clone(),
                cache: env.cache.path().canonicalize().unwrap(),
            },
            || Arc::clone(&config),
            |_| events.next().unwrap(),
            |batch| {
                received.borrow_mut().extend(batch);
                Ok(())
            },
            None,
            Instant::now,
        );
        assert_eq!(
            received.into_inner(),
            [
                env.path("bbb"),
                env.path("removed"),
                env.path("replaced-directory/child.md")
            ]
        );
    }

    #[test]
    fn watcher_expansion_applies_v2_file_and_depth_limits() {
        let env = Env::new();
        let global = TempDir::new().unwrap();
        fs::write(global.path().join("config.toml"), "config_version = 2\n").unwrap();
        let root = &env.project.canonical_root;
        for (file, text) in [
            ("arrivals/root.md", "one"),
            ("arrivals/nested/next.md", "two"),
            ("arrivals/nested/deeper/skip.md", "three"),
            ("arrivals/.release.md", "visible by explicit rule"),
        ] {
            let path = env.path(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fs::write(env.path(".docsbaseignore"), "!/arrivals/.release.md\n").unwrap();
        let configure = |files| {
            fs::write(
                env.path(".docsbase.toml"),
                format!(
                    "config_version = 2\n[watch]\nmax_expand_files = {files}\nmax_expand_depth = 1\n"
                ),
            )
            .unwrap();
            Config::load_from(global.path(), Some(root)).unwrap()
        };
        let raw_paths = [env.path("arrivals"), env.path("bbb/b.md")];
        let async_paths = |config| {
            let selection =
                super::refilter_accounted(&env.project, env.cache.path(), &raw_paths, config, None)
                    .unwrap();
            assert!(selection.expansion_limited);
            selection.paths
        };
        let config = configure(4);
        for paths in [
            env.producer_batch(&raw_paths, &config),
            async_paths(&config),
        ] {
            assert_eq!(paths.len(), 4);
            assert!(paths.contains(&env.path("arrivals/root.md")));
            assert!(paths.contains(&env.path("arrivals/nested/next.md")));
            assert!(paths.contains(&env.path("arrivals/.release.md")));
            assert_eq!(paths.last(), Some(&env.path("bbb/b.md")));
        }
        let config = configure(1);
        for paths in [
            env.producer_batch(&raw_paths, &config),
            async_paths(&config),
        ] {
            assert_eq!(paths.len(), 2);
            assert_eq!(paths.last(), Some(&env.path("bbb/b.md")));
        }
        fs::write(env.path(".docsbaseignore"), "/arrivals/\n").unwrap();
        let selection =
            super::refilter_accounted(&env.project, env.cache.path(), &raw_paths, &config, None)
                .unwrap();
        assert_eq!(selection.paths, [env.path("bbb/b.md")]);
        assert!(!selection.expansion_limited);
    }

    #[test]
    fn public_watcher_preserves_rendezvous_for_a_polling_receiver() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = crate::daemon::registry::ensure_project(&mut db, root.path()).unwrap();
        let config = Arc::new(Mutex::new(Arc::new(Config::default())));
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        let _watcher = spawn_watcher(&project, cache.path(), config, tx).unwrap();
        let changed = project.canonical_root.join("changed.md");
        fs::write(&changed, "# Change").unwrap();

        let until = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(batch) = rx.try_recv() {
                assert!(batch.contains(&changed), "{batch:?}");
                break;
            }
            assert!(
                Instant::now() < until,
                "polling receiver never received the batch"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn blocking_watcher_retires_while_batch_receiver_stays_full() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = crate::daemon::registry::ensure_project(&mut db, root.path()).unwrap();
        let config = Arc::new(Config::default());
        let cancel = crate::hybrid::work::Cancellation::new();
        let sender_cancel = cancel.clone();
        let (attempted, attempt) = std::sync::mpsc::sync_channel(1);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let queued = vec![root.path().join("already-queued.md")];
        send.send(queued.clone()).unwrap();
        let mut guard = spawn_watcher_sink_owned(
            &project,
            cache.path(),
            move || config.clone(),
            move |batch| {
                let _ = attempted.try_send(());
                send_cancellable(&send, batch, &sender_cancel)
            },
            ProducerOwner::Managed,
            cancel,
        )
        .unwrap();
        fs::write(root.path().join("changed.md"), "# Change").unwrap();
        attempt.recv_timeout(Duration::from_secs(10)).unwrap();

        guard.begin_retirement();
        let until = Instant::now() + Duration::from_secs(5);
        while !guard.finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            guard.finished(),
            "full batch queue prevented producer retirement"
        );
        assert_eq!(receive.try_recv().unwrap(), queued);
    }

    #[test]
    fn disabled_owner_blocks_every_watcher_entry_before_validation() {
        let env = Env::new();
        let watchers = Arc::new(Watchers::with_enabled(false));
        let config = Arc::new(Config::default());
        let mut raw = env.db.settings().settings().clone();
        raw.wal_attempts = 0;
        let invalid = Resolved::new(raw, env.db.settings().provenance().clone());
        let runtime = Arc::new(WatchRuntime::new(
            Arc::clone(&config),
            Arc::new(crate::hybrid::HybridStages::default()),
            crate::vector_cache::VectorCacheSource::Legacy(config.hybrid_cfg.cache.limits()),
        ));
        assert!(
            !watchers
                .ensure(env.cache.path(), &env.project, Arc::clone(&config))
                .unwrap()
        );
        assert!(
            !watchers
                .ensure_with_settings(
                    env.cache.path(),
                    &env.project,
                    Arc::clone(&config),
                    &invalid
                )
                .unwrap()
        );
        assert!(
            !watchers
                .ensure_with_runtime(
                    env.cache.path(),
                    &env.project,
                    Arc::clone(&runtime),
                    &invalid
                )
                .unwrap()
        );
        assert!(!watchers.revise(env.project.id, config));
        assert!(
            !watchers
                .revise_with_runtime(env.cache.path(), env.project.id, runtime)
                .unwrap()
        );
        assert_eq!(watchers.count(), 0);
        assert!(!watchers.watching(env.project.id));
    }

    #[test]
    fn invalid_store_policy_does_not_start_watcher_resources() {
        let env = Env::new();
        let valid = env.db.settings();
        let mut raw = valid.settings().clone();
        raw.wal_attempts = 0;
        let imported = Resolved::new(raw, valid.provenance().clone());
        let watchers = Arc::new(Watchers::new());
        let error = watchers
            .ensure_with_settings(
                env.cache.path(),
                &env.project,
                Arc::new(Config::default()),
                &imported,
            )
            .expect_err("reject before watcher creation");
        assert!(matches!(error, Error::Admission { .. }));
        assert_eq!(watchers.count(), 0);
    }

    #[test]
    fn pending_batch_keeps_its_timers_and_uses_the_reopened_filter() {
        let env = Env::new();
        let root = &env.project.canonical_root;
        let global = TempDir::new().expect("global config");
        fs::write(global.path().join("config.toml"), "config_version = 2\n").unwrap();
        let project_file = root.join(".docsbase.toml");
        fs::write(
            &project_file,
            "config_version = 2\n[watch]\ndebounce_ms = 80\nmax_debounce_ms = 100\n",
        )
        .unwrap();
        fs::write(root.join("a.md"), "first event").unwrap();
        fs::write(root.join("c.md"), "next batch").unwrap();
        let resolved = Config::load_from(global.path(), Some(root)).unwrap();
        let filter = IndexFilter::new(
            root.clone(),
            env.cache.path().canonicalize().unwrap(),
            &resolved,
        );
        let current = std::cell::RefCell::new(Arc::new(resolved));
        let clock = Cell::new(Instant::now());
        let results = std::cell::RefCell::new(Vec::new());
        let ms = Duration::from_millis;
        let mut events = [
            (None, Ok(Some(root.join("a.md")))),
            (Some(ms(80)), Ok(Some(root.join("bbb/b.md")))),
            (Some(ms(25)), Err(RecvTimeoutError::Timeout)),
            (None, Ok(Some(root.join("c.md")))),
            (Some(ms(10)), Err(RecvTimeoutError::Timeout)),
            (None, Err(RecvTimeoutError::Disconnected)),
        ]
        .into_iter();

        forward_batches(
            BatchFilter::ExpandDirectories(Box::new(filter)),
            || current.borrow().clone(),
            |timeout| {
                let (expected_timeout, event) = events.next().expect("next watcher read");
                assert_eq!(timeout, expected_timeout);
                if timeout == Some(ms(80)) {
                    fs::write(
                        &project_file,
                        "config_version = 2\nignores = ['bbb/**']\n[watch]\ndebounce_ms = 10\nmax_debounce_ms = 20\n",
                    )
                    .unwrap();
                    let reopened = Config::load_from(global.path(), Some(root)).unwrap();
                    *current.borrow_mut() = Arc::new(reopened);
                    clock.set(clock.get() + ms(75));
                }
                event
            },
            |batch| {
                results.borrow_mut().push(batch);
                Ok(())
            },
            None,
            || clock.get(),
        );
        assert_eq!(
            results.into_inner(),
            vec![vec![root.join("a.md")], vec![root.join("c.md")]]
        );
    }

    #[test]
    fn queued_paths_use_reopened_config() {
        let env = Env::new();
        let seed = repo::doc_states(env.db.connection(), env.project.id).expect("seed state");
        let old = Arc::new(Config::default());
        let paths = env.producer_batch(&[env.path("bbb/b.md")], &old);
        assert_eq!(paths, vec![env.path("bbb/b.md")], "v1 admitted the path");
        let (tx, rx) = sync_channel(2);
        tx.send(paths).expect("queue old batch");

        // The not-yet-started consumer is an instance-local gate: the queued
        // v1 verdict is guaranteed to precede config publication and new bytes.
        let slot = Arc::new(Mutex::new(WatchContext::Configured(old)));
        *slot.lock().expect("slot") = WatchContext::Configured(ignoring_bbb());
        fs::write(env.path("bbb/b.md"), "betawidget9").expect("new ignored bytes");
        fs::write(env.path("a.md"), "alphawidget9").expect("allowed bytes");
        tx.send(vec![env.path("a.md")]).expect("coalesced update");
        drop(tx);
        let consumer = spawn_consumer(
            env.cache.path().to_path_buf(),
            env.project.clone(),
            slot,
            env.db.settings().clone(),
            rx,
            Weak::new(),
        )
        .expect("consumer");
        consumer.join().expect("batch terminal");

        env.assert_search_count("alphawidget9", 1);
        env.assert_search_count("betawidget9", 0);
        env.assert_search_count("betawidget", 1);
        let states = repo::doc_states(env.db.connection(), env.project.id).expect("states");
        assert!(
            states.contains(&seed[0]),
            "ignored row/hash must remain unchanged"
        );
    }

    #[test]
    fn newly_ignored_file_creates_no_pending_row() {
        let mut env = Env::new();
        let path = env.path("bbb/new.md");
        fs::write(&path, "newwidget9").expect("new file");
        let queued = env.producer_batch(&[path], &Config::default());
        assert_eq!(queued.len(), 1);
        let before = env.states();
        let generation = env.generation();
        assert_eq!(env.run(&queued, &ignoring_bbb()), JobStats::default());
        assert_eq!(env.states(), before);
        assert_eq!(env.generation(), generation);
        env.assert_search_count("newwidget9", 0);
    }

    #[test]
    fn missing_ignored_file_is_purged() {
        let mut env = Env::new();
        let path = env.path("bbb/b.md");
        fs::remove_file(&path).expect("delete");
        assert_eq!(env.run(&[path], &ignoring_bbb()).removed, 1);
        assert_eq!(env.states().len(), 0);
        env.assert_search_count("betawidget", 0);
    }

    #[test]
    fn missing_ignored_directory_is_purged() {
        let mut env = Env::new();
        let extra = env.path("bbb/c.md");
        fs::write(&extra, "secondwidget").expect("extra file");
        assert_eq!(env.run(&[extra], &Arc::new(Config::default())).docs, 1);
        let dir = env.path("bbb");
        fs::remove_dir_all(&dir).expect("delete subtree");
        assert_eq!(env.run(&[dir], &ignoring_bbb()).removed, 2);
        assert_eq!(env.states().len(), 0);
        env.assert_search_count("betawidget", 0);
        env.assert_search_count("secondwidget", 0);
    }

    #[test]
    fn config_change_during_preparation_leaves_state_untouched() {
        let mut env = Env::new();
        let before = env.states();
        let generation = env.generation();
        let path = env.path("bbb/b.md");
        fs::write(&path, "betawidget9").expect("new bytes");
        let old = Arc::new(Config::default());
        let new = ignoring_bbb();
        let reads = Cell::new(0);
        let err = run_batch(&mut env.db, env.cache.path(), &env.project, &[path], || {
            let read = reads.get();
            reads.set(read + 1);
            Arc::clone(if read == 0 { &old } else { &new })
        })
        .expect_err("reopen overtook preparation");
        assert!(
            err.to_string()
                .contains("config changed while preparing batch")
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(env.states(), before, "no stale pending hash or rows");
        assert_eq!(env.generation(), generation);
        env.assert_search_count("betawidget", 1);
        env.assert_search_count("betawidget9", 0);
    }

    #[test]
    fn search_assertion_reads_committed_index_without_acquiring_a_writer() {
        // Keep the fixture's initial writer; do not reopen another writer.
        let (env, _writer) = Env::with_writer();
        // This observer must work while the real writer remains alive, using
        // only committed data and without repair or writer ownership.
        env.assert_search_count("betawidget", 1);
        env.assert_search_count("unindexedwidget", 0);
    }

    #[test]
    fn stale_full_recovery_preserves_sql_until_retry() {
        use crate::index::tantivy_index::REBUILD_MARKER;
        let mut env = Env::new();
        let before = env.states();
        let generation = env.generation();
        let paths = [env.path("bbb/b.md"), env.path("a.md")];
        fs::write(&paths[0], "betawidget9").expect("new ignored bytes");
        fs::write(&paths[1], "alphawidget9").expect("new allowed bytes");
        let index_dir = lifecycle::index_dir(env.cache.path(), env.project.id);
        fs::write(index_dir.join(REBUILD_MARKER), "").expect("force recovery");
        let old = Arc::new(Config::default());
        let new = ignoring_bbb();
        let reads = Cell::new(0);
        let err = run_batch(&mut env.db, env.cache.path(), &env.project, &paths, || {
            let read = reads.get();
            reads.set(read + 1);
            Arc::clone(if read == 0 { &old } else { &new })
        })
        .expect_err("reject obsolete full payload");
        assert!(
            err.to_string()
                .contains("config changed while preparing batch")
        );
        assert_eq!(
            reads.get(),
            2,
            "full recovery also validates after preparation"
        );
        assert_eq!(env.states(), before, "SQL wipe must wait for admission");
        assert_eq!(env.generation(), generation);
        assert!(index_dir.join(REBUILD_MARKER).exists());

        assert_eq!(env.run(&paths, &new).docs, 1);
        assert!(!index_dir.join(REBUILD_MARKER).exists());
        env.assert_search_count("alphawidget9", 1);
        env.assert_search_count("betawidget9", 0);
        assert_eq!(env.states().len(), 1);
    }

    #[test]
    fn ignored_only_retry_finishes_full_recovery() {
        use crate::index::tantivy_index::REBUILD_MARKER;

        let mut env = Env::new();
        let old = Arc::new(Config::default());
        let new = ignoring_bbb();
        let allowed = env.path("a.md");
        fs::write(&allowed, "alphawidget9").expect("allowed bytes");
        assert_eq!(env.run(&[allowed], &old).docs, 1);
        let before = env.states();
        let generation = env.generation();
        let queued = [env.path("bbb/b.md")];
        fs::write(&queued[0], "betawidget9").expect("new ignored bytes");
        let index_dir = lifecycle::index_dir(env.cache.path(), env.project.id);
        fs::write(index_dir.join(REBUILD_MARKER), "").expect("force recovery");
        let reads = Cell::new(0);
        let err = run_batch(&mut env.db, env.cache.path(), &env.project, &queued, || {
            let read = reads.get();
            reads.set(read + 1);
            Arc::clone(if read == 0 { &old } else { &new })
        })
        .expect_err("reject obsolete full payload");
        assert!(
            err.to_string()
                .contains("config changed while preparing batch")
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(env.states(), before, "rejected recovery preserves SQL");
        assert_eq!(env.generation(), generation);
        assert!(index_dir.join(REBUILD_MARKER).exists());

        let stats = env.run(&queued, &new);
        assert!(
            !index_dir.join(REBUILD_MARKER).exists(),
            "an ignored-only retry must finish the outstanding rebuild"
        );
        assert_eq!(
            stats.docs, 1,
            "recover the allowed document outside the batch"
        );
        env.assert_search_count("alphawidget9", 1);
        env.assert_search_count("betawidget9", 0);
        let documents = env.states();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].rel_path, "a.md");
    }

    #[test]
    fn config_retry_refilters_raw_pending_and_coalesces_new_paths() {
        let mut env = Env::new();
        let seed = env.states();
        let ignored = env.path("bbb/b.md");
        let allowed = env.path("a.md");
        fs::write(&ignored, "betawidget9").expect("ignored edit");
        fs::write(&allowed, "alphawidget9").expect("allowed edit");
        let old = Arc::new(Config::default());
        let new = ignoring_bbb();
        let reads = Cell::new(0);
        let drains = Cell::new(0);
        let attempts = Cell::new(0);
        let mut events = Vec::new();
        let mut pending = Batch::new();
        pending.push(ignored.clone());
        let outcome = run_pending_batch(
            &mut pending,
            &BatchRetryPolicy::default(),
            |paths| {
                attempts.set(attempts.get() + 1);
                run_batch(&mut env.db, env.cache.path(), &env.project, paths, || {
                    let read = reads.get();
                    reads.set(read + 1);
                    Arc::clone(if read == 0 { &old } else { &new })
                })
            },
            |pending| {
                if drains.get() == 1 {
                    pending.push(allowed.clone());
                }
                drains.set(drains.get() + 1);
            },
            |_| {}, // Logical retry; no wall-clock backoff in this regression.
            || false,
            &mut |event| events.push(event),
        );
        let BatchOutcome::Done(stats) = outcome else {
            panic!("retry must succeed");
        };
        assert_eq!(stats.docs, 1);
        assert_eq!(attempts.get(), 2);
        assert_eq!(pending.paths(), &[ignored, allowed]);
        assert!(matches!(
            events.as_slice(),
            [BatchEvent::TransientRetry { attempt: 1, .. }]
        ));
        assert!(env.states().contains(&seed[0]));
        env.assert_search_count("alphawidget9", 1);
        env.assert_search_count("betawidget9", 0);
    }

    #[test]
    fn admitted_payload_keeps_bytes_prepared_before_reopen() {
        let mut env = Env::new();
        let path = env.path("bbb/b.md");
        fs::write(&path, "betawidget8").expect("pre-reopen bytes");
        let old = Arc::new(Config::default());
        let new = ignoring_bbb();
        let slot = Mutex::new(Arc::clone(&old));
        let reads = Cell::new(0);
        let stats = run_batch(
            &mut env.db,
            env.cache.path(),
            &env.project,
            std::slice::from_ref(&path),
            || {
                // Model a reopen immediately after the guard cloned v1.
                let captured = Arc::clone(&slot.lock().expect("snapshot"));
                if reads.get() == 1 {
                    *slot.lock().expect("publish") = Arc::clone(&new);
                    fs::write(&path, "betawidget9").expect("post-reopen bytes");
                }
                reads.set(reads.get() + 1);
                captured
            },
        )
        .expect("admitted payload may finish");
        assert_eq!(stats.docs, 1);
        assert_eq!(reads.get(), 2);
        assert!(Arc::ptr_eq(&slot.into_inner().expect("slot"), &new));
        env.assert_search_count("betawidget8", 1);
        env.assert_search_count("betawidget9", 0);
        assert_eq!(env.run(&[path], &new), JobStats::default());
        env.assert_search_count("betawidget8", 1);
    }
}
