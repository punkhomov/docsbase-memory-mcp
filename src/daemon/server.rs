//! IPC server: handshake, sessions and tool routing (FR-3, FR-7, FR-8, FR-12,
//! FR-33; I2, I6, I8).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

use crate::config::schema::Resolved;
use crate::config::{Config, paths};
use crate::daemon::request::{self, Activity};
use crate::daemon::session::SessionRegistry;
use crate::daemon::tools;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::hybrid::config::{Ranking, embedding_model_id};
use crate::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRequest};
use crate::hybrid::jobs as hybrid_jobs;
use crate::ipc::framing::{Frame, FrameReader};
use crate::ipc::protocol::{self, PROTOCOL_VERSION, Request, Response};
use crate::platform::Stream;
use crate::search::metadata::MetadataSource;
use crate::store::Db;
use crate::store::migrations;
use crate::store::read_pool::ReadPool;
use crate::store::settings::{DbSettings, ReadPoolSettings};
use crate::watch::Watchers;

#[cfg(test)]
#[path = "search_phase_tests.rs"]
mod search_phase_tests;

#[path = "scoped.rs"]
mod scoped;

/// Daemon state shared by all connections.
pub struct Shared {
    /// Cache root being served.
    pub cache: PathBuf,
    db: Mutex<Db>,
    // Independent of the DB mutex: child construction never waits for a query
    // just to obtain its startup policy.
    db_settings: Resolved<DbSettings>,
    readers: Arc<ReadPool>,
    payload_budget: crate::search::budget::PayloadBudget,
    /// Live sessions.
    pub sessions: SessionRegistry,
    /// Per-project filesystem watchers (FR-15).
    pub watchers: Arc<Watchers>,
    /// Set by the first `StopDaemon`; later stops return immediately (FR-6).
    stopping: AtomicBool,
    /// Global config snapshot taken at daemon start (OQ-6).
    global_config: Config,
    /// Why the global config could not be loaded; surfaced by `status`.
    global_config_error: Option<String>,
    /// Fingerprint of the global config file at daemon start (OQ-6).
    global_config_stamp: Option<u64>,
    /// Per-project config/stage slots; the map lock only protects lookup.
    project_states: Mutex<HashMap<i64, ProjectSlot>>,
    /// Dense candidate cache keyed by project and index generation (FR-16).
    pub dense: DenseCache,
}

type ProjectSlot = Arc<ProjectCell>;

/// Stable per-project authority; model construction uses only the reload lock.
#[derive(Default)]
struct ProjectCell {
    state: Mutex<ProjectState>,
    reload: Mutex<()>,
    preparation: RwLock<()>,
}

impl ProjectCell {
    fn with_preparation<T>(
        &self,
        expected: &Arc<Config>,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let _preparation = self.preparation_lease(expected)?;
        work()
    }

    fn preparation_lease(&self, expected: &Arc<Config>) -> Result<RwLockReadGuard<'_, ()>> {
        let preparation = self
            .preparation
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let current = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .config
            .as_ref()
            .is_some_and(|config| Arc::ptr_eq(config, expected));
        if !current {
            return Err(Error::Project {
                message: "project configuration changed before search preparation".to_owned(),
                instruction: Some("retry search".to_owned()),
            });
        }
        Ok(preparation)
    }
}

/// A config Arc is its revision identity. Stages always belong to that Arc.
#[derive(Default)]
struct ProjectState {
    config: Option<Arc<Config>>,
    stages: Option<Arc<HybridStages>>,
}

impl Shared {
    /// Opens the registry database and prepares shared state.
    /// Compatibility adapter; production passes policies to [`Self::open_with_policies`].
    ///
    /// # Errors
    /// Returns [`Error::Internal`]/[`Error::Admission`] from [`Db::open`].
    pub fn open(cache: &Path) -> Result<Arc<Self>> {
        Self::open_with_settings(cache, &crate::store::settings::standalone()?)
    }

    /// Compatibility adapter with explicit DB policy and owner-resolved pool policy.
    ///
    /// # Errors
    /// Returns storage, configuration and startup errors as in [`Self::open`].
    pub fn open_with_settings(cache: &Path, settings: &Resolved<DbSettings>) -> Result<Arc<Self>> {
        Self::open_with_policies(
            cache,
            settings,
            &crate::store::settings::standalone_read_pool()?,
        )
    }

