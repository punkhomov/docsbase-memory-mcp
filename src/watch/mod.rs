//! Debounced per-project watcher (FR-15, FR-16, NFR-4; R4, SC-6).
//!
//! [`spawn_watcher`] watches a project root, coalesces bursts with a 1.5 s
//! quiet window (flushing at most 2 s after the first event) and forwards
//! batches of changed markdown paths over a channel; [`Watchers`] owns one
//! watcher plus a consumer thread per project inside the daemon. Events below
//! the cache root (tantivy index state) are ignored so indexing cannot feed
//! itself (R4).
//!
//! Directory events are expanded at flush time and deleted paths may denote
//! directories; the incremental job purges whole subtrees for those (FR-16).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::Config;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_incremental_with};
use crate::index::tantivy_index::IndexHandle;
use crate::index::walk;
use crate::store::Db;
use crate::store::models::{Project, ProjectStatus};
use serde_json::json;

/// Quiet window after the last event; extends while events keep coming, so a
/// git-checkout-like burst becomes one incremental job (FR-15, NFR-4).
pub const DEBOUNCE: Duration = Duration::from_millis(1_500);

/// Hard upper bound from the first event of a batch to its flush, so
/// continuous churn cannot starve indexing (NFR-4).
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(2_000);

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

/// Bound for the notify and batch channels of one watcher (T46).
const MAX_WATCH_EVENTS: usize = 4096;

/// Delay between writer-lease retries of a pending batch.
const LEASE_RETRY: Duration = Duration::from_millis(250);

/// Stops the underlying watcher when dropped.
pub struct WatcherGuard {
    _watcher: RecommendedWatcher,
    _forwarder: std::thread::JoinHandle<()>,
}

/// Watches `project.canonical_root` and sends deduplicated batches of
/// changed paths through `tx` (FR-15). `config` is a swappable open-time
/// configuration slot: reopens replace it without restarting the watcher
/// (OQ-6).
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
    let root = project.canonical_root.clone();
    // Canonicalize once: notify reports verbatim paths on Windows, while the
    // configured cache dir may use the short/drive spelling. The dir itself
    // may not exist yet (tests watch an in-root cache), so tolerate that.
    let cache = canonicalize_allow_missing(cache);
    // Bounded with blocking send: the notify callback applies backpressure
    // instead of growing without bound; no path is dropped (T46).
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel(MAX_WATCH_EVENTS);
    let watch_cache = cache.clone();
    let watch_root = root.clone();
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
            match std::fs::symlink_metadata(&path) {
                // Never follow symlinks out of the root (FR-32).
                Ok(meta) if meta.file_type().is_symlink() => {}
                Ok(meta) => {
                    if meta.is_dir() || walk::is_markdown(&path) {
                        let _ = event_tx.send(path);
                    }
                }
                // Deleted: `.md` files purge, other paths may be directories
                // whose subtree must be purged (FR-16).
                Err(_) => {
                    let _ = event_tx.send(path);
                }
            }
        }
    })
    .map_err(|err| Error::internal_with_source("create watcher", err))?;
    watcher
        .watch(&project.canonical_root, RecursiveMode::Recursive)
        .map_err(|err| {
            Error::internal_with_source(
                format!("watch {}: {err}", project.canonical_root.display()),
                err,
            )
        })?;

    let project_id = project.id;
    let filter_root = root;
    let filter_cache = cache;
    let initial = config
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let forwarder = std::thread::Builder::new()
        .name(format!("docsbase-debounce-{project_id}"))
        .spawn(move || {
            let mut filter = IndexFilter::new(filter_root, filter_cache, &initial);
            drop(initial);
            while let Ok(first) = event_rx.recv() {
                let deadline = Instant::now() + MAX_DEBOUNCE;
                let mut raw = Batch::new();
                raw.push(first);
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match event_rx.recv_timeout(DEBOUNCE.min(remaining)) {
                        Ok(path) => raw.push(path),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            let _ = tx.send(finalize(&mut filter, raw));
                            return;
                        }
                    }
                }
                // Apply the config as of the END of the debounce window: a
                // reopen that lands while events are still arriving must
                // govern this batch (OQ-6; Windows CI showed the old
                // start-of-window capture losing freshly written ignores).
                let batch_config = config
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                filter.set_config(&batch_config);
                let batch = finalize(&mut filter, raw);
                if !batch.is_empty() && tx.send(batch).is_err() {
                    return;
                }
            }
        })
        .map_err(|err| Error::internal_with_source("spawn debounce thread", err))?;
    Ok(WatcherGuard {
        _watcher: watcher,
        _forwarder: forwarder,
    })
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

/// Bounded transient retries per batch (flake #28: tantivy merge thread races
/// the next commit on `.managed.json`; see tantivy issue #2847).
const BATCH_MAX_TRANSIENT_ATTEMPTS: u32 = 3;

/// First transient backoff; multiplied by 3 per attempt (100/300/900 ms).
const BATCH_TRANSIENT_BACKOFF: Duration = Duration::from_millis(100);

