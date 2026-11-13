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
use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::{JobStats, run_incremental_checked};
use crate::index::tantivy_index::IndexHandle;
use crate::index::walk;
use crate::store::Db;
use crate::store::models::{Project, ProjectStatus};
use crate::store::settings::{DbSettings, DbSpec};
use serde_json::json;

/// Quiet window after the last event; extends while events keep coming, so a
/// git-checkout-like burst becomes one incremental job (FR-15, NFR-4).
pub const DEBOUNCE: Duration = Duration::from_millis(1_500);

/// Hard upper bound from the first event of a batch to its flush, so
/// continuous churn cannot starve indexing (NFR-4).
pub const MAX_DEBOUNCE: Duration = Duration::from_secs(2);

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
    spawn_watcher_using(
        project,
        cache,
        move || {
            config
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        },
        tx,
    )
}

fn spawn_watcher_using(
    project: &Project,
    cache: &Path,
    current_config: impl Fn() -> Arc<Config> + Send + 'static,
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
    let initial = current_config();
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
                let batch_config = current_config();
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

/// Immutable provider and vector-source snapshot supplied by the composition root.
/// Config selects indexing limits; the prepared stages select embedding.
pub struct WatchRuntime {
    config: Arc<Config>,
    stages: Arc<crate::hybrid::HybridStages>,
    source: crate::vector_cache::VectorCacheSource,
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
    _guard: WatcherGuard,
    _consumer: std::thread::JoinHandle<()>,
    /// Open-time config, swappable on reopen so batch filtering follows
    /// `.docsbase.toml` edits without restarting notify/threads (OQ-6).
    context: Arc<Mutex<WatchContext>>,
    cache: PathBuf,
}

/// Per-project watchers owned by the daemon (design §5).
#[derive(Default)]
pub struct Watchers {
    inner: Mutex<HashMap<i64, WatcherEntry>>,
    disabled: bool,
}

impl Watchers {
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
        let mut map = self.map();
        if map.contains_key(&project.id) {
            return Ok(false);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(MAX_WATCH_EVENTS);
        let slot = Arc::new(Mutex::new(context));
        let producer_slot = Arc::clone(&slot);
        let guard = spawn_watcher_using(
            project,
            cache,
            move || {
                producer_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .config()
                    .clone()
            },
            tx,
        )?;
        let consumer = spawn_consumer(
            cache.to_path_buf(),
            project.clone(),
            Arc::clone(&slot),
            db_settings.clone(),
            rx,
            Arc::downgrade(self),
        )?;
        map.insert(
            project.id,
            WatcherEntry {
                _guard: guard,
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

fn run_batch_using(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    batch: &[PathBuf],
    mut current_context: impl FnMut() -> WatchContext,
) -> Result<JobStats> {
    // Incremental chunks are only valid for an index built with the current
    // schema; a stale or errored project must be rebuilt by a full index
    // first (T24). Full outcome publication (or the legacy mark_indexed helper)
    // advances the stamp; incremental completion leaves it unchanged.
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
        let mut index = IndexHandle::open_or_create(&lifecycle::index_dir(cache, project.id))?;
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