    /// Constructs writer and prepared readers using explicit owner policies.
    /// Compatibility adapters resolve omitted policies; production calls here.
    ///
    /// # Errors
    /// Invalid policies are rejected before filesystem access. Other failures
    /// match [`Self::open_with_settings`], including reader preparation errors.
    pub fn open_with_policies(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
    ) -> Result<Arc<Self>> {
        use crate::config::schema::{Capabilities, SettingsSpec};
        crate::store::settings::DbSpec::validate(settings.settings(), &Capabilities::current())?;
        crate::store::settings::ReadPoolSpec::validate(
            pool_settings.settings(),
            &Capabilities::current(),
        )?;
        let db = Db::open_with_settings(cache, settings)?;
        let readers = Arc::new(ReadPool::prepare(cache, settings, pool_settings)?);
        let orphans = crate::store::repo::fail_orphan_sync_jobs(db.connection())?;
        if orphans > 0 {
            eprintln!("warning: {orphans} interrupted sync job(s) marked as error");
        }
        // Stamp before loading: a write racing this window then shows up as
        // "changed" (a harmless false positive) instead of being silently
        // adopted as the baseline (a false negative).
        let stamp = global_config_stamp();
        let (global_config, global_config_error) = match Config::global() {
            Ok(config) => (config, None),
            Err(err) => {
                eprintln!("warning: global config unavailable: {err}");
                (Config::default(), Some(err.to_string()))
            }
        };
        let dense_limits = DenseCacheLimits {
            vector_cache_max_bytes: global_config.hybrid_cfg.cache.max_bytes,
            ..DenseCacheLimits::default()
        };
        let payload_budget =
            crate::search::budget::PayloadBudget::new(global_config.search.clone())?;
        let shared = Arc::new(Self {
            cache: cache.to_path_buf(),
            db: Mutex::new(db),
            db_settings: settings.clone(),
            readers,
            payload_budget,
            sessions: SessionRegistry::new(),
            watchers: Arc::new(Watchers::new()),
            stopping: AtomicBool::new(false),
            global_config,
            global_config_error,
            global_config_stamp: stamp,
            project_states: Mutex::new(HashMap::new()),
            dense: DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache, dense_limits),
        });
        match shared.start_watchers() {
            Ok(started) if started > 0 => eprintln!("watching {started} indexed project(s)"),
            Ok(_) => {}
            Err(err) => eprintln!("warning: cannot start watchers: {err}"),
        }
        Ok(shared)
    }

    /// Prepared metadata readers shared by search phases and ANN refresh,
    /// independent of the writer mutex.
    #[must_use]
    pub fn read_pool(&self) -> &ReadPool {
        &self.readers
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn project_slot(&self, project_id: i64) -> ProjectSlot {
        Arc::clone(
            self.project_states
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(project_id)
                .or_default(),
        )
    }

    /// Resolved hybrid stages of `project`; built on first use and refreshed
    /// when the project is reopened. Feature-off + enabled hybrid is an
    /// explicit [`Error::Admission`] (FR-14) - no silent BM25-only fallback.
    ///
    /// # Errors
    /// Admission (feature/provider/key/window) and config read errors.
    pub fn project_stages(
        &self,
        project: &crate::store::models::Project,
    ) -> Result<Arc<HybridStages>> {
        Ok(self.project_runtime(project)?.1)
    }

    fn project_runtime(
        &self,
        project: &crate::store::models::Project,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)> {
        self.project_runtime_with(project, |config| {
            crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
        })
    }

    // Private construction seam: tests can pause a real cache miss without
    // changing the provider registry or using process-global hooks.
    fn project_runtime_with<F>(
        &self,
        project: &crate::store::models::Project,
        mut build: F,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)>
    where
        F: FnMut(&Config) -> Result<HybridStages>,
    {
        let slot = self.project_slot(project.id);
        loop {
            let config = {
                let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
                let config = self.config_in(&mut state, project)?;
                if let Some(stages) = &state.stages {
                    return Ok((config, Arc::clone(stages)));
                }
                config
            };
            // Construction can load models: do not hold the project lock.
            let outcome = build(&config);
            let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state
                .config
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &config))
            {
                // Neither an obsolete result nor its error may escape.
                continue;
            }
            if let Some(stages) = &state.stages {
                return Ok((config, Arc::clone(stages)));
            }
            let stages = Arc::new(outcome?);
            state.stages = Some(Arc::clone(&stages));
            return Ok((config, stages));
        }
    }

    /// Enqueues the open-time backfill sync job when the embedding stage is
    /// on and the project is not fully covered (FR-15); cached chunks make a
    /// fully covered project a cheap no-op.
    ///
    /// # Errors
    /// Stage admission errors (propagated to the open) and registry failures.
    fn schedule_backfill(self: &Arc<Self>, project: &crate::store::models::Project) -> Result<()> {
        let (config, stages) = self.project_runtime(project)?;
        if stages.embedder.is_none()
            || project.status != crate::store::models::ProjectStatus::Indexed
        {
            return Ok(());
        }
        let db = self.db();
        let Some(coverage) = hybrid_jobs::coverage(&db, &config.hybrid_cfg, project.id)? else {
            return Ok(());
        };
        if coverage.cached >= coverage.total {
            return Ok(());
        }
        if crate::store::repo::active_sync_job(db.connection(), project.id)?.is_some() {
            return Ok(());
        }
        let (job_id, created) = crate::daemon::tools::enqueue_sync(&db, project)?;
        if created {
            spawn_sync_job(Arc::clone(self), job_id, project.id, config);
        }
        Ok(())
    }

    fn global_error(&self) -> Option<Error> {
        self.global_config_error.as_ref().map(|err| {
            Error::internal(format!(
                "global config is invalid ({err}); fix it and run `docsbase daemon stop`"
            ))
        })
    }

    /// Project config layered on the daemon's global snapshot; used for
    /// projects that are not registered yet (auto-index registration).
    ///
    /// # Errors
    /// Returns the captured global-config error, or config read/parse errors.
    pub fn config_for_root(&self, root: &Path) -> Result<Config> {
        if let Some(err) = self.global_error() {
            return Err(err);
        }
        Config::for_project(&self.global_config, root)
    }

    fn config_in(
        &self,
        state: &mut ProjectState,
        project: &crate::store::models::Project,
    ) -> Result<Arc<Config>> {
        if let Some(config) = &state.config {
            return Ok(Arc::clone(config));
        }
        let config = Arc::new(self.config_for_root(&project.canonical_root)?);
        self.dense
            .set_vector_cache_max_bytes(project.id, config.hybrid_cfg.cache.max_bytes);
        state.config = Some(Arc::clone(&config));
        Ok(config)
    }

    /// Configuration of `project` as of the last time it was opened; loaded on
    /// first use (OQ-6).
    ///
    /// # Errors
    /// Returns config read/parse errors.
    pub fn project_config(&self, project: &crate::store::models::Project) -> Result<Arc<Config>> {
        let slot = self.project_slot(project.id);
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.config_in(&mut state, project)
    }

    /// Warms the ANN index for `project` after a successful sync/backfill
    /// (FR-8; design §10). Runs in the background; search keeps streaming
    /// exact while the build runs. Stale on-disk indexes are rejected by the
    /// key (generation/digest/format), so no explicit invalidation is needed
    /// here (FR-4). A no-op when the project has no embedding stage.
    /// The handle completes refresh orchestration, not the separate ANN build;
    /// dropping it leaves the worker detached.
    fn refresh_ann_after_sync(
        self: &Arc<Self>,
        project: &crate::store::models::Project,
    ) -> Option<std::thread::JoinHandle<()>> {
        self.refresh_ann_after_sync_with(project, |_, _, _| Ok(()))
    }

    /// Private metadata observation seam; production performs the actual warm.
    fn refresh_ann_after_sync_with<F>(
        self: &Arc<Self>,
        project: &crate::store::models::Project,
        before_metadata: F,
    ) -> Option<std::thread::JoinHandle<()>>
    where
        F: FnOnce(&Db, i64, &str) -> Result<()> + Send + 'static,
    {
        self.refresh_ann_after_sync_before_with(project, || {}, before_metadata)
    }

    // Private scheduling seam for deterministic delayed-revision tests.
    fn refresh_ann_after_sync_before_with<B, F>(
        self: &Arc<Self>,
        project: &crate::store::models::Project,
        before_admission: B,
        before_metadata: F,
    ) -> Option<std::thread::JoinHandle<()>>
    where
        B: FnOnce() + Send + 'static,
        F: FnOnce(&Db, i64, &str) -> Result<()> + Send + 'static,
    {
        let config = self.project_config(project).ok()?;
        let slot = self.project_slot(project.id);
        let embed_cfg = config.hybrid_cfg.embedding.as_ref()?;
        let warm = config.hybrid_cfg.enabled && config.hybrid_cfg.ranking == Ranking::Rrf;
        let model_id = embedding_model_id(embed_cfg);
        let cache_path = self.cache.clone();
        let dense = self.dense.snapshot();
        let readers = Arc::clone(&self.readers);
        let project = project.clone();
        let project_id = project.id;
        Some(std::thread::spawn(move || {
            before_admission();
            if warm {
                let result = (|| {
                    let generation = {
                        let reader = readers.try_acquire()?;
                        slot.with_preparation(&config, || {
                            lifecycle::with_project_read_lease(&cache_path, project_id, || {
                                before_metadata(&reader, project_id, &model_id)?;
                                tools::snapshot_info(&reader, &project, None)
                            })
                        })?
                    };
                    // The scalar lease is gone before entering the shared flight.
                    // Its factory has no personal query budget or outer READ gate.
                    let source = MetadataSource::Prepared(&readers);
                    dense.warm_with_metadata_admitted(
                        DenseRequest {
                            cache_root: &cache_path,
                            project_id,
                            generation,
                            model_id: &model_id,
                        },
                        &crate::hybrid::work::Cancellation::new(),
                        || tools::capture_dense(source, &project, generation),
                        || slot.preparation_lease(&config),
                    )
                })();
                if let Err(err) = result {
                    eprintln!("warning: ann warm failed for project {project_id}: {err}");
                }
            }
            if let Err(err) = dense.cleanup(project_id) {
                eprintln!("warning: ann cleanup failed for project {project_id}: {err}");
            }
            if let Err(err) = dense.cleanup_disk(project_id) {
                eprintln!("warning: ann disk cleanup failed: {err}");
            }
        }))
    }

    /// Re-reads the project config; called when a session opens the project so
    /// edits are picked up without a daemon restart (OQ-6).
    ///
    /// # Errors
    /// Returns config read/parse errors.
    pub fn open_project_config(
        &self,
        project: &crate::store::models::Project,
    ) -> Result<Arc<Config>> {
        self.open_project_config_with(project, |config| {
            crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
        })
    }

    fn open_project_config_with<F>(
        &self,
        project: &crate::store::models::Project,
        build: F,
    ) -> Result<Arc<Config>>
    where
        F: FnOnce(&Config) -> Result<HybridStages>,
    {
        self.open_project_config_observing_contention(project, build, || {})
    }

    fn open_project_config_observing_contention<F, O>(
        &self,
        project: &crate::store::models::Project,
        build: F,
        on_publication_contention: O,
    ) -> Result<Arc<Config>>
    where
        F: FnOnce(&Config) -> Result<HybridStages>,
        O: FnOnce(),
    {
        let slot = self.project_slot(project.id);
        // Only same-project reloads wait for construction. Existing runtime
        // reads and unrelated projects keep their last coherent state.
        let _reload = slot.reload.lock().unwrap_or_else(PoisonError::into_inner);
        let config = Arc::new(self.config_for_root(&project.canonical_root)?);
        let stages = Arc::new(build(&config)?);
        // Never wait for active preparation while retaining the state mutex.
        let _publication = match slot.preparation.try_write() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                on_publication_contention();
                slot.preparation
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
            }
        };
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = state.config.as_deref() != Some(config.as_ref());
        if changed {
            self.dense.invalidate_memory(project.id);
        }
        // Both fallible preparation steps precede publication: a rejected
        // reload leaves the last valid runtime and its policies intact.
        state.config = Some(Arc::clone(&config));
        state.stages = Some(stages);
        self.dense
            .set_vector_cache_max_bytes(project.id, config.hybrid_cfg.cache.max_bytes);
        if changed
            && !self.watchers.revise(project.id, Arc::clone(&config))
            && let Err(err) = self.watchers.ensure_with_settings(
                &self.cache,
                project,
                Arc::clone(&config),
                &self.db_settings,
            )
        {
            eprintln!("warning: cannot watch project {}: {err}", project.id);
        }
        Ok(config)
    }

    /// Starts watchers for every indexed project in the registry (daemon
    /// startup); returns how many were started.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the registry cannot be listed.
    fn start_watchers(&self) -> Result<usize> {
        let db = self.db();
        let mut started = 0;
        for project in registry::list_projects(&db)? {
            match self.ensure_watcher(&project) {
                Ok(true) => started += 1,
                Ok(false) => {}
                Err(err) => {
                    eprintln!("warning: cannot watch project {}: {err}", project.id);
                }
            }
        }
        Ok(started)
    }

    /// Starts a watcher for an indexed project using its open-time config.
    ///
    /// # Errors
    /// Returns config/spawn errors; a failed start is not fatal to callers.
    pub fn ensure_watcher(&self, project: &crate::store::models::Project) -> Result<bool> {
        let slot = self.project_slot(project.id);
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        let config = self.config_in(&mut state, project)?;
        self.watchers
            .ensure_with_settings(&self.cache, project, config, &self.db_settings)
    }

    /// Message for `status` when the global config on disk differs from the
    /// snapshot the daemon started with (OQ-6); `None` when in sync.
    #[must_use]
    pub fn restart_notice(&self) -> Option<String> {
        if let Some(err) = &self.global_config_error {
            return Some(format!(
                "global config is invalid ({err}); fix it and run `docsbase daemon stop`"
            ));
        }
        if global_config_stamp() == self.global_config_stamp {
            return None;
        }
        Some("global config changed; restart the daemon: `docsbase daemon stop`".to_owned())
    }
}

