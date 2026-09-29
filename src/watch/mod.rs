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
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::Config;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::index::job::run_incremental_with;
use crate::index::tantivy_index::IndexHandle;
use crate::index::walk;
use crate::store::Db;
use crate::store::models::{Project, ProjectStatus};

/// Quiet window after the last event; extends while events keep coming, so a
/// git-checkout-like burst becomes one incremental job (FR-15, NFR-4).
pub const DEBOUNCE: Duration = Duration::from_millis(1_500);

/// Hard upper bound from the first event of a batch to its flush, so
/// continuous churn cannot starve indexing (NFR-4).
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(2_000);

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
    tx: Sender<Vec<PathBuf>>,
) -> Result<WatcherGuard> {
    let root = project.canonical_root.clone();
    let cache = cache.to_path_buf();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
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
                let batch_config = config
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                filter.set_config(&batch_config);
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
struct Batch {
    paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
}

impl Batch {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, path: PathBuf) {
        if self.seen.insert(path.clone()) {
            self.paths.push(path);
        }
    }

    fn into_vec(self) -> Vec<PathBuf> {
        self.paths
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
    batch.into_vec()
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
        let mut map = self.map();
        if map.contains_key(&project.id) {
            return Ok(false);
        }
        let (tx, rx) = std::sync::mpsc::channel();
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
                loop {
                    // Clone and release the slot: a reopen must not stall
                    // behind a long incremental batch.
                    let config = config
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    match run_batch(&mut db, &cache, &project, &config, &pending.paths) {
                        Ok(stats) => {
                            eprintln!(
                                "watcher: project {project_id} refreshed {} doc(s), {} chunk(s), {} error(s)",
                                stats.docs, stats.chunks, stats.errors
                            );
                            pending = Batch::new();
                            break;
                        }
                        // A full index job holds the lease: keep the batch and
                        // retry instead of dropping fresh edits (FR-16).
                        Err(err) if lifecycle::is_lease_conflict(&err) => {
                            std::thread::sleep(LEASE_RETRY);
                        }
                        Err(err) => {
                            eprintln!(
                                "warning: watcher batch for project {project_id}: {err}"
                            );
                            pending = Batch::new();
                            break;
                        }
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