/// Retry policy for one pending watcher batch.
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
        Self {
            max_transient_attempts: BATCH_MAX_TRANSIENT_ATTEMPTS,
            lease_retry: LEASE_RETRY,
            transient_backoff: BATCH_TRANSIENT_BACKOFF,
        }
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
                lease_attempts += 1;
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
fn injected_watch_fault(root: &Path, cache: &Path) -> Option<Error> {
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
    let warnings: Vec<serde_json::Value> = pending
        .paths()
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

/// Maximum files and depth for one directory expansion, bounding the work a
/// single directory-create event can trigger (R4).
const EXPAND_MAX_FILES: usize = 10_000;
const EXPAND_MAX_DEPTH: usize = 32;

/// Adds every indexable markdown file under `dir` (recursively, without
/// following symlinks) to `batch`, bounded by [`EXPAND_MAX_FILES`] and
/// [`EXPAND_MAX_DEPTH`].
fn expand_dir(filter: &mut IndexFilter, batch: &mut Batch, dir: &Path) {
    expand_dir_inner(filter, batch, dir, 0);
}

fn expand_dir_inner(filter: &mut IndexFilter, batch: &mut Batch, dir: &Path, depth: usize) {
    if depth > EXPAND_MAX_DEPTH || batch.paths.len() >= EXPAND_MAX_FILES {
        return;
    }
    if !filter.inside_root(dir) || crate::platform::paths::is_under(dir, &filter.cache) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if batch.paths.len() >= EXPAND_MAX_FILES {
            eprintln!("warning: watcher directory expansion truncated at {EXPAND_MAX_FILES} files");
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

/// One running watcher plus its incremental consumer.
struct WatcherEntry {
    _guard: WatcherGuard,
    _consumer: std::thread::JoinHandle<()>,
    /// Open-time config, swappable on reopen so batch filtering follows
    /// `.docsbase.toml` edits without restarting notify/threads (OQ-6).
    config: Arc<Mutex<Arc<Config>>>,
}

/// Per-project watchers owned by the daemon (design §5).
#[derive(Default)]
pub struct Watchers {
    inner: Mutex<HashMap<i64, WatcherEntry>>,
}

impl Watchers {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of watched projects (`status`).
    #[must_use]
    pub fn count(&self) -> u64 {
        u64::try_from(self.map().len()).unwrap_or(u64::MAX)
    }

    /// True while a watcher thread is alive for `project_id`; a dead watcher
    /// is visible in `status` instead of silently dropping edits.
    #[must_use]
    pub fn watching(&self, project_id: i64) -> bool {
        self.map().contains_key(&project_id)
    }

    /// Starts a watcher for `project` when it is indexed and not yet watched;
    /// returns `true` when a new watcher was started. `config` is the project
    /// configuration as of open time (OQ-6).
    ///
    /// # Errors
    /// Returns spawn errors; a failed start is not fatal to callers.
    pub fn ensure(
        self: &Arc<Self>,
        cache: &Path,
        project: &Project,
        config: Arc<Config>,
    ) -> Result<bool> {
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
        let mut map = self.map();
        if map.contains_key(&project.id) {
            return Ok(false);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(MAX_WATCH_EVENTS);
        let slot = Arc::new(Mutex::new(config));
        let guard = spawn_watcher(project, cache, Arc::clone(&slot), tx)?;
        let consumer = spawn_consumer(
            cache.to_path_buf(),
            project.clone(),
            Arc::clone(&slot),
            rx,
            Arc::downgrade(self),
        )?;
        map.insert(
            project.id,
            WatcherEntry {
                _guard: guard,
                _consumer: consumer,
                config: slot,
            },
        );
        Ok(true)
    }

    /// Swaps the config an existing watcher filters with (OQ-6 reopen);
    /// returns `false` when the project is not watched.
    pub fn revise(&self, project_id: i64, config: Arc<Config>) -> bool {
        let slot = self
            .map()
            .get(&project_id)
            .map(|entry| Arc::clone(&entry.config));
        match slot {
            Some(slot) => {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = config;
                true
            }
            None => false,
        }
    }

    fn remove(&self, project_id: i64) {
        self.map().remove(&project_id);
    }

    fn map(&self) -> MutexGuard<'_, HashMap<i64, WatcherEntry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn spawn_consumer(
    cache: PathBuf,
    project: Project,
    config: Arc<Mutex<Arc<Config>>>,
    rx: Receiver<Vec<PathBuf>>,
    watchers: Weak<Watchers>,
) -> Result<std::thread::JoinHandle<()>> {
    let project_id = project.id;
    std::thread::Builder::new()
        .name(format!("docsbase-watch-{project_id}"))
        .spawn(move || {
            let mut db = match Db::open(&cache) {
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
                let policy = BatchRetryPolicy::default();
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
                        // Clone and release the slot: a reopen must not stall
                        // behind a long incremental batch.
                        let config = config
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        run_batch(&mut db, &cache, &project, &config, paths)
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

fn run_batch(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    config: &Config,
    batch: &[PathBuf],
) -> Result<crate::index::job::JobStats> {
    // Incremental chunks are only valid for an index built with the current
    // schema; a stale or errored project must be rebuilt by a full index
    // first (T24). The stamp itself is only written by `registry::mark_indexed`.
    let current = registry::project_by_id(db, project.id)?.ok_or_else(|| Error::Project {
        message: format!("project {} disappeared from the registry", project.id),
        instruction: None,
    })?;
    if current.status != ProjectStatus::Indexed
        || current.schema_version != crate::store::migrations::SCHEMA_VERSION
    {
        return Err(Error::Project {
            message: format!(
                "project {} needs a full rebuild before incremental updates",
                project.id
            ),
            instruction: Some("run `docsbase index` to rebuild".to_owned()),
        });
    }
    // The writer handle is opened per batch: holding it between batches would
    // block full index jobs on the tantivy lock.
    lifecycle::with_writer_lease(cache, project.id, || {
        let mut index = IndexHandle::open_or_create(&lifecycle::index_dir(cache, project.id))?;
        let stats = run_incremental_with(db, &mut index, project, batch, config)?;
        registry::set_status(db, project.id, ProjectStatus::Indexed)?;
        Ok(stats)
    })
}