/// Content hash of the global config file; `None` when it does not exist.
fn global_config_stamp() -> Option<u64> {
    let path = paths::config_dir().ok()?.join("config.toml");
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// Bound for waiting until in-flight requests settle before shutdown (T46).
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Stderr tracing for connection lifecycle (`DOCSBASE_IPC_DEBUG=1`).
fn ipc_debug(message: &str) {
    if std::env::var_os("DOCSBASE_IPC_DEBUG").is_some() {
        eprintln!("ipc-debug: {message}");
    }
}

#[derive(Default)]
struct Peer {
    session_id: Option<u64>,
    cancel_rx: Option<oneshot::Receiver<()>>,
    hello_done: bool,
}

/// Serves an explicitly unadmitted compatibility connection.
pub async fn handle_connection(
    shared: Arc<Shared>,
    stream: Stream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
) {
    handle_connection_in(shared, stream, in_flight, events, shutdown, None).await;
}

/// Real accept retains its existing connection slot through actual work.
pub(super) async fn handle_admitted_connection(
    shared: Arc<Shared>,
    stream: Stream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    handle_connection_in(
        shared,
        stream,
        in_flight,
        events,
        shutdown,
        Some(Arc::new(permit)),
    )
    .await;
}

async fn handle_connection_in(
    shared: Arc<Shared>,
    stream: Stream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
    connection: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
) {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = FrameReader::new(BufReader::new(read_half));
    let mut peer = Peer::default();

    loop {
        let next_frame = match peer.cancel_rx.as_mut() {
            Some(cancel) => tokio::select! {
                frame = reader.read_frame() => frame,
                // The janitor reaped this session: stop holding the socket.
                _ = cancel => break,
            },
            None => reader.read_frame().await,
        };
        let line = match next_frame {
            Ok(Frame::Line(line)) => {
                ipc_debug(&format!(
                    "frame {} bytes on session {:?}",
                    line.len(),
                    peer.session_id
                ));
                line
            }
            Ok(Frame::Eof) => {
                ipc_debug("connection EOF");
                break;
            }
            Err(err) => {
                ipc_debug(&format!("connection read error: {err}"));
                break;
            }
            Ok(frame @ (Frame::Incomplete | Frame::TooLarge)) => {
                let message = match frame {
                    Frame::TooLarge => {
                        format!("frame exceeds {} bytes", protocol::MAX_FRAME_BYTES)
                    }
                    _ => "incomplete frame without delimiter".to_owned(),
                };
                let err = Error::Protocol { message };
                let response = error_response(&err);
                if let Ok(bytes) = protocol::encode(&response) {
                    let _ = writer.write_all(&bytes).await;
                }
                break;
            }
        };
        let guard = Activity::new(&in_flight, &events, connection.as_ref());
        let decoded = protocol::decode_request(&line);
        let scope = match &decoded {
            Ok(request) => scoped::Scope::new(&shared, request, Arc::clone(&guard)),
            Err(_) => Ok(scoped::Scope {
                activity: Arc::clone(&guard),
                operation: None,
            }),
        };
        let immediate = scope.is_err()
            || decoded
                .as_ref()
                .is_err_and(|error| matches!(error, Error::Admission { .. }));
        let (response, close) = match (decoded, &scope) {
            (Ok(request), Ok(scope)) => {
                handle_request(
                    &shared,
                    &shutdown,
                    &in_flight,
                    &mut peer,
                    request,
                    scope,
                    &mut reader,
                )
                .await
            }
            (Err(error), _) => (error_response(&error), immediate),
            (_, Err(error)) => (error_response(error), true),
        };
        let operation = scope
            .as_ref()
            .ok()
            .and_then(|scope| scope.operation.as_ref());
        let delivered = scoped::reply(&mut writer, response, operation, immediate).await;
        drop(guard);
        if !delivered {
            break;
        }
        if close {
            break;
        }
    }

    if let Some(id) = peer.session_id {
        ipc_debug(&format!("leaving session {id}"));
        shared.sessions.leave(id);
    }
    let _ = events.try_send(());
}

async fn handle_request<R: tokio::io::AsyncBufRead + Unpin>(
    shared: &Arc<Shared>,
    shutdown: &watch::Sender<bool>,
    in_flight: &Arc<AtomicUsize>,
    peer: &mut Peer,
    request: Request,
    scope: &scoped::Scope,
    reader: &mut FrameReader<R>,
) -> (Response, bool) {
    match request {
        Request::Hello {
            protocol_version,
            build_id,
            ..
        } => {
            if protocol_version != PROTOCOL_VERSION {
                let err = Error::Protocol {
                    message: format!("protocol version {protocol_version} != {PROTOCOL_VERSION}"),
                };
                return (error_response(&err), true);
            }
            if build_id != protocol::build_id() {
                let daemon_build = protocol::build_id();
                crate::conflict::record(
                    &shared.cache,
                    &crate::conflict::Conflict {
                        kind: "hello_build_mismatch",
                        expected: &daemon_build,
                        actual: &build_id,
                        build_id: &build_id,
                        // The refused frontend's schema is unknown; record
                        // the daemon's own expectation here.
                        schema_version: migrations::SCHEMA_VERSION,
                        cache_root: &shared.cache,
                        pid: std::process::id(),
                        recorded_build_id: Some(&daemon_build),
                        recorded_schema_version: Some(migrations::SCHEMA_VERSION),
                        holder_pid: None,
                    },
                );
                let err = Error::Admission {
                    message: format!(
                        "build {build_id:?} != {daemon_build:?}; run `docsbase install` or restart the daemon and frontend"
                    ),
                };
                return (error_response(&err), true);
            }
            peer.hello_done = true;
            (
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    build_id: protocol::build_id(),
                    schema_version: migrations::SCHEMA_VERSION,
                    capabilities: vec![protocol::SEARCH_CONTEXT_CAPABILITY.to_owned()],
                },
                false,
            )
        }
        Request::RegisterSession { pid, cwd } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            register_and_respond(
                shared,
                &mut peer.session_id,
                &mut peer.cancel_rx,
                pid,
                &cwd,
                false,
            )
            .await
        }
        Request::RegisterUnbound { pid, cwd } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            register_and_respond(
                shared,
                &mut peer.session_id,
                &mut peer.cancel_rx,
                pid,
                &cwd,
                true,
            )
            .await
        }
        Request::CallTool { name, args } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            call_and_respond(shared, peer.session_id, &name, args, scope).await
        }
        Request::Search { args, .. } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            scoped::call(shared, peer, args, scope, reader).await
        }
        Request::StopDaemon => {
            // Idempotent: only the first stop drains and signals shutdown, so
            // concurrent stops (install retries) cannot wait on each other
            // (FR-5, FR-6). The drain lets every other request finish; the
            // process then lives until background jobs complete.
            if !shared.stopping.swap(true, Ordering::SeqCst) {
                wait_for_drain(in_flight, DRAIN_TIMEOUT).await;
                let _ = shutdown.send(true);
            }
            (Response::ToolResult { value: Value::Null }, false)
        }
    }
}

async fn call_and_respond(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: Value,
    scope: &scoped::Scope,
) -> (Response, bool) {
    let operation = scope.operation.clone();
    let value = call_tool_in(
        shared,
        session_id,
        name,
        args,
        operation.clone(),
        Some(Arc::clone(&scope.activity)),
    )
    .await;
    let close = operation
        .as_ref()
        .is_some_and(|operation| operation.cancellation().is_cancelled());
    match value {
        Ok(value) => (Response::ToolResult { value }, close),
        Err(err) => (error_response(&err), close),
    }
}

/// Waits until only the stopping request itself is in flight; `false` on
/// timeout so a wedged request cannot hold shutdown forever (T46). SIGTERM
/// remains the operator's escalation path.
async fn wait_for_drain(in_flight: &AtomicUsize, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while in_flight.load(Ordering::SeqCst) > 1 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

async fn register_and_respond(
    shared: &Arc<Shared>,
    session_id: &mut Option<u64>,
    cancel_rx: &mut Option<oneshot::Receiver<()>>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
) -> (Response, bool) {
    match register_session(shared, pid, cwd, allow_unbound).await {
        Ok((id, value, cancel)) => {
            if let Some(previous) = session_id.replace(id) {
                shared.sessions.leave(previous);
            }
            *cancel_rx = Some(cancel);
            (Response::ToolResult { value }, false)
        }
        Err(err) => (error_response(&err), false),
    }
}

async fn register_session(
    shared: &Arc<Shared>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    let shared_for_task = Arc::clone(shared);
    let cwd = cwd.to_path_buf();
    let cwd_for_task = cwd.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<crate::store::models::Project> {
        // Own connection: the shared read connection must stay free while a
        // full index runs (F4/NFR-1).
        let mut db = Db::open_with_settings(&shared_for_task.cache, &shared_for_task.db_settings)?;
        let project_err = match registry::resolve_by_cwd(&db, &cwd_for_task) {
            Ok(project) => {
                shared_for_task.open_project_config(&project)?;
                shared_for_task.schedule_backfill(&project)?;
                return Ok(project);
            }
            Err(err) => err,
        };
        let Ok(root) = registry::project_root_for(&cwd_for_task, &shared_for_task.cache) else {
            return Err(project_err);
        };
        let config = shared_for_task.config_for_root(&root)?;
        if !config.auto_index {
            return Err(project_err);
        }
        let project = registry::ensure_project(&mut db, &root)?;
        if let Err(err) =
            tools::run_project_index(&mut db, &shared_for_task.cache, &project, &config)
        {
            // A concurrent registration may have won the writer lease and
            // already indexed the project; bind to that result instead.
            if let Some(indexed) = registry::resolve_by_cwd(&db, &cwd_for_task)
                .ok()
                .filter(|project| project.status == crate::store::models::ProjectStatus::Indexed)
            {
                return Ok(indexed);
            }
            return Err(err);
        }
        let project = registry::project_by_id(&db, project.id)?.ok_or_else(|| {
            Error::internal(format!("project {} vanished after auto-index", project.id))
        })?;
        shared_for_task.open_project_config(&project)?;
        Ok(project)
    })
    .await
    .map_err(|err| Error::internal_with_source("session task join", err))?;

    match outcome {
        Ok(project) => {
            let (id, cancel) = shared.sessions.join(pid, cwd, Some(project.id))?;
            ipc_debug(&format!("bound session {id} (pid {pid})"));
            Ok((
                id,
                json!({
                    "session_id": id,
                    "project_id": project.id,
                    "name": project.name,
                    "status": project.status.as_str(),
                }),
                cancel,
            ))
        }
        Err(err) if allow_unbound => {
            let (id, cancel) = shared.sessions.join(pid, cwd, None)?;
            Ok((
                id,
                json!({
                    "session_id": id,
                    "project_id": null,
                    "name": null,
                    "status": "unregistered",
                    "hint": err.to_string(),
                }),
                cancel,
            ))
        }
        Err(err) => Err(err),
    }
}

fn search_operation(
    shared: &Shared,
    name: &str,
) -> Result<Option<crate::search::operation::SearchOperation>> {
    if name == "search_docs" {
        crate::search::operation::SearchOperation::new(
            &shared.global_config.search,
            Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )
        .map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
async fn call_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: Value,
) -> Result<Value> {
    call_tool_in(
        shared,
        session_id,
        name,
        args,
        search_operation(shared, name)?,
        None,
    )
    .await
}

async fn call_tool_in(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: Value,
    operation: Option<crate::search::operation::SearchOperation>,
    activity: Option<Arc<Activity>>,
) -> Result<Value> {
    let shared = Arc::clone(shared);
    let name = name.to_owned();
    let worker_operation = operation.clone();
    let queued_at = Instant::now();
    let worker = tokio::task::spawn_blocking(move || {
        if let Some(operation) = &worker_operation {
            operation.record_queue(queued_at.elapsed());
        }
        let result = route_tool(&shared, session_id, &name, &args, worker_operation);
        drop(activity);
        result
    });
    request::run(worker, operation.as_ref()).await
}

/// `search_docs` with the project's hybrid stages, dense cache and window.
fn route_search(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    args: &Value,
    operation: crate::search::operation::SearchOperation,
) -> Result<Value> {
    operation.check()?;
    let project = {
        let reader = MetadataSource::Prepared(&shared.readers).acquire(&operation)?;
        operation.check()?;
        bound_project(shared, &reader, session_id)?
    };
    let (config, stages) = shared.project_runtime(&project)?;
    operation.check()?;
    let slot = shared.project_slot(project.id);
    let window = config
        .hybrid_cfg
        .reranker
        .as_ref()
        .map_or(crate::hybrid::config::DEFAULT_RERANK_WINDOW, |reranker| {
            reranker.window
        });
    tools::search_docs_from(
        &tools::SearchContext {
            stages: &stages,
            dense: &shared.dense,
            cache: &shared.cache,
            project: &project,
            window,
            payload_budget: &shared.payload_budget,
            operation,
        },
        crate::search::metadata::MetadataSource::Prepared(&shared.readers),
        args,
        || slot.preparation_lease(&config),
    )
}

fn route_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
    operation: Option<crate::search::operation::SearchOperation>,
) -> Result<Value> {
    if name == "search_docs" {
        return route_search(
            shared,
            session_id,
            args,
            operation.ok_or_else(|| Error::internal("search request context missing"))?,
        );
    }
    route_metadata_tool(shared, session_id, name, args)
}

fn route_metadata_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let db = shared.db();
    match name {
        "status" => {
            let stats = shared.sessions.stats();
            let notice = shared.restart_notice();
            tools::status(
                &db,
                &stats,
                shared.watchers.count(),
                notice.as_deref(),
                &|id| shared.watchers.watching(id),
                &|project| {
                    let Ok(config) = shared.project_config(project) else {
                        return None;
                    };
                    hybrid_jobs::coverage(&db, &config.hybrid_cfg, project.id)
                        .ok()
                        .flatten()
                },
                Some(&shared.dense),
            )
        }
        "list_projects" => tools::list_projects(&db),
        "list_docs" => {
            let project = bound_project(shared, &db, session_id)?;
            tools::list_docs(&db, &project, args)
        }
        "get_doc" => {
            let project = bound_project(shared, &db, session_id)?;
            let config = shared.project_config(&project)?;
            tools::get_doc(&project, &config, args)
        }
        "read_neighbors" => {
            let project = bound_project(shared, &db, session_id)?;
            tools::read_neighbors(&db, &project, args)
        }
        "index_project" => {
            drop(db);
            let path = match opt_str(args, "path")? {
                Some(raw) => {
                    let raw = PathBuf::from(raw);
                    if raw.is_absolute() {
                        raw
                    } else {
                        session_cwd(shared, session_id)?.join(raw)
                    }
                }
                None => session_cwd(shared, session_id)?,
            };
            let root = registry::project_root_for(&path, &shared.cache)?;
            let mut job_db = Db::open_with_settings(&shared.cache, &shared.db_settings)?;
            let project = registry::ensure_project(&mut job_db, &root)?;
            let config = shared.project_config(&project)?;
            let (job_id, created) = tools::enqueue_sync(&job_db, &project)?;
            let mut just_indexed = false;
            if created {
                tools::run_sync_job_in(&mut job_db, &shared.cache, job_id, &project, &config)?;
                just_indexed = true;
            }
            let fresh =
                registry::project_by_id(&job_db, project.id)?.ok_or_else(|| Error::Project {
                    message: format!("project {} vanished after indexing", project.id),
                    instruction: None,
                })?;
            if let Err(err) = shared.ensure_watcher(&fresh) {
                eprintln!("warning: cannot watch project {}: {err}", fresh.id);
            }
            if just_indexed {
                let _ = shared.refresh_ann_after_sync(&fresh);
            }
            current_job(&job_db, job_id)
        }
        "sync_start" => {
            let project = match opt_i64(args, "project_id")? {
                Some(id) => registry::project_by_id(&db, id)?.ok_or_else(|| Error::Project {
                    message: format!("unknown project id {id}"),
                    instruction: Some("call list_projects".to_owned()),
                })?,
                None => bound_project(shared, &db, session_id)?,
            };
            registry::ensure_root_present(&project)?;
            // Fail fast on a broken project config instead of leaving a job
            // that would only error out in the background.
            let config = shared.project_config(&project)?;
            let (job_id, created) = tools::enqueue_sync(&db, &project)?;
            if created {
                spawn_sync_job(Arc::clone(shared), job_id, project.id, config);
            }
            current_job(&db, job_id)
        }
        "sync_status" => tools::sync_status(&db, args),
        other => Err(Error::internal(format!(
            "tool {other:?} is not implemented yet"
        ))),
    }
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(Error::Protocol {
            message: format!("{key} must be a string, got {other}"),
        }),
    }
}

fn opt_i64(args: &Value, key: &str) -> Result<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| Error::Protocol {
            message: format!("{key} must be an integer, got {value}"),
        }),
    }
}

fn current_job(db: &Db, job_id: i64) -> Result<Value> {
    let job = crate::store::repo::sync_job(db.connection(), job_id)?
        .ok_or_else(|| Error::internal(format!("sync job {job_id} vanished after enqueue")))?;
    Ok(tools::sync_job_value(&job))
}

fn spawn_sync_job(shared: Arc<Shared>, job_id: i64, project_id: i64, config: Arc<Config>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || {
                match tools::execute_sync_job_with_settings(
                    &shared.cache,
                    job_id,
                    project_id,
                    &config,
                    &shared.db_settings,
                ) {
                    Ok(project) => {
                        if let Err(err) = shared.ensure_watcher(&project) {
                            eprintln!("warning: cannot watch project {}: {err}", project.id);
                        }
                        let _ = shared.refresh_ann_after_sync(&project);
                    }
                    Err(err) => eprintln!("warning: sync job {job_id} failed: {err}"),
                }
            });
        }
        Err(err) => {
            eprintln!("warning: sync job {job_id} not started (no runtime: {err})");
        }
    }
}

fn bound_project(
    shared: &Arc<Shared>,
    db: &Db,
    session_id: Option<u64>,
) -> Result<crate::store::models::Project> {
    let project_id = session_id
        .and_then(|id| shared.sessions.get(id))
        .and_then(|session| session.project_id)
        .ok_or_else(|| Error::Project {
            message: "session is not bound to a project".to_owned(),
            instruction: Some("register a session from your project directory".to_owned()),
        })?;
    registry::project_by_id(db, project_id)?.ok_or_else(|| Error::Project {
        message: format!("project {project_id} disappeared from the registry"),
        instruction: None,
    })
}

fn session_cwd(shared: &Arc<Shared>, session_id: Option<u64>) -> Result<PathBuf> {
    session_id
        .and_then(|id| shared.sessions.get(id))
        .map(|session| session.cwd)
        .ok_or_else(|| Error::Project {
            message: "session is not bound to a project".to_owned(),
            instruction: Some("index_project needs a path or a bound session".to_owned()),
        })
}

fn hello_required() -> Response {
    error_response(&Error::Protocol {
        message: "Hello must be the first request".to_owned(),
    })
}

fn error_response(err: &Error) -> Response {
    Response::Error {
        code: err.mcp_code(),
        message: err.inner_message(),
        instruction: err.instruction().map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::config::Config;
    use crate::daemon::registry;
    use crate::daemon::tools::run_project_index;
    use crate::hybrid::dense::DenseRows;
    use crate::hybrid::jobs::embed_project_chunks_with;
    use crate::vector_cache::Embedder;

    #[tokio::test]
    async fn drain_timeout_is_bounded() {
        let stuck = AtomicUsize::new(2);
        let started = tokio::time::Instant::now();
        assert!(!wait_for_drain(&stuck, Duration::from_millis(50)).await);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "drain must stop at the timeout"
        );

        let idle = AtomicUsize::new(1);
        assert!(wait_for_drain(&idle, Duration::from_millis(50)).await);
    }

    struct FakeEmbedder;

    impl crate::vector_cache::Embedder for FakeEmbedder {
        fn model_id(&self) -> &'static str {
            "openrouter/model@d3@l2"
        }

        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    let hash = blake3::hash(text.as_bytes());
                    let mut vector: Vec<f32> = hash.as_bytes()[..3]
                        .iter()
                        .map(|byte| f32::from(*byte) / 127.5 - 1.0)
                        .collect();
                    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
                    for value in &mut vector {
                        *value /= norm;
                    }
                    vector
                })
                .collect())
        }
    }

    use crate::store::models::Project;
    use std::fmt::Write as _;
    use std::sync::mpsc::{self as std_mpsc, TryRecvError};

    const REFRESH_WATCHDOG: Duration = Duration::from_secs(300);

    // Drop shared state and DB handles before removing directories on Windows.
    struct RefreshFixture {
        shared: Arc<Shared>,
        db: Db,
        project: Project,
        root: tempfile::TempDir,
        cache: tempfile::TempDir,
    }

    impl RefreshFixture {
        fn new(enabled: bool, ranking: Option<&str>, chunks: usize) -> Self {
            Self::with_db_settings(
                enabled,
                ranking,
                chunks,
                &crate::store::settings::standalone().expect("policy"),
            )
        }

        fn with_db_settings(
            enabled: bool,
            ranking: Option<&str>,
            chunks: usize,
            policy: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
        ) -> Self {
            Self::with_policies(
                enabled,
                ranking,
                chunks,
                policy,
                &crate::store::settings::standalone_read_pool().expect("pool policy"),
            )
        }

        fn with_policies(
            enabled: bool,
            ranking: Option<&str>,
            chunks: usize,
            policy: &Resolved<DbSettings>,
            pool: &Resolved<ReadPoolSettings>,
        ) -> Self {
            let cache = tempfile::TempDir::new().expect("cache");
            let root = tempfile::TempDir::new().expect("root");
            // Open an empty registry: this fixture tests refresh, not watchers.
            let shared = Shared::open_with_policies(cache.path(), policy, pool).expect("shared");
            let ranking = ranking.map_or(String::new(), |value| format!("ranking = \"{value}\"\n"));
            fs::write(
                root.path().join(".docsbase.toml"),
                format!(
                    "[hybrid]\nenabled = {enabled}\n{ranking}[hybrid.embedding]\nprovider = \"openrouter\"\nmodel = \"model\"\ndimensions = 3\nnormalize = true\n"
                ),
            )
            .expect("write config");
            let mut db = Db::open_with_settings(cache.path(), policy).expect("db");
            let canonical = root.path().canonicalize().expect("canonical");
            let project = registry::ensure_project(&mut db, &canonical).expect("project");
            let mut fixture = Self {
                shared,
                db,
                project,
                root,
                cache,
            };
            fixture.index_chunks(chunks);
            fixture
        }

        fn index_chunks(&mut self, chunks: usize) {
            let mut body = String::new();
            for i in 0..chunks {
                writeln!(body, "# doc {i}\n\nbody {i}.\n").expect("markdown");
            }
            fs::write(self.root.path().join("docs.md"), body).expect("write corpus");
            run_project_index(
                &mut self.db,
                self.cache.path(),
                &self.project,
                &Config::default(),
            )
            .expect("index");
            embed_project_chunks_with(&self.db, &FakeEmbedder, self.project.id).expect("embed");
            self.project = registry::project_by_id(&self.db, self.project.id)
                .expect("project lookup")
                .expect("indexed project");
            let counts = crate::store::repo::project_counts(self.db.connection(), self.project.id)
                .expect("counts");
            assert_eq!(counts.docs, 1, "fixture must use one Markdown file");
            assert_eq!(usize::try_from(counts.chunks).expect("chunk count"), chunks);
        }

        fn stale_tmp(&self) -> PathBuf {
            let other_pid = std::process::id().wrapping_add(1);
            let path = self.cache.path().join(format!(
                "projects/{}/ann/tmp-{other_pid}-dead",
                self.project.id
            ));
            fs::create_dir_all(&path).expect("stale tmp");
            path
        }
    }

    fn finish_refresh(worker: Option<std::thread::JoinHandle<()>>) {
        let worker = worker.expect("embedding config must schedule a refresh worker");
        let deadline = Instant::now() + REFRESH_WATCHDOG;
        while !worker.is_finished() {
            assert!(Instant::now() < deadline, "refresh worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.join().expect("refresh worker must not panic");
    }

    fn configured_db_policy() -> crate::config::schema::Resolved<crate::store::settings::DbSettings>
    {
        crate::config::resolve::resolve_component::<crate::store::settings::DbSpec>(
            &crate::config::bootstrap::DefaultsContext::standalone(),
            [(
                crate::config::schema::SourceId::Cli,
                crate::store::settings::DbPatch {
                    write_busy_timeout: Some(Duration::from_millis(17)),
                    read_busy_timeout: Some(Duration::from_millis(29)),
                    ..crate::store::settings::DbPatch::default()
                },
            )],
        )
        .expect("policy")
    }

    #[test]
    fn configured_store_policy_reaches_initial_writer() {
        let cache = tempfile::tempdir().expect("cache");
        let policy = configured_db_policy();
        let shared = Shared::open_with_settings(cache.path(), &policy).expect("shared");
        let timeout: u32 = shared
            .db()
            .connection()
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("timeout");
        assert_eq!(timeout, 17);
    }

    #[test]
    fn configured_pool_policy_reaches_prepared_readers() {
        use crate::config::bootstrap::DefaultsContext;
        use crate::config::resolve::resolve_component;
        use crate::config::schema::SourceId;
        use crate::store::settings::{ReadPoolPatch, ReadPoolSpec};
        let cache = tempfile::tempdir().expect("cache");
        let policy = configured_db_policy();
        let pool = resolve_component::<ReadPoolSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Cli,
                ReadPoolPatch {
                    max_connections: Some(1),
                },
            )],
        )
        .expect("pool policy");
        let shared = Shared::open_with_policies(cache.path(), &policy, &pool).expect("shared");
        assert_eq!(shared.read_pool().settings(), &pool);
        let reader = shared
            .read_pool()
            .acquire(Instant::now() + Duration::from_secs(300))
            .expect("reader");
        let timeout: u32 = reader
            .connection()
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("timeout");
        assert_eq!(timeout, 29);
        assert!(
            reader
                .connection()
                .execute_batch("CREATE TABLE denied (id INTEGER)")
                .is_err()
        );
    }

    #[test]
    fn invalid_pool_policy_does_not_open_shared_writer() {
        let root = tempfile::tempdir().expect("root");
        let cache = root.path().join("cache");
        let valid = crate::store::settings::standalone_read_pool().expect("pool policy");
        let mut raw = valid.settings().clone();
        raw.max_connections = 0;
        let imported = Resolved::new(raw, valid.provenance().clone());
        assert!(Shared::open_with_policies(&cache, &configured_db_policy(), &imported).is_err());
        assert!(!cache.exists());
    }

    #[test]
    fn refresh_refuses_exhausted_prepared_pool_and_still_cleans() {
        let fixture = RefreshFixture::new(true, Some("rrf"), 1);
        let stale = fixture.stale_tmp();
        let readers: Vec<_> = (0..fixture
            .shared
            .read_pool()
            .settings()
            .settings()
            .max_connections)
            .map(|_| {
                fixture
                    .shared
                    .read_pool()
                    .try_acquire()
                    .expect("hold every reader")
            })
            .collect();
        let (called, observed) = std_mpsc::channel();
        finish_refresh(fixture.shared.refresh_ann_after_sync_with(
            &fixture.project,
            move |_, _, _| {
                called
                    .send(())
                    .map_err(|err| Error::internal(err.to_string()))
            },
        ));
        let attempted = observed.try_recv();
        drop(readers);
        assert_eq!(
            attempted,
            Err(TryRecvError::Disconnected),
            "refresh must not open a replacement outside the prepared pool"
        );
        assert!(
            !stale.exists(),
            "cleanup must run after reader admission refusal"
        );
    }

    #[test]
    fn refresh_releases_single_scalar_reader_before_lazy_warm() {
        use crate::config::bootstrap::DefaultsContext;
        use crate::config::resolve::resolve_component;
        use crate::config::schema::SourceId;
        use crate::store::settings::{ReadPoolPatch, ReadPoolSpec};
        let pool = resolve_component::<ReadPoolSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Cli,
                ReadPoolPatch {
                    max_connections: Some(1),
                },
            )],
        )
        .expect("one reader");
        let mut fixture =
            RefreshFixture::with_policies(true, Some("rrf"), 1, &configured_db_policy(), &pool);
        // Isolate metadata ownership from ANN tier/threshold selection.
        Arc::get_mut(&mut fixture.shared)
            .expect("unique fixture state")
            .dense = DenseCache::with_cap(2);
        finish_refresh(fixture.shared.refresh_ann_after_sync(&fixture.project));
        let generation =
            crate::store::repo::index_generation(fixture.db.connection(), fixture.project.id)
                .expect("generation");
        let rows = fixture
            .shared
            .dense
            .get_with_metadata(
                DenseRequest {
                    cache_root: fixture.cache.path(),
                    project_id: fixture.project.id,
                    generation,
                    model_id: FakeEmbedder.model_id(),
                },
                &crate::hybrid::work::Cancellation::new(),
                || panic!("warm must materialize and retain rows using the single reader"),
            )
            .expect("warm populated resident state");
        match rows {
            crate::hybrid::dense::DenseRows::Ready(index) => assert_eq!(index.rows().len(), 1),
            _ => panic!("explicit exact-only fixture must retain its one vector"),
        }
        assert!(fixture.shared.read_pool().try_acquire().is_ok());
    }

    #[test]
    fn ann_refresh_uses_configured_readonly_connection() {
        let policy = configured_db_policy();
        let fixture = RefreshFixture::with_db_settings(true, Some("rrf"), 1, &policy);
        let (called, observed) = std_mpsc::channel();
        finish_refresh(fixture.shared.refresh_ann_after_sync_with(&fixture.project,
            move |db, _, _| {
                let timeout: u32 = db.connection()
                    .pragma_query_value(None, "busy_timeout", |row| row.get(0)).expect("timeout");
                assert_eq!(timeout, 29, "child reader must receive the same supplied policy");
                let error = db.connection().execute("CREATE TABLE forbidden_refresh_write (id INTEGER)", [])
                    .expect_err("ANN metadata connection must be read-only");
                assert!(matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ReadOnly));
                called.send(()).map_err(|error| Error::internal(error.to_string()))
            }));
        observed
            .try_recv()
            .expect("actual refresh must observe the connection");
    }

    #[test]
    fn sync_refresh_warms_ann_for_rrf() {
        let fixture = RefreshFixture::new(true, Some("rrf"), 1);
        let stale = fixture.stale_tmp();
        let (called, observed) = std_mpsc::channel();
        let (release, proceed) = std_mpsc::channel();
        let worker = fixture
            .shared
            .refresh_ann_after_sync_with(&fixture.project, move |_, project, model| {
                called
                    .send((project, model.to_owned()))
                    .map_err(|err| Error::internal(err.to_string()))?;
                // Dropping the parent sender also releases this gate on panic.
                proceed
                    .recv_timeout(REFRESH_WATCHDOG)
                    .map_err(|err| Error::internal(err.to_string()))?;
                Ok(())
            })
            .expect("worker");
        assert_eq!(
            observed
                .recv_timeout(REFRESH_WATCHDOG)
                .expect("warm must start"),
            (fixture.project.id, FakeEmbedder.model_id().to_owned())
        );
        assert!(
            !worker.is_finished(),
            "refresh must return while warm is gated"
        );
        assert!(stale.exists(), "cleanup must follow warm, not precede it");
        release.send(()).expect("release warm");
        finish_refresh(Some(worker));
        assert!(!stale.exists(), "refresh must complete project cleanup");
    }

    fn assert_refresh_without_warm(fixture: &RefreshFixture) {
        let stale = fixture.stale_tmp();
        let (called, observed) = std_mpsc::channel();
        finish_refresh(fixture.shared.refresh_ann_after_sync_with(
            &fixture.project,
            move |_, _, _| {
                called
                    .send(())
                    .map_err(|err| Error::internal(err.to_string()))?;
                Ok(())
            },
        ));
        assert_eq!(
            observed.try_recv(),
            Err(TryRecvError::Disconnected),
            "a completed inactive-dense refresh must not call warm"
        );
        assert!(
            !stale.exists(),
            "cleanup must run even when warm is skipped"
        );
    }

    #[test]
    fn sync_refresh_skips_ann_for_lexical_ranking() {
        // Cover both parsed default lexical ranking and its explicit spelling.
        for ranking in [None, Some("lexical")] {
            assert_refresh_without_warm(&RefreshFixture::new(true, ranking, 1));
        }
    }

    #[test]
    fn sync_refresh_skips_ann_when_hybrid_is_disabled() {
        assert_refresh_without_warm(&RefreshFixture::new(false, Some("rrf"), 1));
    }

    #[test]
    fn sync_refresh_skips_worker_without_a_usable_embedding_config() {
        let fixture = RefreshFixture::new(true, Some("rrf"), 1);
        let mut config = (*fixture
            .shared
            .project_config(&fixture.project)
            .expect("valid project config"))
        .clone();
        config.hybrid_cfg.embedding = None;
        let slot = fixture.shared.project_slot(fixture.project.id);
        slot.state.lock().expect("project state").config = Some(Arc::new(config));
        assert!(
            fixture
                .shared
                .refresh_ann_after_sync(&fixture.project)
                .is_none()
        );

        slot.state.lock().expect("project state").config = None;
        fs::write(fixture.root.path().join(".docsbase.toml"), "[invalid toml")
            .expect("invalid config fixture");
        assert!(
            fixture
                .shared
                .refresh_ann_after_sync(&fixture.project)
                .is_none()
        );
    }

    #[test]
    fn sync_refresh_continues_cleanup_after_failures() {
        use crate::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};

        let mut fixture = RefreshFixture::new(true, Some("rrf"), 1);
        Arc::get_mut(&mut fixture.shared)
            .expect("fixture has unique shared state")
            .dense = DenseCache::with_policy_limits(
            AnnPolicy::BuildOrLoad,
            fixture.cache.path(),
            DenseCacheLimits {
                max_ann_disk_bytes: 0,
                ..DenseCacheLimits::default()
            },
        );
        let ann_root = fixture
            .cache
            .path()
            .join(format!("projects/{}/ann", fixture.project.id));
        fs::create_dir_all(ann_root.parent().expect("project directory")).expect("mkdir");
        // A real project-cleanup error must not prevent global disk cleanup.
        fs::write(&ann_root, b"not a directory").expect("broken ann directory");
        let store = AnnStore::new(fixture.cache.path().to_path_buf());
        let slots: [PathBuf; 2] = std::array::from_fn(|i| {
            let key = AnnKey {
                project_id: fixture.project.id + 1 + i64::try_from(i).expect("slot index"),
                generation: 1,
                model_id: FakeEmbedder.model_id().to_owned(),
                dim: 3,
                chunk_count: 1,
                chunk_set_digest: "abcd1234".to_owned(),
                format_version: ANN_FORMAT_VERSION,
            };
            store
                .persist_dump(&key, |dir| {
                    // Metadata-only payload: disk cleanup does not load the graph.
                    fs::write(dir.join("index.hnsw.graph"), b"graph bytes").expect("graph");
                    fs::write(dir.join("index.hnsw.data"), b"data bytes").expect("data");
                    Ok("index".to_owned())
                })
                .expect("disk-budget fixture")
                .dir
        });
        assert!(slots.iter().all(|path| path.is_dir()));
        let (called, observed) = std_mpsc::channel();
        finish_refresh(fixture.shared.refresh_ann_after_sync_with(
            &fixture.project,
            move |_, project, model| {
                called
                    .send((project, model.to_owned()))
                    .map_err(|err| Error::internal(err.to_string()))?;
                Err(Error::internal("injected warm failure"))
            },
        ));
        assert_eq!(
            observed.try_recv().expect("warm was attempted"),
            (fixture.project.id, FakeEmbedder.model_id().to_owned())
        );
        assert!(ann_root.is_file());
        assert_eq!(
            slots.iter().filter(|path| path.exists()).count(),
            1,
            "disk cleanup must continue after both errors and keep its last directory"
        );
    }

    /// Passive observations never call `DenseCache::get` (which can start builds).
    fn ann_ready(cache: &std::path::Path, project_id: i64) -> bool {
        let Ok(keys) = fs::read_dir(cache.join(format!("projects/{project_id}/ann"))) else {
            return false;
        };
        keys.filter_map(std::result::Result::ok)
            .any(|key| key.path().join("READY").is_file())
    }

    #[test]
    fn ann_refresh_builds_at_the_default_threshold() {
        use crate::hybrid::ann::ANN_MIN_VECTORS;

        let mut fixture = RefreshFixture::new(true, Some("rrf"), ANN_MIN_VECTORS - 1);
        finish_refresh(fixture.shared.refresh_ann_after_sync(&fixture.project));
        let below = fixture
            .shared
            .dense
            .get(&fixture.db, fixture.project.id, FakeEmbedder.model_id())
            .expect("below-threshold state");
        assert!(
            matches!(below, DenseRows::Ready(_)),
            "below threshold must stay exact: {below:?}"
        );
        assert!(!ann_ready(fixture.cache.path(), fixture.project.id));

        fixture.index_chunks(ANN_MIN_VECTORS);
        finish_refresh(fixture.shared.refresh_ann_after_sync(&fixture.project));
        let deadline = Instant::now() + REFRESH_WATCHDOG;
        loop {
            let snapshot = fixture.shared.dense.ann_snapshot(fixture.project.id);
            if let Some((count, bytes)) = snapshot {
                assert_eq!(count, ANN_MIN_VECTORS);
                assert!(bytes > 0);
                assert!(ann_ready(fixture.cache.path(), fixture.project.id));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "refresh did not publish the default-threshold ANN"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn delayed_old_ann_refresh_is_rejected_after_reload_and_still_cleans_up() {
        let fixture = RefreshFixture::new(true, Some("rrf"), 1);
        let stale = fixture.stale_tmp();
        let (entered_tx, entered_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let (called_tx, called_rx) = std_mpsc::channel();
        let worker = fixture.shared.refresh_ann_after_sync_before_with(
            &fixture.project,
            move || {
                entered_tx.send(()).expect("revision captured");
                release_rx
                    .recv_timeout(REFRESH_WATCHDOG)
                    .expect("release delayed scheduling");
            },
            move |_, _, _| {
                called_tx.send(()).expect("warm called");
                Ok(())
            },
        );
        entered_rx
            .recv_timeout(REFRESH_WATCHDOG)
            .expect("old refresh scheduled");
        fs::write(
            fixture.root.path().join(".docsbase.toml"),
            "hybrid = false\n",
        )
        .expect("new config");
        let reloaded = fixture.shared.open_project_config(&fixture.project);
        release_tx.send(()).expect("release old refresh");
        finish_refresh(worker);
        assert!(!reloaded.expect("reload before old warm").hybrid_cfg.enabled);
        assert_eq!(
            called_rx.try_recv(),
            Err(TryRecvError::Disconnected),
            "old revision cannot warm"
        );
        assert!(!stale.exists(), "cleanup runs when stale warm is refused");
    }
}

#[cfg(test)]
#[path = "project_state_tests.rs"]
mod project_state_tests;
