//! IPC server: handshake, sessions and tool routing (FR-3, FR-7, FR-8, FR-12,
//! FR-33; I2, I6, I8).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::config::schema::Resolved;
use crate::daemon::launch::{
    AdmittedLaunch, ConfigStamp, FixedProject, LaunchConfig, LaunchSnapshot,
};
use crate::daemon::request::{self, Activity};
use crate::daemon::session::SessionRegistry;
use crate::daemon::settings::{DaemonPatch, DaemonSettings};
use crate::daemon::tools;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::hybrid::config::Ranking;
#[cfg(test)]
use crate::hybrid::dense::DenseCacheLimits;
#[cfg(test)]
use crate::hybrid::dense::DenseRequest;
use crate::hybrid::dense::{AnnPolicy, BoundDenseRequest, DenseCache};
use crate::hybrid::jobs as hybrid_jobs;
use crate::hybrid::meta::AnnState;
use crate::index::async_job::catalog_sync;
use crate::ipc::framing::{Frame, FrameReader};
use crate::ipc::protocol::{self, PROTOCOL_VERSION, Request, Response};
use crate::platform::transport::Stream;
use crate::search::metadata::MetadataSource;
use crate::store::Db;
use crate::store::migrations;
use crate::store::read_pool::ReadPool;
use crate::store::registry_inspect::ObservedProjectIndex;
use crate::store::settings::{DbSettings, ReadPoolSettings};
use crate::store::{
    generation_build::CatalogHead,
    models::Project,
    project_index::{
        CatalogProjectInfo, ProjectIndex, catalog_project_info, require_catalog_embedding,
    },
};
use crate::watch::Watchers;

#[cfg(test)]
#[path = "search_phase_tests.rs"]
mod search_phase_tests;

#[cfg(all(test, feature = "hybrid-remote"))]
#[path = "async_index_tests.rs"]
mod async_index_tests;
#[cfg(all(test, feature = "hybrid-remote"))]
#[path = "async_search_tests.rs"]
mod async_search_tests;

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;

#[cfg(test)]
#[path = "server_worker_tests.rs"]
mod server_worker_tests;

#[path = "scoped.rs"]
mod scoped;

#[cfg(feature = "hybrid-remote")]
#[path = "ask.rs"]
mod ask;

#[path = "search_async.rs"]
pub(crate) mod search_async;

#[path = "live_config.rs"]
mod live_config;

/// Daemon state shared by all connections.
pub struct Shared {
    catalog_epoch: Option<[u8; 16]>,
    /// Cache root being served.
    pub cache: PathBuf,
    // Independent of the DB mutex: child construction never waits for a query
    // just to obtain its startup policy.
    db_settings: Resolved<DbSettings>,
    readers: Arc<ReadPool>,
    payload_budget: crate::search::budget::PayloadBudget,
    pub(super) workers: Arc<crate::daemon::worker_admission::WorkerAdmission>,
    pub(super) activity: request::ActivitySource,
    activity_events: Mutex<Option<mpsc::Receiver<()>>>,
    jobs: std::sync::OnceLock<super::job_service::JobService>,
    #[cfg(test)]
    pub(super) registration_work_gate: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    // Website has exactly one fixed-project background sync closure.
    sync_slot: Arc<tokio::sync::Semaphore>,
    #[cfg(feature = "hybrid-remote")]
    remote: Arc<crate::hybrid::remote::RemoteExecution>,
    /// Live sessions.
    pub sessions: SessionRegistry,
    /// Per-project filesystem watchers (FR-15).
    pub watchers: Arc<Watchers>,
    /// Set by the first `StopDaemon`; later stops return immediately (FR-6).
    stopping: AtomicBool,
    /// Global config snapshot taken at daemon start (OQ-6).
    global_config: Config,
    ipc_debug: bool,
    website: Option<FixedProject>,
    /// Why the global config could not be loaded; surfaced by `status`.
    global_config_error: Option<String>,
    /// Fingerprint of the global config file at daemon start (OQ-6).
    global_config_stamp: ConfigStamp,
    global_config_overrides: crate::daemon::settings::DaemonPatch,
    global_config_defaults: crate::config::bootstrap::DefaultsContext,
    global_config_path: Option<PathBuf>,
    config_lock_dir: Option<PathBuf>,
    invalidation: crate::config::policy::PolicyContext,
    /// Per-project config/stage slots; the map lock only protects lookup.
    project_states: Mutex<HashMap<i64, ProjectSlot>>,
    /// Dense candidate cache keyed by project and index generation (FR-16).
    pub dense: DenseCache,
    // Close after the prepared readers so SQLite's last handle can clean up
    // WAL sidecars. A final read-only connection cannot perform that cleanup.
    db: Mutex<Db>,
}

type ProjectSlot = Arc<ProjectCell>;

impl Drop for Shared {
    fn drop(&mut self) {
        #[cfg(feature = "hybrid-remote")]
        self.shutdown_remote();
        self.activity.retire_bridge();
    }
}

/// Stable per-project authority; model construction uses only the reload lock.
#[derive(Default)]
pub(crate) struct ProjectCell {
    state: Mutex<ProjectState>,
    reload: Mutex<()>,
    preparation: RwLock<()>,
    catalog_sync: crate::index::async_job::catalog_sync::Slot,
    catalog_backfill: std::sync::OnceLock<Arc<tokio::sync::Semaphore>>,
    watch_candidate: crate::watch::WatchConfigCandidate,
    #[cfg(test)]
    preparation_observer: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    #[cfg(all(test, unix, feature = "hybrid-remote"))]
    pub(crate) reused_commit_fault: Mutex<Option<catalog_sync::ReusedCommitFault>>,
}

impl ProjectCell {
    pub(crate) fn search_preparation_lease<'a>(
        &'a self,
        expected: &Arc<Config>,
        stages: &Arc<HybridStages>,
    ) -> Result<RwLockReadGuard<'a, ()>> {
        let lease = self.preparation_lease(expected)?;
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state
            .search_stages
            .as_ref()
            .or(state.stages.as_ref())
            .is_some_and(|current| Arc::ptr_eq(current, stages))
        {
            return Err(Error::Project {
                message: "project runtime changed before search preparation".to_owned(),
                instruction: Some("retry search".to_owned()),
            });
        }
        drop(state);
        Ok(lease)
    }

    fn with_preparation<T>(
        &self,
        expected: &Arc<Config>,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let _preparation = self.preparation_lease(expected)?;
        work()
    }

    pub(crate) fn preparation_lease(
        &self,
        expected: &Arc<Config>,
    ) -> Result<RwLockReadGuard<'_, ()>> {
        #[cfg(test)]
        if let Some(observer) = self.preparation_observer.lock().unwrap().as_ref() {
            let _ = observer.send(());
        }
        let preparation = self
            .preparation
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        self.check_config_revision(expected)?;
        Ok(preparation)
    }

    fn check_config_revision(&self, expected: &Arc<Config>) -> Result<()> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.unverified.is_some() {
            return Err(Shared::catalog_config_busy());
        }
        if !state
            .config
            .as_ref()
            .is_some_and(|config| Arc::ptr_eq(config, expected))
        {
            return Err(Error::Project {
                message: "project configuration changed before search preparation".to_owned(),
                instruction: Some("retry search".to_owned()),
            });
        }
        Ok(())
    }

    pub(crate) fn begin_watch_config_transition(
        &self,
        config: Arc<Config>,
    ) -> crate::watch::WatchConfigTransition {
        crate::watch::WatchConfigTransition::begin(&self.watch_candidate, config)
    }

    pub(crate) fn source_watermark(&self) -> u64 {
        self.watch_candidate.source_watermark()
    }

    pub(crate) fn check_source_watermark(&self, watermark: u64) -> Result<()> {
        if self.source_watermark() != watermark {
            return Err(Error::Project {
                message: "project files changed during index verification".into(),
                instruction: Some("retry index or sync".into()),
            });
        }
        Ok(())
    }

    pub(crate) fn try_catalog_reload(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        match self.reload.try_lock() {
            Ok(guard) => Ok(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => Err(Shared::catalog_config_busy()),
        }
    }

    pub(crate) fn catalog_publication(
        &self,
        expected: &Arc<Config>,
    ) -> Result<CatalogConfigPublication<'_>> {
        let publication = self.try_catalog_publication()?;
        self.check_config_revision(expected)?;
        Ok(publication)
    }

    fn try_catalog_publication(&self) -> Result<CatalogConfigPublication<'_>> {
        // Match the lock order of ordinary reload and watcher installation.
        let reload = self.try_catalog_reload()?;
        let preparation = match self.preparation.try_write() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(Shared::catalog_config_busy()),
        };
        Ok(CatalogConfigPublication {
            slot: self,
            preparation,
            reload,
        })
    }
}

pub(crate) struct CatalogConfigPublication<'a> {
    slot: &'a ProjectCell,
    preparation: std::sync::RwLockWriteGuard<'a, ()>,
    reload: std::sync::MutexGuard<'a, ()>,
}

impl<'a> CatalogConfigPublication<'a> {
    /// Readers can use the new runtime while watcher installation remains
    /// serialized with any subsequent config publication.
    pub(crate) fn release_preparation(self) -> std::sync::MutexGuard<'a, ()> {
        drop(self.preparation);
        self.reload
    }

    pub(crate) fn retain_unverified(
        &self,
        proof: CatalogRuntimeProof,
        runtime: Arc<crate::watch::WatchRuntime>,
        watch_transition: Option<crate::watch::WatchConfigTransition>,
    ) {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.unverified = Some(UnverifiedRuntime {
            proof,
            runtime,
            _watch_transition: watch_transition,
        });
    }

    pub(crate) fn install(&self, config: Arc<Config>, stages: Arc<HybridStages>) -> ProjectState {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(
            &mut *state,
            ProjectState {
                config: Some(config),
                stages: None,
                search_stages: Some(stages),
                ..ProjectState::default()
            },
        )
    }
}

/// A config Arc is its revision identity. Stages always belong to that Arc.
#[derive(Default)]
pub(crate) struct ProjectState {
    config: Option<Arc<Config>>,
    stages: Option<Arc<HybridStages>>,
    search_stages: Option<Arc<HybridStages>>,
    unverified: Option<UnverifiedRuntime>,
}

struct UnverifiedRuntime {
    proof: CatalogRuntimeProof,
    runtime: Arc<crate::watch::WatchRuntime>,
    _watch_transition: Option<crate::watch::WatchConfigTransition>,
}

#[derive(Clone, Copy)]
pub(crate) enum CatalogRuntimeProof {
    Generation(CatalogHead),
    Reused(catalog_sync::ReusedGenerationReceipt),
}

impl CatalogRuntimeProof {
    fn is_committed(self, connection: &rusqlite::Connection) -> Result<bool> {
        match self {
            Self::Generation(head) => head.is_selected(connection),
            Self::Reused(receipt) => receipt.is_committed(connection),
        }
    }
}

/// Explicit bootstrap ownership supplied before the daemon's first CPU/native work.
pub(crate) struct AsyncDaemonOwners {
    pub(crate) jobs: super::job_service::JobService,
    pub(crate) workers: Arc<crate::daemon::worker_admission::WorkerAdmission>,
    pub(crate) watchers: Arc<Watchers>,
    pub(crate) activity: request::ActivitySource,
    pub(crate) events: mpsc::Receiver<()>,
}

type RuntimeOwners = (
    request::ActivitySource,
    mpsc::Receiver<()>,
    Arc<crate::daemon::worker_admission::WorkerAdmission>,
    Arc<Watchers>,
    Option<super::job_service::JobService>,
);

fn prepare_runtime_owners(
    owners: Option<AsyncDaemonOwners>,
    global_config: &Config,
    watch_debug: bool,
) -> Result<RuntimeOwners> {
    Ok(if let Some(owners) = owners {
        (
            owners.activity,
            owners.events,
            owners.workers,
            owners.watchers,
            Some(owners.jobs),
        )
    } else {
        let (activity, events) = request::ActivitySource::channel(global_config.daemon.max_events);
        let workers = Arc::new(
            crate::daemon::worker_admission::WorkerAdmission::with_policies(
                &global_config.daemon,
                &global_config.remote,
            )?,
        );
        (
            activity,
            events,
            workers,
            Arc::new(Watchers::with_diagnostics(
                global_config.watchers_enabled(),
                watch_debug,
            )),
            None,
        )
    })
}

fn validate_runtime_policies(
    config: &Config,
    settings: &Resolved<DbSettings>,
    pool_settings: &Resolved<ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
) -> Result<()> {
    use crate::config::schema::{Capabilities, SettingsSpec};
    let capabilities = Capabilities::current();
    config.validate()?;
    crate::hybrid::dense_settings::DenseSpec::validate(dense_settings, &capabilities)?;
    crate::store::settings::DbSpec::validate(settings.settings(), &capabilities)?;
    crate::store::settings::ReadPoolSpec::validate(pool_settings.settings(), &capabilities)
}

fn prepare_metadata_store(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    pool_settings: &Resolved<ReadPoolSettings>,
    observed_epoch: Option<[u8; 16]>,
    initialize_catalog: bool,
    launch: &LaunchConfig,
) -> Result<(Db, ReadPool, Option<[u8; 16]>)> {
    let db = if let Some(epoch) = observed_epoch {
        crate::store::generation_build::open_index_registry(cache, settings, epoch)?
    } else if initialize_catalog {
        Db::open_for_index(cache, settings, true)?
    } else {
        Db::open_with_settings(cache, settings)?
    };
    launch.recheck_startup(Some(&db))?;
    // The writer chooses the schema under SQLite admission. A direct Index
    // may have initialized the cache after the launch observation.
    let catalog_epoch = db
        .has_generation_catalog()
        .then(|| db.generation_epoch())
        .transpose()?;
    let readers = if let Some(epoch) = catalog_epoch {
        ReadPool::prepare_for_catalog(cache, settings, pool_settings, epoch)?
    } else {
        ReadPool::prepare(cache, settings, pool_settings)?
    };
    Ok((db, readers, catalog_epoch))
}

impl Shared {
    fn config_lock_dir(&self) -> Result<&Path> {
        self.config_lock_dir
            .as_deref()
            .ok_or_else(|| Error::internal("configuration lock directory is unavailable"))
    }

    pub(super) const fn registry_schema(&self) -> u32 {
        if self.catalog_epoch.is_some() {
            crate::store::GENERATION_SCHEMA_VERSION
        } else {
            migrations::SCHEMA_VERSION
        }
    }

    /// Opens the registry database and prepares shared state.
    /// Compatibility adapter: one captured global source, warned defaults on a
    /// malformed source, and owner-resolved daemon resources. Strict boundaries
    /// use [`Self::open_with_launch`].
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
    /// Compatibility adapter; dense/query policy is resolved at this boundary.
    ///
    /// # Errors
    /// Invalid policies are rejected before filesystem access. Other failures
    /// match [`Self::open_with_settings`], including reader preparation errors.
    pub fn open_with_policies(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
    ) -> Result<Arc<Self>> {
        Self::open_with_dense_policy(
            cache,
            settings,
            pool_settings,
            crate::hybrid::dense_settings::standalone()?.settings(),
        )
    }

    /// Compatibility adapter with explicit storage/readers/query policy.
    ///
    /// # Errors
    /// Invalid policies fail before filesystem access; other startup errors match
    /// [`Self::open_with_policies`].
    pub fn open_with_resource_policies(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        query_settings: &crate::hybrid::dense_settings::QueryCacheSettings,
    ) -> Result<Arc<Self>> {
        let mut dense = crate::hybrid::dense_settings::standalone()?.into_settings();
        dense.query_cache_max_entries = query_settings.max_entries;
        dense.query_cache_max_bytes = query_settings.max_bytes;
        Self::open_with_dense_policy(cache, settings, pool_settings, &dense)
    }

    /// Compatibility adapter with component-owned dense/query policy and one
    /// captured global source. Malformed global input retains warned defaults
    /// and blocks project opens; the legacy vector-store budget keeps precedence.
    ///
    /// # Errors
    /// Invalid component policies fail before filesystem access; startup failures
    /// otherwise match [`Self::open_with_policies`].
    pub fn open_with_dense_policy(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    ) -> Result<Arc<Self>> {
        let launch = LaunchSnapshot::capture().library_compatibility(DaemonPatch::default())?;
        Self::open_with_launch_config(cache, settings, pool_settings, dense_settings, launch)
    }

    /// Strict launch from one bounded source capture. Reject invalid configuration
    /// before creating cache state, readers, watchers or a listener.
    ///
    /// # Errors
    /// Invalid global/daemon/component policies or resource preparation failures.
    pub fn open_with_launch(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        dense_settings: &crate::hybrid::dense_settings::DenseSettings,
        snapshot: &LaunchSnapshot,
        patch: DaemonPatch,
    ) -> Result<Arc<Self>> {
        Self::open_with_launch_config(
            cache,
            settings,
            pool_settings,
            dense_settings,
            snapshot.strict(patch)?,
        )
    }

    pub(crate) fn open_with_launch_config(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        dense_settings: &crate::hybrid::dense_settings::DenseSettings,
        launch: LaunchConfig,
    ) -> Result<Arc<Self>> {
        Self::open_with_admitted_launch(
            cache,
            settings,
            pool_settings,
            dense_settings,
            launch.admit(cache, settings)?,
        )
    }

    pub(crate) fn open_with_admitted_launch(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        dense_settings: &crate::hybrid::dense_settings::DenseSettings,
        admitted: AdmittedLaunch,
    ) -> Result<Arc<Self>> {
        Self::open_with_runtime_owners(
            cache,
            settings,
            pool_settings,
            dense_settings,
            admitted,
            None,
        )
    }

    #[allow(clippy::too_many_lines)] // Ordered startup of native owners and their shared state.
    pub(crate) fn open_with_runtime_owners(
        cache: &Path,
        settings: &Resolved<DbSettings>,
        pool_settings: &Resolved<ReadPoolSettings>,
        dense_settings: &crate::hybrid::dense_settings::DenseSettings,
        admitted: AdmittedLaunch,
        owners: Option<AsyncDaemonOwners>,
    ) -> Result<Arc<Self>> {
        let AdmittedLaunch {
            launch,
            website,
            catalog_epoch,
            config_lock_dir,
        } = admitted;
        validate_runtime_policies(
            launch.config.settings(),
            settings,
            pool_settings,
            dense_settings,
        )?;
        // Admission has already observed the stopped registry. Its native
        // SQLite reader can leave WAL sidecars, so compare logical registry
        // identity on the startup writer after opening it.
        launch.verify_startup_sources()?;
        let ann_policy = if website.is_some() {
            AnnPolicy::Disabled
        } else {
            AnnPolicy::BuildOrLoad
        };
        let mut dense =
            DenseCache::for_config(ann_policy, cache, dense_settings, launch.config.settings())?;
        dense.set_cpu_pool(launch.cpu_pool.clone());
        let (db, readers, catalog_epoch) = prepare_metadata_store(
            cache,
            settings,
            pool_settings,
            catalog_epoch,
            // Library compatibility constructors can serve without a JobRoot.
            // Their in-place Index writer still requires a legacy registry.
            owners.is_some()
                && website.is_none()
                && launch.config.settings().vector_store.is_some(),
            &launch,
        )?;
        let global_config = launch.config.into_settings();
        let orphans = crate::store::repo::fail_orphan_sync_jobs(db.connection())?;
        if orphans > 0 {
            eprintln!("warning: {orphans} interrupted sync job(s) marked as error");
        }
        let (activity, activity_events, workers, watchers, jobs) =
            prepare_runtime_owners(owners, &global_config, launch.watch_debug)?;
        #[cfg(feature = "hybrid-remote")]
        let remote = Arc::new(crate::hybrid::remote::RemoteExecution::real_with_owners(
            &global_config.remote,
            workers.clone(),
            activity.clone(),
        )?);
        let payload_budget =
            crate::search::budget::PayloadBudget::new(global_config.search.clone())?;
        let project_states = website.as_ref().map_or_else(HashMap::new, |fixed| {
            HashMap::from([(
                fixed.project.id,
                Arc::new(ProjectCell {
                    state: Mutex::new(ProjectState {
                        config: Some(Arc::clone(&fixed.config)),
                        stages: Some(Arc::new(HybridStages::default())),
                        search_stages: None,
                        ..ProjectState::default()
                    }),
                    ..ProjectCell::default()
                }),
            )])
        });
        let shared = Arc::new(Self {
            catalog_epoch,
            cache: cache.to_path_buf(),
            db: Mutex::new(db),
            db_settings: settings.clone(),
            readers: Arc::new(readers),
            payload_budget,
            workers,
            #[cfg(feature = "hybrid-remote")]
            remote,
            activity,
            activity_events: Mutex::new(Some(activity_events)),
            jobs: std::sync::OnceLock::new(),
            #[cfg(test)]
            registration_work_gate: Mutex::new(None),
            sync_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            sessions: SessionRegistry::with_settings(&global_config.daemon)?,
            watchers,
            stopping: AtomicBool::new(false),
            global_config,
            ipc_debug: launch.ipc_debug,
            website,
            global_config_error: launch.error,
            global_config_stamp: launch.stamp,
            global_config_overrides: launch.overrides,
            global_config_defaults: launch.defaults,
            global_config_path: launch.path,
            config_lock_dir,
            invalidation: launch.invalidation,
            project_states: Mutex::new(project_states),
            dense,
        });
        if let Some(jobs) = jobs {
            shared.activate_jobs(jobs)?;
        }
        if shared.catalog_epoch.is_none() {
            shared.restore_watchers();
        }
        Ok(shared)
    }

    pub(crate) fn activate_jobs(
        self: &Arc<Self>,
        service: super::job_service::JobService,
    ) -> Result<()> {
        service.retain_watchers(self.watchers.clone(), self.activity.clone())?;
        self.jobs
            .set(service)
            .map_err(|_| Error::internal("job root already activated"))?;
        self.watchers.activate_async(Arc::downgrade(self))?;
        if self.catalog_epoch.is_some() {
            self.restore_watchers();
        }
        Ok(())
    }

    #[cfg(test)]
    fn observe_registration_work(&self) {
        let gate = self.registration_work_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate();
        }
    }

    pub(crate) async fn run_watch_batch(
        self: &Arc<Self>,
        project: crate::store::models::Project,
        runtime: Arc<crate::watch::WatchRuntime>,
        paths: Arc<crate::watch::WatchPayload>,
        operation: crate::search::operation::SearchOperation,
        activity: Arc<Activity>,
    ) -> Result<crate::index::job::JobStats> {
        if self.catalog_epoch.is_some() {
            let result = self
                .run_catalog_watch(
                    project.clone(),
                    runtime,
                    paths,
                    &operation,
                    activity.clone(),
                )
                .await;
            return match result {
                Err(error) if crate::store::project_index::catalog_recovery_required(&error) => {
                    self.recover_catalog_watch(project, &operation, activity)
                        .await
                }
                result => result,
            };
        }
        let service = self
            .jobs
            .get()
            .ok_or_else(|| Error::internal("watcher job owner absent"))?;
        let permit = self.workers.background(&operation).await?;
        let cache = self.cache.clone();
        let root = project.canonical_root.clone();
        let call = operation.clone();
        let injected = request::spawn(Some(permit), Some(activity.clone()), move || {
            call.check()?;
            if let Some(error) = crate::watch::injected_watch_fault(&root, &cache) {
                return Err(error);
            }
            Ok(())
        });
        request::run(injected, Some(&operation)).await?;
        let context = crate::index::async_job::JobContext {
            input: crate::index::async_job::JobInput::Watch(paths),
            singleton: None,
            cache: self.cache.clone(),
            db_settings: self.db_settings.clone(),
            revision: Some(self.project_slot(project.id)),
            dense: Some(Arc::new(
                self.dense
                    .snapshot_with_build_settings(runtime.config.ann_build),
            )),
            project,
            config: runtime.config.clone(),
            stages: runtime.stages.clone(),
            source: runtime.source.clone(),
        };
        let account = match &context.input {
            crate::index::async_job::JobInput::Watch(input) => input.clone(),
            _ => return Err(Error::internal("watcher input absent")),
        };
        let result = crate::index::async_job::run(
            service,
            &self.workers,
            activity,
            operation.clone(),
            &self.payload_budget,
            context,
        )
        .await;
        if result.is_err() {
            self.settle_watch_account(&account, &operation).await?;
        }
        serde_json::from_value(result?)
            .map_err(|error| Error::internal_with_source("watcher job counters", error))
    }

    async fn run_catalog_watch(
        self: &Arc<Self>,
        project: crate::store::models::Project,
        runtime: Arc<crate::watch::WatchRuntime>,
        paths: Arc<crate::watch::WatchPayload>,
        operation: &crate::search::operation::SearchOperation,
        activity: Arc<Activity>,
    ) -> Result<crate::index::job::JobStats> {
        let service = self
            .jobs
            .get()
            .ok_or_else(|| Error::internal("watcher job owner absent"))?;
        if runtime.stages.embedder.is_some() || runtime.config.hybrid_cfg.enabled {
            let result = self
                .run_catalog_hybrid_watch(
                    project,
                    runtime,
                    paths.clone(),
                    operation,
                    activity,
                    service,
                )
                .await;
            if result.is_err() {
                self.settle_watch_account(&paths, operation).await?;
            }
            return result;
        }
        let owner = self.clone();
        let input = crate::index::job::JobInput::Watch(paths.clone());
        let account = paths.account.clone();
        let call = operation.clone();
        let result = service
            .run_native(&self.workers, activity, operation, move || {
                let slot = owner.check_catalog_watch(&project, &call)?;
                let db = owner.open_work_registry()?;
                let mut capture = crate::index::job::CaptureBudget {
                    account: &account,
                    operation: &call,
                    #[cfg(test)]
                    observed: None,
                };
                crate::store::project_index::apply_lexical_batch(
                    &db,
                    &project,
                    &runtime.config,
                    &input,
                    Some(&mut capture),
                    || {
                        let accepted =
                            slot.preparation_lease(&runtime.config).map_err(|error| {
                                Error::internal_with_source(
                                    "retry watcher with the current catalog runtime",
                                    error,
                                )
                            })?;
                        call.check()?;
                        Ok(accepted)
                    },
                )
            })
            .await;
        if result.is_err() {
            self.settle_watch_account(&paths, operation).await?;
        }
        result
    }

    async fn recover_catalog_watch(
        self: &Arc<Self>,
        project: Project,
        operation: &crate::search::operation::SearchOperation,
        activity: Arc<Activity>,
    ) -> Result<crate::index::job::JobStats> {
        let service = self
            .jobs
            .get()
            .ok_or_else(|| Error::internal("watcher job owner absent"))?;
        let epoch = self
            .catalog_epoch
            .ok_or_else(|| Error::internal("catalog watcher has no registry epoch"))?;
        let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
            .wait(
                &crate::hybrid::work::Cancellation::new(),
                self.workers.control(),
            )
            .await?;
        let shared = self.clone();
        let call = operation.clone();
        let capture = request::spawn(Some(permit), Some(activity.clone()), move || {
            call.check()?;
            registry::ensure_root_present(&project)?;
            let revision = shared.project_slot(project.id);
            let owner = revision
                .catalog_sync
                .try_own()
                .ok_or_else(|| Error::Project {
                    message: "another writer holds the project sync slot".into(),
                    instruction: Some("wait for the running index to finish".into()),
                })?;
            prepare_catalog_project_sync(
                &shared,
                project,
                epoch,
                CatalogSyncConfig::Current,
                revision,
                owner,
            )
        });
        let context = request::run(capture, Some(operation)).await?;
        let revision = context.revision.clone();
        let watermark = revision.source_watermark();
        let (owner, _) = admit_catalog_sync(self, service, &activity, operation, context).await?;
        match catalog_sync::execute(service, &self.workers, &activity, operation, owner).await {
            Err(error)
                if !lifecycle::is_lease_conflict(&error)
                    && revision.source_watermark() != watermark =>
            {
                operation.check()?;
                Err(Error::internal_with_source(
                    "retry catalog recovery after a source change",
                    error,
                ))
            }
            result => result.map(|(stats, _)| stats),
        }
    }

    async fn run_catalog_hybrid_watch(
        self: &Arc<Self>,
        project: Project,
        runtime: Arc<crate::watch::WatchRuntime>,
        paths: Arc<crate::watch::WatchPayload>,
        operation: &crate::search::operation::SearchOperation,
        activity: Arc<Activity>,
        service: &super::job_service::JobService,
    ) -> Result<crate::index::job::JobStats> {
        let shared = self.clone();
        let captured_project = project.clone();
        let call = operation.clone();
        let revision = service
            .run_native(&self.workers, activity.clone(), operation, move || {
                shared.check_catalog_watch(&captured_project, &call)
            })
            .await?;
        let context = crate::index::async_job::catalog_watch::Context {
            cache: self.cache.clone(),
            epoch: self
                .catalog_epoch
                .ok_or_else(|| Error::internal("catalog watcher has no registry epoch"))?,
            settings: self.db_settings.clone(),
            project,
            revision,
            runtime,
            paths,
            pin_ttl: self.dense.limits().ann_lifetimes.backfill_pin_ttl(),
        };
        crate::index::async_job::catalog_watch::run(
            service,
            &self.workers,
            activity,
            operation,
            &self.payload_budget,
            context,
        )
        .await
    }

    fn check_catalog_watch(
        &self,
        project: &Project,
        operation: &crate::search::operation::SearchOperation,
    ) -> Result<ProjectSlot> {
        if let Some(error) =
            crate::watch::injected_watch_fault(&project.canonical_root, &self.cache)
        {
            return Err(error);
        }
        operation.check()?;
        let slot = self.project_slot(project.id);
        self.reconcile_catalog_runtime(project.id, &slot)
            .map_err(|error| {
                Error::internal_with_source("retry catalog runtime verification for watcher", error)
            })?;
        Ok(slot)
    }

    async fn settle_watch_account(
        &self,
        paths: &crate::watch::WatchPayload,
        operation: &crate::search::operation::SearchOperation,
    ) -> Result<()> {
        // Native and codec workers retain the same guard until they settle.
        while Arc::strong_count(&paths.account) > 1 {
            crate::hybrid::work::RemoteCall::new(operation.clone())
                .wait(&crate::hybrid::work::Cancellation::new(), async {
                    tokio::time::sleep(Duration::from_millis(
                        self.global_config.daemon.drain_poll_ms,
                    ))
                    .await;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn report_watch_event(
        self: &Arc<Self>,
        project: &crate::store::models::Project,
        event: crate::watch::BatchEvent,
        policy: crate::watch::BatchRetryPolicy,
        pending: Option<Arc<crate::watch::WatchPayload>>,
        operation: &crate::search::operation::SearchOperation,
        activity: &Arc<Activity>,
    ) -> Result<()> {
        let permit = self.workers.background(operation).await?;
        let shared = self.clone();
        let project = project.clone();
        let call = operation.clone();
        let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
            call.check()?;
            crate::watch::report_async_event(
                &shared.cache,
                &project,
                event.clone(),
                policy,
                pending.as_deref(),
                shared.watchers.debug(),
                || shared.open_work_registry(),
            )?;
            #[cfg(test)]
            shared.watchers.emit_batch(event);
            Ok(())
        });
        request::run(worker, Some(operation)).await
    }

    pub(crate) async fn prepare_watch_pending(
        &self,
        project: &crate::store::models::Project,
        previous: Option<Arc<crate::watch::WatchPayload>>,
        first: Option<Vec<PathBuf>>,
        mut rx: mpsc::Receiver<Vec<PathBuf>>,
        operation: &crate::search::operation::SearchOperation,
        activity: &Arc<Activity>,
    ) -> Result<crate::watch::WatchPrepared> {
        let permit = match self.workers.background(operation).await {
            Ok(permit) => permit,
            Err(error) => {
                return Ok(crate::watch::WatchPrepared {
                    payload: Err(error),
                    rx,
                    _retention: activity.clone(),
                });
            }
        };
        let project = project.clone();
        let budget = self.payload_budget.clone();
        let watchers = self.watchers.clone();
        let call = operation.clone();
        let retention = activity.clone();
        let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
            let payload =
                if registry::root_state(&project.canonical_root) == registry::RootState::Missing {
                    Ok(None)
                } else {
                    crate::watch::merge_pending(&budget, previous, first, &mut rx, &call, &watchers)
                        .map(Some)
                };
            Ok(crate::watch::WatchPrepared {
                payload,
                rx,
                _retention: retention,
            })
        });
        request::run(worker, Some(operation)).await
    }

    pub(super) fn close_watchers(&self) {
        self.watchers.close();
    }

    pub(crate) fn watch_activity(&self) -> Arc<Activity> {
        self.activity.track()
    }
    pub(crate) fn watch_activity_source(&self) -> request::ActivitySource {
        self.activity.clone()
    }

    pub(crate) fn watch_config_candidate(
        &self,
        project_id: i64,
    ) -> crate::watch::WatchConfigCandidate {
        self.project_slot(project_id).watch_candidate.clone()
    }
    pub(crate) fn watch_operation(&self) -> Result<crate::search::operation::SearchOperation> {
        crate::search::operation::SearchOperation::with_timeout(
            Instant::now(),
            Duration::from_millis(self.global_config.remote.background_timeout_ms),
            crate::hybrid::work::Cancellation::new(),
        )
    }

    /// Prepared metadata readers shared by search phases and ANN refresh,
    /// independent of the writer mutex.
    #[must_use]
    pub fn read_pool(&self) -> &ReadPool {
        &self.readers
    }

    pub(super) fn take_activity_events(&self) -> Result<mpsc::Receiver<()>> {
        self.activity_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| Error::internal("daemon activity receiver already consumed"))
    }

    /// Complete immutable daemon policy captured by the controlling boundary.
    #[must_use]
    pub fn daemon_settings(&self) -> &DaemonSettings {
        &self.global_config.daemon
    }

    fn profile_denial() -> Error {
        Error::Admission { message: "website profile permits only its fixed project through read-only binding and explicit sync".to_owned() }
    }

    fn catalog_config_busy() -> Error {
        Error::Admission {
            message: "project configuration is busy; retry after the current index operation or reload settles".into(),
        }
    }

    fn admit_project(&self, project: &crate::store::models::Project) -> Result<()> {
        if let Some(fixed) = &self.website
            && (fixed.project.id != project.id
                || crate::platform::paths::normalize_for_compare(&fixed.project.canonical_root)
                    != crate::platform::paths::normalize_for_compare(&project.canonical_root))
        {
            return Err(Self::profile_denial());
        }
        Ok(())
    }

    fn admit_binding(&self, project_id: Option<i64>) -> Result<()> {
        if let Some(fixed) = &self.website
            && project_id != Some(fixed.project.id)
        {
            return Err(Self::profile_denial());
        }
        Ok(())
    }

    fn visible_projects(&self, db: &Db) -> Result<Vec<crate::store::models::Project>> {
        if let Some(fixed) = &self.website {
            let project =
                registry::project_by_id(db, fixed.project.id)?.ok_or_else(Self::profile_denial)?;
            self.admit_project(&project)?;
            Ok(vec![project])
        } else {
            registry::list_projects(db)
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open_work_registry(&self) -> Result<Db> {
        match self.catalog_epoch {
            Some(epoch) => crate::store::generation_build::open_index_registry(
                &self.cache,
                &self.db_settings,
                epoch,
            ),
            None => Db::open_with_settings(&self.cache, &self.db_settings),
        }
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

    fn reconcile_catalog_runtime(&self, project_id: i64, slot: &ProjectCell) -> Result<()> {
        if self.catalog_epoch.is_none()
            || slot
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .unverified
                .is_none()
        {
            return Ok(());
        }
        let reader = self.readers.try_acquire()?;
        let publication = slot.try_catalog_publication()?;
        let proof = slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unverified
            .as_ref()
            .map(|candidate| candidate.proof);
        let Some(proof) = proof else {
            return Ok(());
        };
        let (candidate, retired) = lifecycle::with_existing_project_read_lease(
            self.readers.cache_root(),
            project_id,
            || {
                // Leave the candidate available if the lease or read fails.
                // Keep the head stable through publication of the runtime.
                let db_error = crate::store::repo::db_error;
                let tx = reader
                    .connection()
                    .unchecked_transaction()
                    .map_err(db_error)?;
                let selected = proof.is_committed(&tx)?;
                tx.commit().map_err(db_error)?;
                let candidate = slot
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unverified
                    .take();
                let retired = candidate.as_ref().filter(|_| selected).map(|candidate| {
                    publication.install(
                        candidate.runtime.config.clone(),
                        candidate.runtime.stages.clone(),
                    )
                });
                Ok((candidate, retired))
            },
        )?;
        drop(reader);
        let reload = publication.release_preparation();
        if retired.is_some()
            && let Some(candidate) = &candidate
            && let Err(error) = self.watchers.revise_with_runtime(
                &self.cache,
                project_id,
                candidate.runtime.clone(),
            )
        {
            eprintln!("warning: cannot revise watcher for project {project_id}: {error}");
        }
        drop(reload);
        drop(retired);
        drop(candidate);
        Ok(())
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

    #[cfg(feature = "hybrid-remote")]
    pub(super) fn shutdown_remote(&self) {
        self.remote.shutdown();
    }

    #[cfg_attr(
        not(feature = "hybrid-remote"),
        expect(
            clippy::unused_self,
            reason = "The common stage factory consumes the Shared remote owner only when that capability is compiled"
        )
    )]
    fn async_stages(&self, config: &Config) -> Result<HybridStages> {
        #[cfg(feature = "hybrid-remote")]
        {
            crate::hybrid::config::from_config_with_execution(
                &config.hybrid_cfg,
                &config.providers,
                &self.remote,
            )
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
        }
    }

    fn project_runtime_for_search(
        &self,
        project: &crate::store::models::Project,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)> {
        self.project_runtime_kind(project, true, |config| self.async_stages(config))
    }

    fn project_runtime(
        &self,
        project: &crate::store::models::Project,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)> {
        if self.jobs.get().is_some() {
            return self.project_runtime_for_search(project);
        }
        self.project_runtime_with(project, |config| {
            crate::hybrid::blocking::with_bridge(self.activity.bridge.clone(), || {
                crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
            })
        })
    }

    // Private construction seam: tests can pause a real cache miss without
    // changing the provider registry or using process-global hooks.
    fn project_runtime_with<F>(
        &self,
        project: &crate::store::models::Project,
        build: F,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)>
    where
        F: FnMut(&Config) -> Result<HybridStages>,
    {
        self.project_runtime_kind(project, false, build)
    }

    fn project_runtime_kind<F>(
        &self,
        project: &crate::store::models::Project,
        search: bool,
        mut build: F,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)>
    where
        F: FnMut(&Config) -> Result<HybridStages>,
    {
        self.admit_project(project)?;
        let slot = self.project_slot(project.id);
        loop {
            self.reconcile_catalog_runtime(project.id, &slot)?;
            let config = {
                let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
                let config = self.config_in(&mut state, project)?;
                let remote = config.hybrid_cfg.embedding.as_ref().is_some_and(|stage| {
                    stage.provider == crate::hybrid::config::PROVIDER_OPENROUTER
                }) || config.hybrid_cfg.reranker.as_ref().is_some_and(|stage| {
                    stage.provider == crate::hybrid::config::PROVIDER_OPENROUTER
                });
                let cached = if search {
                    state
                        .search_stages
                        .as_ref()
                        .or_else(|| if remote { None } else { state.stages.as_ref() })
                } else {
                    state.stages.as_ref().or_else(|| {
                        if remote {
                            None
                        } else {
                            state.search_stages.as_ref()
                        }
                    })
                };
                if let Some(stages) = cached {
                    return Ok((config, Arc::clone(stages)));
                }
                config
            };
            // Construction can load models: do not hold the project lock.
            let outcome = if self.website.is_some() {
                Ok(HybridStages::default())
            } else {
                build(&config)
            };
            let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.unverified.is_some()
                || !state
                    .config
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &config))
            {
                // Neither an obsolete result nor its error may escape.
                continue;
            }
            let cached = if search {
                &mut state.search_stages
            } else {
                &mut state.stages
            };
            if let Some(stages) = cached.as_ref() {
                return Ok((config, Arc::clone(stages)));
            }
            let stages = Arc::new(outcome?);
            *cached = Some(Arc::clone(&stages));
            return Ok((config, stages));
        }
    }

    // Legacy policy belongs to the captured config, not a newer reload's map.
    // Owned policy is immutable and must never receive a project override here.
    fn vector_source_for(
        &self,
        project_id: i64,
        config: &Config,
    ) -> crate::vector_cache::VectorCacheSource {
        use crate::vector_cache::VectorCacheSource;
        match self.dense.vector_source(project_id) {
            VectorCacheSource::Legacy(_) => {
                VectorCacheSource::Legacy(config.hybrid_cfg.cache.limits())
            }
            owned @ VectorCacheSource::Owned(_) => owned,
        }
    }

    /// Enqueues the open-time backfill sync job when the embedding stage is
    /// on and the project is not fully covered (FR-15); cached chunks make a
    /// fully covered project a cheap no-op.
    ///
    /// # Errors
    /// Stage admission errors (propagated to the open) and registry failures.
    fn schedule_backfill(self: &Arc<Self>, project: &crate::store::models::Project) -> Result<()> {
        self.schedule_backfill_in(project, None)
    }

    fn schedule_backfill_in(
        self: &Arc<Self>,
        project: &crate::store::models::Project,
        operation: Option<crate::search::operation::SearchOperation>,
    ) -> Result<()> {
        if matches!(
            self.invalidation,
            crate::config::policy::PolicyContext::Managed(_)
        ) {
            // The startup receipt permits only its reviewed work. Opening a
            // project does not authorize a new embedding job for its vectors.
            return Ok(());
        }
        let operation = match operation {
            Some(operation) => operation,
            None => self.watch_operation()?,
        };
        self.admit_project(project)?;
        if self.website.is_some() {
            return Ok(());
        }
        let (config, stages) = self.project_runtime(project)?;
        if stages.embedder.is_none()
            || project.status != crate::store::models::ProjectStatus::Indexed
        {
            return Ok(());
        }
        let source = self.vector_source_for(project.id, &config);
        if self.catalog_epoch.is_some() {
            return self.schedule_catalog_backfill(project, config, stages, source, &operation);
        }
        let db = self.db();
        let Some(coverage) = hybrid_jobs::configured_coverage(&db, &config, project.id, &source)?
        else {
            return Ok(());
        };
        if coverage.cached >= coverage.total {
            return Ok(());
        }
        if crate::store::repo::active_sync_job(db.connection(), project.id)?.is_some() {
            return Ok(());
        }
        if let Some(service) = self.jobs.get() {
            // Reserve existing L/queue authority before allocating a detached
            // coordinator; opens cannot accumulate unadmitted Tokio tasks.
            drop(db);
            // Accepted automatic work owns cancellation independently of the
            // frontend, but retains the exact original absolute deadline.
            let detached = crate::search::operation::SearchOperation::with_timeout(
                operation.started_at(),
                operation.deadline().duration_since(operation.started_at()),
                crate::hybrid::work::Cancellation::new(),
            )?;
            let reservation = service.reserve(detached)?;
            let context = crate::index::async_job::JobContext {
                input: crate::index::async_job::JobInput::Sync { join_active: true },
                singleton: None,
                cache: self.cache.clone(),
                db_settings: self.db_settings.clone(),
                revision: Some(self.project_slot(project.id)),
                dense: Some(Arc::new(
                    self.dense.snapshot_with_build_settings(config.ann_build),
                )),
                project: project.clone(),
                config,
                stages,
                source,
            };
            let shared = self.clone();
            let activity = self.activity.track();
            drop(
                tokio::runtime::Handle::try_current()
                    .map_err(|error| {
                        Error::internal_with_source("background runtime owner absent", error)
                    })?
                    .spawn(async move {
                        let Some(service) = shared.jobs.get() else {
                            return;
                        };
                        if let Err(error) = crate::index::async_job::run_reserved(
                            service,
                            &shared.workers,
                            activity,
                            &shared.payload_budget,
                            context,
                            reservation,
                        )
                        .await
                        {
                            eprintln!("warning: automatic backfill: {error}");
                        }
                    }),
            );
            return Ok(());
        }
        let (job_id, created) =
            tools::enqueue_sync_with_retention(&db, project, self.daemon_settings().max_sync_jobs)?;
        if created {
            spawn_sync_job(
                Arc::clone(self),
                job_id,
                project.id,
                config,
                stages,
                source,
                (None, None),
            );
        }
        Ok(())
    }

    fn schedule_catalog_backfill(
        self: &Arc<Self>,
        project: &Project,
        config: Arc<Config>,
        stages: Arc<HybridStages>,
        source: crate::vector_cache::VectorCacheSource,
        operation: &crate::search::operation::SearchOperation,
    ) -> Result<()> {
        use crate::index::async_job::catalog_backfill;

        let Some(service) = self.jobs.get() else {
            return Ok(());
        };
        let revision = self.project_slot(project.id);
        let slot = revision
            .catalog_backfill
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)));
        let Ok(singleton) = slot.clone().try_acquire_owned() else {
            return Ok(());
        };
        let detached = crate::search::operation::SearchOperation::with_timeout(
            operation.started_at(),
            operation.deadline().duration_since(operation.started_at()),
            crate::hybrid::work::Cancellation::new(),
        )?;
        let Ok(reservation) = service.reserve(detached) else {
            // Opening a project remains available under backfill pressure.
            return Ok(());
        };
        let context = catalog_backfill::Context {
            project: project.clone(),
            readers: self.readers.clone(),
            revision,
            config,
            stages,
            source,
            pin_ttl: self.dense.limits().ann_lifetimes.backfill_pin_ttl(),
            _singleton: singleton,
        };
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            Error::internal_with_source("background runtime owner absent", error)
        })?;
        let service = service.clone();
        let shared = self.clone();
        let activity = self.activity.track();
        drop(runtime.spawn(async move {
            if let Err(error) = catalog_backfill::run(
                &service,
                &shared.workers,
                activity,
                &shared.payload_budget,
                context,
                reservation,
            )
            .await
            {
                eprintln!("warning: automatic catalog backfill: {error}");
            }
        }));
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
        if let Some(fixed) = &self.website {
            let root = root.canonicalize().map_err(|_| Self::profile_denial())?;
            if crate::platform::paths::normalize_for_compare(&root)
                != crate::platform::paths::normalize_for_compare(&fixed.project.canonical_root)
            {
                return Err(Self::profile_denial());
            }
        }
        let config = Config::for_project(&self.global_config, root)?;
        if self.website.is_some() {
            crate::daemon::launch::lexical_only(&config)?;
        }
        Ok(config)
    }

    fn config_in(
        &self,
        state: &mut ProjectState,
        project: &crate::store::models::Project,
    ) -> Result<Arc<Config>> {
        self.admit_project(project)?;
        if state.unverified.is_some() {
            return Err(Self::catalog_config_busy());
        }
        if let Some(config) = &state.config {
            return Ok(Arc::clone(config));
        }
        let config = Arc::new(self.config_for_root(&project.canonical_root)?);
        if self.website.is_none() && self.global_config.vector_store.is_none() {
            self.dense
                .set_vector_cache_max_bytes(project.id, config.hybrid_cfg.cache.max_bytes)?;
        }
        state.config = Some(Arc::clone(&config));
        Ok(config)
    }

    /// Configuration of `project` as of the last time it was opened; loaded on
    /// first use (OQ-6).
    ///
    /// # Errors
    /// Returns config read/parse errors.
    pub fn project_config(&self, project: &crate::store::models::Project) -> Result<Arc<Config>> {
        self.admit_project(project)?;
        let slot = self.project_slot(project.id);
        self.reconcile_catalog_runtime(project.id, &slot)?;
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
        if self.website.is_some() {
            return None;
        }
        let config = self.project_config(project).ok()?;
        let slot = self.project_slot(project.id);
        let runtime = {
            let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .config
                .as_ref()
                .filter(|current| Arc::ptr_eq(current, &config))
                .and(state.search_stages.as_ref().or(state.stages.as_ref()))
                .cloned()
        };
        let (model_id, warm) = match runtime {
            Some(stages) if self.catalog_epoch.is_some() => (
                stages.embedder.as_ref()?.model_id().to_owned(),
                stages.ranking == Ranking::Rrf,
            ),
            _ => (
                config
                    .providers
                    .embedding_model_id(config.hybrid_cfg.embedding.as_ref()?)
                    .ok()?,
                config.hybrid_cfg.enabled && config.hybrid_cfg.ranking == Ranking::Rrf,
            ),
        };
        let cache_path = self.cache.clone();
        let dense = self.dense.snapshot_with_build_settings(config.ann_build);
        let readers = Arc::clone(&self.readers);
        let project = project.clone();
        let project_id = project.id;
        Some(std::thread::spawn(move || {
            before_admission();
            if warm {
                let result = (|| {
                    let index = {
                        let reader = readers.try_acquire()?;
                        slot.with_preparation(&config, || {
                            ann_warm_binding(
                                reader,
                                &project,
                                &cache_path,
                                &model_id,
                                before_metadata,
                            )
                        })?
                    };
                    // The scalar lease is gone before entering the shared flight.
                    // Its factory has no personal query budget or outer READ gate.
                    let source = MetadataSource::Prepared(&readers);
                    dense.warm_bound_with_metadata_admitted(
                        BoundDenseRequest {
                            cache_root: &cache_path,
                            index,
                            model_id: &model_id,
                        },
                        &crate::hybrid::work::Cancellation::new(),
                        || tools::capture_dense(source, &project, index),
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
            if let Err(err) = dense.cleanup_ann_disk() {
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
        if self.jobs.get().is_some() {
            return self.open_project_config_with(project, |config| self.async_stages(config));
        }
        self.open_project_config_with(project, |config| {
            crate::hybrid::blocking::with_bridge(self.activity.bridge.clone(), || {
                crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)
            })
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
        self.publish_project_config(project, build, on_publication_contention, None)
            .map(|(config, _)| config)
    }

    fn stage_catalog_config(
        &self,
        project: &crate::store::models::Project,
        active: &Arc<Config>,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)> {
        let slot = self.project_slot(project.id);
        let _reload = slot.try_catalog_reload()?;
        slot.check_config_revision(active)?;
        let config = self.config_for_root(&project.canonical_root)?;
        if config == **active {
            return self.project_runtime_for_search(project);
        }
        let stages = self.async_stages(&config)?;
        catalog_stage_model(&config, &stages)?;
        Ok((Arc::new(config), Arc::new(stages)))
    }

    fn runtime_for_config_open(
        &self,
        slot: &ProjectCell,
        project: &crate::store::models::Project,
        desired: Arc<Config>,
        shared_catalog: bool,
    ) -> Result<(Arc<Config>, Option<Arc<HybridStages>>)> {
        let (current, stages) = {
            let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.unverified.is_some() {
                return Err(Self::catalog_config_busy());
            }
            (
                state.config.clone(),
                state
                    .search_stages
                    .as_ref()
                    .or(state.stages.as_ref())
                    .cloned(),
            )
        };
        let (config, stages) = if let Some(current) = current
            && (current == desired
                || (shared_catalog
                    && catalog_requires_index(&current, &desired)?
                    && crate::store::project_index::catalog_needs_index_for(
                        self.readers.try_acquire()?,
                        project,
                        &crate::index::outcome::recipe_id(&desired),
                        configured_catalog_embedding(&desired)?,
                    )?)) {
            // Opens retain the active recipe until explicit Index can
            // publish the required pair together with the new settings.
            (current, stages)
        } else {
            (desired, None)
        };
        if shared_catalog
            && matches!(
                self.invalidation,
                crate::config::policy::PolicyContext::Managed(_)
            )
            && crate::store::project_index::catalog_needs_index_for(
                self.readers.try_acquire()?,
                project,
                &crate::index::outcome::recipe_id(&config),
                configured_catalog_embedding(&config)?,
            )?
        {
            return Err(Error::Admission {
                message: "published index does not match the project settings; run docsbase index"
                    .into(),
            });
        }
        Ok((config, stages))
    }

    fn publish_project_config<F, O>(
        &self,
        project: &crate::store::models::Project,
        build: F,
        on_publication_contention: O,
        explicit_index_db: Option<&Db>,
    ) -> Result<(Arc<Config>, Arc<HybridStages>)>
    where
        F: FnOnce(&Config) -> Result<HybridStages>,
        O: FnOnce(),
    {
        self.admit_project(project)?;
        let slot = self.project_slot(project.id);
        self.reconcile_catalog_runtime(project.id, &slot)?;
        // Only same-project reloads wait for construction. Existing runtime
        // reads and unrelated projects keep their last coherent state.
        let shared_catalog = self.catalog_epoch.is_some() && self.website.is_none();
        let _reload = if shared_catalog {
            slot.try_catalog_reload()?
        } else {
            slot.reload.lock().unwrap_or_else(PoisonError::into_inner)
        };
        let config = Arc::new(self.config_for_root(&project.canonical_root)?);
        let (config, stages) = if self.website.is_some()
            || self.catalog_epoch.is_some()
            || explicit_index_db.is_some()
        {
            self.runtime_for_config_open(&slot, project, config, shared_catalog)?
        } else {
            (config, None)
        };
        let cached = stages.is_some();
        let stages = match stages {
            Some(stages) => stages,
            None if self.website.is_some() => Arc::new(HybridStages::default()),
            None => Arc::new(build(&config)?),
        };
        if shared_catalog {
            self.verify_catalog_reopen(project, &config, &stages)?;
        }
        if cached {
            return Ok((config, stages));
        }
        let watch_runtime = if self.website.is_none() && self.watchers.enabled() {
            let source = self.vector_source_for(project.id, &config);
            source.validate_root(&self.cache)?;
            Some(Arc::new(crate::watch::WatchRuntime::new(
                Arc::clone(&config),
                Arc::clone(&stages),
                source,
            )))
        } else {
            None
        };
        // Never wait for active preparation while retaining the state mutex.
        let _publication = match slot.preparation.try_write() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                on_publication_contention();
                if shared_catalog {
                    return Err(Self::catalog_config_busy());
                }
                slot.preparation
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
            }
        };
        // Sync creates its row while holding a read lease on this project.
        // Check again under the publication lease before replacing its settings.
        if let Some(db) = explicit_index_db
            && crate::store::repo::active_sync_job(db.connection(), project.id)?.is_some()
        {
            return Err(crate::index::async_job::active_sync_conflict());
        }
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.website.is_none() && self.global_config.vector_store.is_none() {
            self.dense
                .set_vector_cache_max_bytes(project.id, config.hybrid_cfg.cache.max_bytes)?;
        }
        let dense_changed = state
            .config
            .as_deref()
            .is_none_or(|previous| config.changes_dense_state(previous));
        if dense_changed && self.website.is_none() {
            self.dense.invalidate_memory(project.id);
        }
        // All fallible preparation steps precede publication: a rejected
        // reload leaves the last valid runtime and its policies intact.
        state.config = Some(Arc::clone(&config));
        if self.jobs.get().is_some() {
            state.stages = None;
            state.search_stages = Some(Arc::clone(&stages));
        } else {
            state.stages = Some(Arc::clone(&stages));
            state.search_stages = None;
        }
        // Provider construction may refresh credentials even when Config values
        // are equal. Every published runtime must reach the watcher as one pair.
        if let Some(watch_runtime) = watch_runtime {
            let watcher = self
                .watchers
                .revise_with_runtime(&self.cache, project.id, Arc::clone(&watch_runtime))
                .and_then(|revised| {
                    if revised {
                        Ok(false)
                    } else {
                        self.watchers.ensure_with_runtime(
                            &self.cache,
                            project,
                            watch_runtime,
                            &self.db_settings,
                        )
                    }
                });
            if let Err(err) = watcher {
                eprintln!("warning: cannot watch project {}: {err}", project.id);
            }
        }
        Ok((config, stages))
    }

    fn verify_catalog_reopen(
        &self,
        project: &Project,
        config: &Config,
        stages: &HybridStages,
    ) -> Result<()> {
        let model = catalog_stage_model(config, stages)?;
        crate::store::project_index::require_selected_catalog_embedding(
            self.readers.try_acquire()?,
            project,
            model,
        )
    }

    fn restore_watchers(&self) {
        match self.start_watchers() {
            Ok(started) if started > 0 => eprintln!("watching {started} indexed project(s)"),
            Ok(_) => {}
            Err(err) => eprintln!("warning: cannot start watchers: {err}"),
        }
    }

    /// Starts watchers for every indexed project in the registry (daemon
    /// startup); returns how many were started.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the registry cannot be listed.
    fn start_watchers(&self) -> Result<usize> {
        if self.website.is_some() || !self.watchers.enabled() {
            return Ok(0);
        }
        let projects = registry::list_projects(&self.db())?;
        let mut started = 0;
        for project in projects {
            let restored = if self.catalog_epoch.is_some() {
                self.restore_catalog_watcher(&project)
            } else {
                self.ensure_watcher(&project)
            };
            match restored {
                Ok(true) => started += 1,
                Ok(false) => {}
                Err(err) => {
                    eprintln!("warning: cannot watch project {}: {err}", project.id);
                }
            }
        }
        Ok(started)
    }

    fn restore_catalog_watcher(&self, project: &Project) -> Result<bool> {
        let local = {
            let reader = self.readers.try_acquire()?;
            crate::store::project_index::local_watcher_project(&reader, project)?
        };
        let Some(local) = local.filter(|local| {
            local.status == crate::store::models::ProjectStatus::Indexed
                && registry::root_state(&local.canonical_root) != registry::RootState::Missing
        }) else {
            return Ok(false);
        };
        // Publish the matching stages before creating the native consumer.
        self.open_project_config(&local)?;
        Ok(self.watchers.watching(local.id))
    }

    /// Starts a watcher for an indexed project using its open-time config.
    ///
    /// # Errors
    /// Returns config/spawn errors; a failed start is not fatal to callers.
    pub fn ensure_watcher(&self, project: &crate::store::models::Project) -> Result<bool> {
        self.ensure_watcher_observing(project, || {})
    }

    fn ensure_watcher_observing(
        &self,
        project: &crate::store::models::Project,
        before_admission: impl FnOnce(),
    ) -> Result<bool> {
        self.admit_project(project)?;
        if self.website.is_some() || !self.watchers.enabled() {
            return Ok(false);
        }
        let local;
        let project = if self.catalog_epoch.is_some() {
            let reader = self.readers.try_acquire()?;
            local = crate::store::project_index::local_watcher_project(&reader, project)?;
            let Some(project) = &local else {
                return Ok(false);
            };
            project
        } else {
            project
        };
        if project.status != crate::store::models::ProjectStatus::Indexed
            || registry::root_state(&project.canonical_root) == registry::RootState::Missing
        {
            return Ok(false);
        }
        let slot = self.project_slot(project.id);
        let mut before_admission = Some(before_admission);
        loop {
            // Model construction runs outside project/state and registry guards.
            let (config, stages) = self.project_runtime(project)?;
            let source = self.vector_source_for(project.id, &config);
            if let Some(observe) = before_admission.take() {
                observe();
            }
            // Serialize creation with reload publication. A later revision must
            // not be followed by creation from this obsolete captured pair.
            let _reload = slot.reload.lock().unwrap_or_else(PoisonError::into_inner);
            let current = slot
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .config
                .as_ref()
                .is_some_and(|latest| Arc::ptr_eq(latest, &config));
            if !current {
                continue;
            }
            return self.watchers.ensure_with_runtime(
                &self.cache,
                project,
                Arc::new(crate::watch::WatchRuntime::new(config, stages, source)),
                &self.db_settings,
            );
        }
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
        crate::daemon::launch::runtime_restart_notice(
            self.global_config_path.as_deref(),
            &self.global_config_stamp,
            &self.global_config,
            &self.global_config_overrides,
            self.global_config_defaults,
            self.catalog_epoch.is_some(),
        )
    }
}

/// Bound for waiting until in-flight requests settle before shutdown (T46).
pub const DRAIN_TIMEOUT: Duration =
    Duration::from_millis(crate::daemon::settings::DaemonSpec::DEFAULTS.reply_drain_ms);

/// Stderr tracing for connection lifecycle (`DOCSBASE_IPC_DEBUG=1`).
fn ipc_debug(shared: &Shared, message: std::fmt::Arguments<'_>) {
    if shared.ipc_debug {
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
pub(super) fn handle_admitted_connection(
    shared: Arc<Shared>,
    stream: Stream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> impl std::future::Future<Output = ()> + Send {
    let connection = Arc::new(permit);
    let attached = stream.ownership().retain_connection(connection.clone());
    async move {
        if attached.is_err() {
            return;
        }
        handle_connection_in(
            shared,
            stream,
            in_flight,
            events,
            shutdown,
            Some(connection),
        )
        .await;
    }
}

async fn write_frame_error(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    frame: Frame,
    deadline: Instant,
) {
    let message = match frame {
        Frame::TooLarge => format!("frame exceeds {} bytes", protocol::MAX_FRAME_BYTES),
        _ => "incomplete frame without delimiter".to_owned(),
    };
    let response = error_response(&Error::Protocol { message });
    let _ = scoped::reply(writer, response, None, false, true, deadline).await;
}

struct StopRequest<'a> {
    shutdown: &'a watch::Sender<bool>,
    in_flight: &'a AtomicUsize,
    deadline: Instant,
}

#[allow(clippy::too_many_lines)] // Ordered request ownership, delivery and connection cancellation form one scope.
async fn handle_connection_in(
    shared: Arc<Shared>,
    stream: Stream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
    connection: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
) {
    let ownership = stream.ownership();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = FrameReader::new(BufReader::new(read_half));
    let mut peer = Peer::default();

    {
        let work = async {
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
                        ipc_debug(
                            &shared,
                            format_args!(
                                "frame {} bytes on session {:?}",
                                line.len(),
                                peer.session_id
                            ),
                        );
                        line
                    }
                    Ok(Frame::Eof) => {
                        ipc_debug(&shared, format_args!("connection EOF"));
                        break;
                    }
                    Err(err) => {
                        ipc_debug(&shared, format_args!("connection read error: {err}"));
                        break;
                    }
                    Ok(frame @ (Frame::Incomplete | Frame::TooLarge)) => {
                        let guard = Activity::new(&in_flight, &events, connection.as_ref());
                        if ownership.retain_request(guard.clone()).is_err() {
                            break;
                        }
                        let deadline = Instant::now()
                            + Duration::from_millis(shared.daemon_settings().control_timeout_ms);
                        write_frame_error(&mut writer, frame, deadline).await;
                        break;
                    }
                };
                let guard = Activity::new(&in_flight, &events, connection.as_ref());
                if ownership.retain_request(guard.clone()).is_err() {
                    break;
                }
                let decoded = protocol::decode_request(&line);
                // Stop's drain and final ACK share its original whole budget.
                let stop = matches!(&decoded, Ok(Request::StopDaemon)).then(|| StopRequest {
                    shutdown: &shutdown,
                    in_flight: &in_flight,
                    deadline: Instant::now()
                        + Duration::from_millis(shared.daemon_settings().reply_drain_ms),
                });
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
                            stop.as_ref(),
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
                // Non-search control delivery starts when its response is ready.
                // Search and Stop retain their original absolute deadlines.
                let control_deadline = stop.map_or_else(
                    || {
                        Instant::now()
                            + Duration::from_millis(shared.daemon_settings().control_timeout_ms)
                    },
                    |stop| stop.deadline,
                );
                let delivered = scoped::reply(
                    &mut writer,
                    response,
                    operation,
                    immediate,
                    close,
                    control_deadline,
                )
                .await;
                if delivered {
                    ownership.request_completed();
                }
                drop(guard);
                if !delivered {
                    break;
                }
                if close {
                    break;
                }
            }
        };
        if connection.is_some() {
            // Graceful stop closes this token only after drain; signal
            // escalation closes it immediately. Drop a wedged reply/task while
            // physical closures keep their original Activities and permits.
            tokio::select! {
                biased;
                () = shared.workers.cancelled() => {},
                () = work => {},
            }
        } else {
            work.await;
        }
    }

    if let Some(id) = peer.session_id {
        ipc_debug(&shared, format_args!("leaving session {id}"));
        shared.sessions.leave(id);
    }
    let _ = events.try_send(());
}

#[allow(clippy::too_many_lines)] // Exhaustive wire dispatcher; each arm owns one protocol operation.
async fn handle_request<R: tokio::io::AsyncBufRead + Unpin>(
    shared: &Arc<Shared>,
    stop: Option<&StopRequest<'_>>,
    peer: &mut Peer,
    request: Request,
    scope: &scoped::Scope,
    reader: &mut FrameReader<R>,
) -> (Response, bool) {
    // Closing ingress does not cancel work already accepted by another
    // handler: graceful installation must let its later CPU phases finish.
    if shared.stopping.load(Ordering::SeqCst) && !matches!(&request, Request::StopDaemon) {
        return (
            error_response(&Error::Admission {
                message: "daemon is shutting down".into(),
            }),
            true,
        );
    }
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
                        schema_version: shared.registry_schema(),
                        cache_root: &shared.cache,
                        pid: std::process::id(),
                        recorded_build_id: Some(&daemon_build),
                        recorded_schema_version: Some(shared.registry_schema()),
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
                    schema_version: shared.registry_schema(),
                    capabilities: {
                        let mut capabilities = vec![
                            protocol::SEARCH_CONTEXT_CAPABILITY.to_owned(),
                            protocol::LITERAL_QUERY_CAPABILITY.to_owned(),
                            protocol::PANEL_READ_CAPABILITY.to_owned(),
                        ];
                        if shared.global_config.vector_store.is_some() {
                            capabilities.push(protocol::ACTIVE_CONFIG_V2_CAPABILITY.to_owned());
                        }
                        if shared.website.is_some() {
                            capabilities.push(protocol::ACTIVE_WEBSITE_CAPABILITY.to_owned());
                            capabilities.push(protocol::SERVER_STATUS_CAPABILITY.to_owned());
                        }
                        if shared.catalog_epoch.is_some() {
                            capabilities.push(protocol::CHUNK_REF_CAPABILITY.to_owned());
                        }
                        capabilities
                    },
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
                Some(Arc::clone(&scope.activity)),
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
                Some(Arc::clone(&scope.activity)),
            )
            .await
        }
        Request::CallTool { name, args } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            if name == "ask_docs" {
                return scoped::call_named(shared, peer, &name, args, scope, reader).await;
            }
            call_and_respond(shared, peer.session_id, &name, args, scope).await
        }
        Request::Search { args, .. } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            scoped::call(shared, peer, args, scope, reader).await
        }
        Request::InspectProjectConfig {
            protocol_version,
            build_id,
            path,
        } => {
            let result =
                live_config::inspect(shared, scope, protocol_version, &build_id, path).await;
            (
                result.map_or_else(
                    |error| error_response(&error),
                    |value| Response::ToolResult { value },
                ),
                true,
            )
        }
        Request::ApplyProjectConfig {
            path,
            plan_id,
            allow_remote,
            follow_job,
        } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            let result =
                live_config::apply(shared, scope, path, plan_id, allow_remote, follow_job).await;
            (
                result.map_or_else(
                    |error| error_response(&error),
                    |value| Response::ToolResult { value },
                ),
                false,
            )
        }
        Request::BindReadOnly { pid, project_id } => {
            if !peer.hello_done {
                return (hello_required(), true);
            }
            if let Err(error) = shared.admit_binding(project_id) {
                return (error_response(&error), false);
            }
            let permit = match shared.workers.control().await {
                Ok(permit) => permit,
                Err(error) => return (error_response(&error), false),
            };
            let reader_shared = Arc::clone(shared);
            let activity = Arc::clone(&scope.activity);
            let bound = request::spawn(Some(permit), Some(activity), move || {
                reader_shared.admit_binding(project_id)?;
                let project = project_id
                    .map(|id| {
                        registry::project_by_id(&reader_shared.db(), id)?.ok_or_else(|| {
                            Error::Project {
                                message: "unknown project".into(),
                                instruction: None,
                            }
                        })
                    })
                    .transpose()?;
                if let Some(project) = &project {
                    reader_shared.admit_project(project)?;
                    if reader_shared.website.is_some() {
                        reader_shared.open_project_config(project)?;
                    }
                }
                Ok::<_, Error>(
                    project.map_or_else(|| reader_shared.cache.clone(), |p| p.canonical_root),
                )
            })
            .await
            .map_err(|e| Error::internal_with_source("read-only bind", e))
            .and_then(|r| r);
            match bound.and_then(|cwd| shared.sessions.join(pid, cwd, project_id)) {
                Ok((id, cancel)) => {
                    if let Some(old) = peer.session_id.replace(id) {
                        shared.sessions.leave(old);
                    }
                    peer.cancel_rx = Some(cancel);
                    (
                        Response::ToolResult {
                            value: json!({"session_id": id}),
                        },
                        false,
                    )
                }
                Err(error) => (error_response(&error), false),
            }
        }
        Request::StopDaemon => {
            let Some(stop) = stop else {
                return (
                    error_response(&Error::internal("missing stop context")),
                    true,
                );
            };
            // Idempotent: only the first stop drains and signals shutdown, so
            // concurrent stops (install retries) cannot wait on each other
            // (FR-5, FR-6). The drain lets every other request finish; the
            // process then lives until background jobs complete.
            if !shared.stopping.swap(true, Ordering::SeqCst) {
                shared.close_watchers();
                let settings = shared.daemon_settings();
                wait_for_drain(
                    stop.in_flight,
                    tokio::time::Instant::from_std(stop.deadline),
                    Duration::from_millis(settings.drain_poll_ms),
                )
                .await;
                let _ = stop.shutdown.send(true);
            }
            (Response::ToolResult { value: Value::Null }, true)
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
async fn wait_for_drain(
    in_flight: &AtomicUsize,
    deadline: tokio::time::Instant,
    poll: Duration,
) -> bool {
    wait_for_drain_with(
        in_flight,
        deadline,
        poll,
        tokio::time::Instant::now,
        tokio::time::sleep_until,
    )
    .await
}

async fn wait_for_drain_with<N, S, F>(
    in_flight: &AtomicUsize,
    deadline: tokio::time::Instant,
    poll: Duration,
    mut now: N,
    mut sleep_until: S,
) -> bool
where
    N: FnMut() -> tokio::time::Instant,
    S: FnMut(tokio::time::Instant) -> F,
    F: std::future::Future<Output = ()>,
{
    while in_flight.load(Ordering::SeqCst) > 1 {
        let current = now();
        if current >= deadline {
            return false;
        }
        let wake_at = current + poll.min(deadline.duration_since(current));
        sleep_until(wake_at).await;
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
    activity: Option<Arc<Activity>>,
) -> (Response, bool) {
    match register_session_in(shared, pid, cwd, allow_unbound, activity).await {
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

#[cfg(test)]
async fn register_session(
    shared: &Arc<Shared>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    register_session_in(shared, pid, cwd, allow_unbound, None).await
}

async fn register_session_in(
    shared: &Arc<Shared>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
    activity: Option<Arc<Activity>>,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    if shared.website.is_some() {
        return Err(Shared::profile_denial());
    }
    if shared.jobs.get().is_some() {
        return registration_async::run(shared, pid, cwd, allow_unbound, activity).await;
    }
    let permit = shared.workers.control().await?;
    let shared_for_task = Arc::clone(shared);
    let cwd = cwd.to_path_buf();
    let cwd_for_task = cwd.clone();
    let outcome = request::spawn(
        Some(permit),
        activity,
        move || -> Result<crate::store::models::Project> {
            #[cfg(test)]
            shared_for_task.observe_registration_work();
            // Own connection: the shared read connection must stay free while a
            // full index runs (F4/NFR-1).
            let mut db =
                Db::open_with_settings(&shared_for_task.cache, &shared_for_task.db_settings)?;
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
            // Publish policy only through the per-project authority; a raw config
            // must not overwrite a concurrent reload's legacy dense budget.
            let (config, stages) = shared_for_task.project_runtime(&project)?;
            if !config.auto_index {
                return Err(project_err);
            }
            let source = shared_for_task.vector_source_for(project.id, &config);
            if let Err(err) = tools::run_project_index_with_vectors(
                &mut db,
                &shared_for_task.cache,
                &project,
                &config,
                &stages,
                &source,
            ) {
                // A concurrent registration may have won the writer lease and
                // already indexed the project; bind to that result instead.
                if let Some(indexed) =
                    registry::resolve_by_cwd(&db, &cwd_for_task)
                        .ok()
                        .filter(|project| {
                            project.status == crate::store::models::ProjectStatus::Indexed
                        })
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
        },
    )
    .await
    .map_err(|err| Error::internal_with_source("session task join", err))?;

    finish_registration(shared, pid, &cwd, allow_unbound, outcome)
}

#[path = "registration_async.rs"]
mod registration_async;

fn finish_registration(
    shared: &Shared,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
    outcome: Result<crate::store::models::Project>,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    match outcome {
        Ok(project) => {
            let (id, cancel) = shared
                .sessions
                .join(pid, cwd.to_path_buf(), Some(project.id))?;
            ipc_debug(shared, format_args!("bound session {id} (pid {pid})"));
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
            let (id, cancel) = shared.sessions.join(pid, cwd.to_path_buf(), None)?;
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
    if name == "ask_docs" {
        return crate::search::operation::SearchOperation::with_timeout(
            Instant::now(),
            crate::ask::AskLimits::TIMEOUT,
            crate::hybrid::work::Cancellation::new(),
        )
        .map(Some);
    }
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
    let mut cancellation = request::CancelOnDrop(
        operation
            .as_ref()
            .map(|operation| operation.cancellation().clone()),
    );
    if name == "ask_docs" {
        #[cfg(feature = "hybrid-remote")]
        {
            let operation = operation
                .as_ref()
                .ok_or_else(|| Error::internal("ask_docs operation missing"))?;
            let result = ask::run(shared, session_id, args, operation.clone(), activity).await;
            cancellation.0 = None;
            return result;
        }
        #[cfg(not(feature = "hybrid-remote"))]
        return Err(Error::Admission {
            message: "ask_docs requires the hybrid-remote build feature".into(),
        });
    }
    if name == "search_docs"
        && let Some(operation) = operation.as_ref()
    {
        let result = search_async::run(shared, session_id, args, operation.clone(), activity).await;
        cancellation.0 = None;
        return result;
    }
    if matches!(name, "index_project" | "sync_start")
        && let Some(service) = shared.jobs.get()
    {
        return index_async(
            shared,
            service,
            session_id,
            args,
            activity,
            name == "sync_start",
        )
        .await;
    }
    let permit = match &operation {
        Some(operation) => shared.workers.search(operation).await?,
        None => shared.workers.control().await?,
    };
    let shared = Arc::clone(shared);
    let name = name.to_owned();
    let worker_operation = operation.clone();
    let queued_at = Instant::now();
    let child_activity = activity.clone();
    if let Some(operation) = &operation {
        operation.check()?;
    }
    let worker = request::spawn(Some(permit), activity, move || {
        if let Some(operation) = &worker_operation {
            operation.record_queue(queued_at.elapsed());
        }
        route_tool(
            &shared,
            session_id,
            &name,
            &args,
            worker_operation,
            child_activity,
        )
    });
    let result = request::run(worker, operation.as_ref()).await;
    cancellation.0 = None;
    result
}

fn prepare_index_context(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: &Value,
    detached: bool,
) -> Result<std::result::Result<crate::index::async_job::JobContext, Value>> {
    let mut db = Db::open_with_settings(&shared.cache, &shared.db_settings)?;
    if detached && shared.website.is_some() {
        let bound = bound_project(shared, &db, session)?;
        shared.admit_binding(opt_i64(args, "project_id")?.or(Some(bound.id)))?;
    }
    let project = if detached {
        let requested = match opt_i64(args, "project_id")? {
            Some(id) => id,
            None => bound_project(shared, &db, session)?.id,
        };
        shared.admit_binding(Some(requested))?;
        registry::project_by_id(&db, requested)?.ok_or_else(|| Error::Project {
            message: format!("unknown project id {requested}"),
            instruction: Some("call list_projects".into()),
        })?
    } else {
        let path = match opt_str(args, "path")? {
            Some(raw) => {
                let raw = PathBuf::from(raw);
                if raw.is_absolute() {
                    raw
                } else {
                    session_cwd(shared, session)?.join(raw)
                }
            }
            None => session_cwd(shared, session)?,
        };
        let root = registry::project_root_for(&path, &shared.cache)?;
        registry::ensure_project(&mut db, &root)?
    };
    shared.admit_project(&project)?;
    registry::ensure_root_present(&project)?;
    let refresh_config = !detached
        && shared.global_config.vector_store.is_some()
        && args.get("refresh_config").and_then(Value::as_bool) == Some(true);
    if let Some(job) = crate::store::repo::active_sync_job(db.connection(), project.id)? {
        if refresh_config {
            return Err(crate::index::async_job::active_sync_conflict());
        }
        return Ok(Err(tools::sync_status(&db, &json!({"job_id":job.id}))?));
    }
    let singleton = if shared.website.is_some() {
        Some(
            shared
                .sync_slot
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Admission {
                    message: "fixed-project sync is still finishing".into(),
                })?,
        )
    } else {
        None
    };
    let (config, stages) = if refresh_config {
        // Explicit v2 Index can arrive on a registry-only session. Refresh the
        // project and retain the same revision to admit its index job.
        shared.publish_project_config(
            &project,
            |config| shared.async_stages(config),
            || {},
            Some(&db),
        )?
    } else {
        shared.project_runtime_for_search(&project)?
    };
    let source = shared.vector_source_for(project.id, &config);
    Ok(Ok(crate::index::async_job::JobContext {
        input: crate::index::async_job::JobInput::Sync {
            join_active: !refresh_config,
        },
        singleton,
        cache: shared.cache.clone(),
        db_settings: shared.db_settings.clone(),
        revision: Some(shared.project_slot(project.id)),
        dense: Some(Arc::new(
            shared.dense.snapshot_with_build_settings(config.ann_build),
        )),
        project,
        config,
        stages,
        source,
    }))
}

async fn finish_index_watch(
    shared: &Arc<Shared>,
    project_id: i64,
    operation: &crate::search::operation::SearchOperation,
    activity: &Arc<Activity>,
) {
    if !shared.watchers.enabled()
        || shared.stopping.load(Ordering::SeqCst)
        || operation.check().is_err()
    {
        return;
    }
    let permit = tokio::select! {
        biased;
        () = operation.cancellation().cancelled() => return,
        () = tokio::time::sleep_until(tokio::time::Instant::from_std(operation.deadline())) => return,
        permit = shared.workers.control() => match permit {
            Ok(permit) => permit,
            Err(_) => return,
        },
    };
    let owner = shared.clone();
    let call = operation.clone();
    let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
        call.check()?;
        let fresh = {
            let db = owner.db();
            registry::project_by_id(&db, project_id)?
        };
        if let Some(project) = fresh
            && let Err(error) = owner.ensure_watcher(&project)
        {
            eprintln!("warning: cannot watch project {project_id}: {error}");
        }
        Ok(())
    });
    let _ = request::run(worker, Some(operation)).await;
}

async fn index_async(
    shared: &Arc<Shared>,
    service: &super::job_service::JobService,
    session: Option<u64>,
    args: Value,
    activity: Option<Arc<Activity>>,
    detached: bool,
) -> Result<Value> {
    if shared.website.is_some() && !detached {
        return Err(Shared::profile_denial());
    }
    let operation = crate::search::operation::SearchOperation::with_timeout(
        Instant::now(),
        Duration::from_millis(shared.global_config.remote.background_timeout_ms),
        crate::hybrid::work::Cancellation::new(),
    )?;
    let mut cancellation = request::CancelOnDrop(Some(operation.cancellation().clone()));
    let result = index_async_with_operation(
        shared, service, session, args, activity, detached, operation,
    )
    .await;
    cancellation.0 = None;
    result
}

async fn index_async_with_operation(
    shared: &Arc<Shared>,
    service: &super::job_service::JobService,
    session: Option<u64>,
    args: Value,
    activity: Option<Arc<Activity>>,
    detached: bool,
    operation: crate::search::operation::SearchOperation,
) -> Result<Value> {
    if shared.catalog_epoch.is_some() {
        return start_catalog_sync(
            shared, service, session, args, activity, operation, detached,
        )
        .await;
    }
    // The outer request owns cancellation; this phase borrows its original
    // operation so an ordinary preparation error cannot cancel a later recovery.
    let activity = activity.unwrap_or_else(|| shared.activity.track());
    let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
        .wait(
            &crate::hybrid::work::Cancellation::new(),
            shared.workers.control(),
        )
        .await?;
    let context_owner = shared.clone();
    let captured_operation = operation.clone();
    let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
        captured_operation.check()?;
        prepare_index_context(&context_owner, session, &args, detached)
    });
    let context = match request::run(worker, Some(&operation)).await? {
        Ok(context) => context,
        Err(existing) => {
            return Ok(existing);
        }
    };
    let project_id = context.project.id;
    let result = if detached {
        let (mut owner, work, receiver) = crate::index::async_job::start(
            service,
            &shared.workers,
            activity.clone(),
            &operation,
            &shared.payload_budget,
            context,
        )
        .await?;
        let (next, status) = service
            .phase(
                &shared.workers,
                activity.clone(),
                &operation,
                owner,
                |state| state.acknowledgement(),
            )
            .await?;
        owner = next;
        let status = (**status).clone();
        let service = service.clone();
        let workers = shared.workers.clone();
        let shared = shared.clone();
        let operation = operation.clone();
        let activity = activity.clone();
        // No await between ownership transfer to the detached task and acknowledgement.
        drop(tokio::spawn(async move {
            let result = crate::index::async_job::execute(
                &service,
                &workers,
                activity.clone(),
                operation.clone(),
                owner,
                work,
                receiver,
            )
            .await;
            if result.is_ok() {
                finish_index_watch(&shared, project_id, &operation, &activity).await;
            }
        }));
        Ok(status)
    } else {
        crate::index::async_job::run(
            service,
            &shared.workers,
            activity.clone(),
            operation.clone(),
            &shared.payload_budget,
            context,
        )
        .await
    };
    if !detached && result.is_ok() {
        finish_index_watch(shared, project_id, &operation, &activity).await;
        operation.check()?;
    }
    result
}

async fn start_catalog_sync(
    shared: &Arc<Shared>,
    service: &super::job_service::JobService,
    session: Option<u64>,
    args: Value,
    activity: Option<Arc<Activity>>,
    operation: crate::search::operation::SearchOperation,
    detached: bool,
) -> Result<Value> {
    let epoch = shared
        .catalog_epoch
        .ok_or_else(|| Error::internal("catalog job has no registry epoch"))?;
    let activity = activity.unwrap_or_else(|| shared.activity.track());
    let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
        .wait(
            &crate::hybrid::work::Cancellation::new(),
            shared.workers.control(),
        )
        .await?;
    let context_owner = shared.clone();
    let call = operation.clone();
    let capture = request::spawn(Some(permit), Some(activity.clone()), move || {
        call.check()?;
        capture_catalog_sync(&context_owner, session, &args, epoch, detached)
    });
    let context = match request::run(capture, Some(&operation)).await? {
        Ok(context) => context,
        Err(existing) => return Ok(existing),
    };

    run_catalog_sync(shared, service, context, activity, operation, detached).await
}

async fn run_catalog_sync(
    shared: &Arc<Shared>,
    service: &super::job_service::JobService,
    context: catalog_sync::Context,
    activity: Arc<Activity>,
    operation: crate::search::operation::SearchOperation,
    detached: bool,
) -> Result<Value> {
    let project_id = context.build.project.id;
    let warm = (shared.website.is_none()
        && context.stages.ranking == Ranking::Rrf
        && context.stages.embedder.is_some())
    .then(|| CatalogAnnWarm {
        project: context.build.project.clone(),
        config: context.build.config.clone(),
        stages: context.stages.clone(),
    });
    let (owner, acknowledgement) =
        admit_catalog_sync(shared, service, &activity, &operation, context).await?;
    let run = {
        let service = service.clone();
        let shared = shared.clone();
        let operation = operation.clone();
        let activity = activity.clone();
        async move {
            let (_, published) =
                catalog_sync::execute(&service, &shared.workers, &activity, &operation, owner)
                    .await?;
            finish_index_watch(&shared, project_id, &operation, &activity).await;
            if let Some(warm) = warm {
                warm.schedule(&shared, &service, &operation, published);
            }
            Ok::<_, Error>(())
        }
    };
    // The JobRoot owns the terminal envelope and native work even if the
    // connection is dropped after acknowledgement.
    if detached {
        drop(tokio::spawn(run));
        return Ok(acknowledgement);
    }
    run.await?;
    let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
        .wait(
            &crate::hybrid::work::Cancellation::new(),
            shared.workers.control(),
        )
        .await?;
    let shared = shared.clone();
    let status = request::spawn(Some(permit), Some(activity), move || {
        tools::sync_status(&shared.db(), &acknowledgement)
    });
    request::run(status, Some(&operation)).await
}

async fn admit_catalog_sync(
    shared: &Shared,
    service: &super::job_service::JobService,
    activity: &Arc<Activity>,
    operation: &crate::search::operation::SearchOperation,
    context: catalog_sync::Context,
) -> Result<(super::job_service::JobOwner, Value)> {
    let owner = service
        .admit_with_account(
            shared.workers.clone(),
            activity.clone(),
            operation,
            &shared.payload_budget,
            None,
        )
        .await?;
    let call = operation.clone();
    let (owner, status) = service
        .phase(
            &shared.workers,
            activity.clone(),
            operation,
            owner,
            move |state| state.prepare_catalog_sync(context, &call),
        )
        .await?;
    Ok((owner, (**status).clone()))
}

struct CatalogAnnWarm {
    project: Project,
    config: Arc<Config>,
    stages: Arc<HybridStages>,
}

impl CatalogAnnWarm {
    fn schedule(
        self,
        shared: &Arc<Shared>,
        service: &super::job_service::JobService,
        operation: &crate::search::operation::SearchOperation,
        published: ObservedProjectIndex,
    ) -> bool {
        let shutdown = service.shutdown_token();
        if shared.stopping.load(Ordering::SeqCst) || shutdown.is_cancelled() {
            return false;
        }
        let Ok(reservation) = shared.workers.reserve_background(operation) else {
            return false;
        };
        let owner = shared.clone();
        let activity = shared.activity.track();
        let call = operation.clone();
        let project_id = self.project.id;
        drop(tokio::spawn(async move {
            let mut cancellation = request::CancelOnDrop(Some(call.cancellation().clone()));
            let work = async {
                let permit = owner
                    .workers
                    .background_reserved(&call, reservation)
                    .await?;
                let native_call = call.clone();
                let native_activity = activity.clone();
                let worker = request::spawn(Some(permit), Some(activity), move || {
                    native_call.check()?;
                    self.run(&owner, &native_call, native_activity, published)
                });
                request::run(worker, Some(&call)).await
            };
            let result = tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                result = work => result,
            };
            cancellation.0 = None;
            if let Err(error) = result {
                eprintln!("warning: catalog ANN warm for project {project_id}: {error}");
            }
        }));
        true
    }

    fn run(
        &self,
        shared: &Shared,
        operation: &crate::search::operation::SearchOperation,
        activity: Arc<Activity>,
        published: ObservedProjectIndex,
    ) -> Result<()> {
        let Some(embedder) = self.stages.embedder.as_ref() else {
            return Ok(());
        };
        let model = embedder.model_id();
        let project = &self.project;
        let slot = shared.project_slot(project.id);
        let dense = shared
            .dense
            .snapshot_with_activity(activity, self.config.ann_build);
        let admit = || slot.search_preparation_lease(&self.config, &self.stages);
        let mut eligible = false;
        let index = {
            let _revision = admit()?;
            let reader = shared.readers.try_acquire()?;
            ann_warm_binding(reader, project, &shared.cache, model, |db, id, model| {
                let limit = i64::try_from(dense.limits().max_vectors)
                    .unwrap_or(i64::MAX)
                    .saturating_add(1);
                let count = crate::store::repo::count_chunks_up_to(db.connection(), id, limit)?;
                eligible = usize::try_from(count)
                    .is_ok_and(|chunks| dense.limits().ann_build_eligible_for_model(chunks, model));
                Ok(())
            })?
        };
        if index.project_id() != published.project_id
            || index.catalog_generation() != published.epoch.zip(published.generation_id)
            || index.content_revision() != published.content_revision
        {
            return Ok(());
        }
        if eligible {
            operation.check()?;
            dense.warm_bound_with_metadata_admitted(
                BoundDenseRequest {
                    cache_root: &shared.cache,
                    index,
                    model_id: model,
                },
                operation.cancellation(),
                || tools::capture_dense(MetadataSource::Prepared(&shared.readers), project, index),
                admit,
            )?;
        }
        operation.check()?;
        dense.cleanup(project.id)?;
        dense.cleanup_ann_disk()
    }
}

fn capture_catalog_sync(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: &Value,
    epoch: [u8; 16],
    detached: bool,
) -> Result<std::result::Result<catalog_sync::Context, Value>> {
    let project = catalog_job_project(shared, session, args, detached)?;
    registry::ensure_root_present(&project)?;
    let refresh = !detached && args.get("refresh_config").and_then(Value::as_bool) == Some(true);
    let revision = shared.project_slot(project.id);
    let Some(owner) = revision.catalog_sync.try_own() else {
        if refresh {
            return Err(crate::index::async_job::active_sync_conflict());
        }
        let id = revision.catalog_sync.active_id();
        if id > 0 {
            let db = shared.db();
            if let Some(job) = crate::store::repo::sync_job(db.connection(), id)?
                && job.project_id == project.id
                && revision.catalog_sync.active_id() == id
            {
                return Ok(Err(tools::sync_job_value(&job)));
            }
        }
        return Err(Error::Admission {
            message: "project sync is starting or retiring; retry sync".into(),
        });
    };
    let config = if refresh {
        CatalogSyncConfig::Reload
    } else {
        CatalogSyncConfig::Current
    };
    prepare_catalog_project_sync(shared, project, epoch, config, revision, owner).map(Ok)
}

fn capture_catalog_first_index(shared: &Shared, project: Project) -> Result<catalog_sync::Context> {
    let epoch = shared
        .catalog_epoch
        .ok_or_else(|| Error::internal("first index has no registry epoch"))?;
    registry::ensure_root_present(&project)?;
    let revision = shared.project_slot(project.id);
    let owner = revision
        .catalog_sync
        .try_own()
        .ok_or_else(crate::index::async_job::active_sync_conflict)?;
    let mut context = prepare_catalog_project_sync(
        shared,
        project,
        epoch,
        CatalogSyncConfig::Current,
        revision,
        owner,
    )?;
    context.first_generation = true;
    Ok(context)
}

// Captured plans retain reviewed source identity through provider work.
enum CatalogSyncConfig {
    Current,
    Reload,
    Reviewed(Box<crate::config::observe::LiveProjectChange>),
}

fn capture_reviewed_catalog_sync(
    shared: &Shared,
    project: Project,
    change: crate::config::observe::LiveProjectChange,
) -> Result<catalog_sync::Context> {
    let epoch = shared
        .catalog_epoch
        .ok_or_else(|| Error::internal("catalog epoch absent"))?;
    registry::ensure_root_present(&project)?;
    let revision = shared.project_slot(project.id);
    let owner = revision
        .catalog_sync
        .try_own()
        .ok_or_else(crate::index::async_job::active_sync_conflict)?;
    prepare_catalog_project_sync(
        shared,
        project,
        epoch,
        CatalogSyncConfig::Reviewed(Box::new(change)),
        revision,
        owner,
    )
}

fn prepare_catalog_project_sync(
    shared: &Shared,
    project: Project,
    epoch: [u8; 16],
    capture: CatalogSyncConfig,
    revision: ProjectSlot,
    owner: catalog_sync::Owner,
) -> Result<catalog_sync::Context> {
    let refresh = !matches!(capture, CatalogSyncConfig::Current);
    let active = shared.project_config(&project)?;
    let (config, stages, reviewed) = match capture {
        CatalogSyncConfig::Current => {
            let (config, stages) = shared.project_runtime_for_search(&project)?;
            (config, stages, None)
        }
        CatalogSyncConfig::Reload => {
            let (config, stages) = shared.stage_catalog_config(&project, &active)?;
            (config, stages, None)
        }
        CatalogSyncConfig::Reviewed(change) => {
            revision.check_config_revision(&change.active)?;
            {
                let reader = shared.readers.try_acquire()?;
                if !change.reviewed.head.is_selected(reader.connection())? {
                    return Err(Error::Admission {
                        message: "reviewed catalog head changed before preparation; run config plan again".into(),
                    });
                }
            }
            change.reviewed.verify_sources()?;
            let stages = reviewed_catalog_stages(shared, &revision, &change)?;
            (change.config, stages, Some(Box::new(change.reviewed)))
        }
    };
    let reuse_unchanged = shared.website.is_none() && reviewed.is_none();
    let active_model = {
        let state = revision
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state
            .config
            .as_ref()
            .filter(|current| Arc::ptr_eq(current, &active))
            .and(state.search_stages.as_ref().or(state.stages.as_ref()))
            .and_then(|stages| stages.embedder.as_ref())
            .map(|embedder| embedder.model_id().to_owned())
    };
    let source = shared.vector_source_for(project.id, &config);
    let refresh = if refresh {
        source.validate_root(&shared.cache)?;
        Some(catalog_sync::RuntimeUpdate {
            active,
            watchers: Arc::clone(&shared.watchers),
            global_config: shared.global_config.clone(),
            global_path: shared.global_config_path.clone(),
            config_lock_dir: shared.config_lock_dir()?.to_path_buf(),
            watch_runtime: Arc::new(crate::watch::WatchRuntime::new(
                Arc::clone(&config),
                Arc::clone(&stages),
                source.clone(),
            )),
            reviewed,
        })
    } else {
        None
    };
    Ok(catalog_sync::Context {
        build: catalog_sync::BuildContext {
            cache: shared.cache.clone(),
            epoch,
            settings: shared.db_settings.clone(),
            project,
            config,
            embedder: stages.embedder.clone(),
            active_model,
            source,
            pin_ttl: shared.dense.limits().ann_lifetimes.backfill_pin_ttl(),
        },
        stages,
        revision,
        owner,
        refresh,
        reuse_unchanged,
        first_generation: false,
    })
}

fn reviewed_catalog_stages(
    shared: &Shared,
    revision: &ProjectCell,
    change: &crate::config::observe::LiveProjectChange,
) -> Result<Arc<HybridStages>> {
    let retained = if change.config.hybrid_cfg == change.active.hybrid_cfg
        && change.config.providers == change.active.providers
    {
        let state = revision
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.unverified.is_some()
            || !state
                .config
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &change.active))
        {
            return Err(Shared::catalog_config_busy());
        }
        state
            .search_stages
            .as_ref()
            .or(state.stages.as_ref())
            .cloned()
    } else {
        None
    };
    let stages = match retained {
        Some(stages) => stages,
        None => Arc::new(shared.async_stages(&change.config)?),
    };
    catalog_stage_model(&change.config, &stages)?;
    Ok(stages)
}

fn catalog_stage_model<'a>(config: &Config, stages: &'a HybridStages) -> Result<Option<&'a str>> {
    let model = stages.embedder.as_ref().map(|stage| stage.model_id());
    if model != config.embedding_model_key()?.as_deref() {
        return Err(Error::internal(
            "constructed embedding stage does not match the resolved catalog settings",
        ));
    }
    Ok(model)
}

fn configured_catalog_embedding(
    config: &Config,
) -> Result<Option<crate::vector_cache::EmbeddingRecipe>> {
    Ok(config
        .embedding_model_key()?
        .as_deref()
        .map(crate::vector_cache::EmbeddingRecipe::for_model))
}

fn ann_warm_binding(
    reader: crate::store::read_pool::ReadLease<'_>,
    project: &crate::store::models::Project,
    cache: &Path,
    model_id: &str,
    before_metadata: impl FnOnce(&Db, i64, &str) -> Result<()>,
) -> Result<ProjectIndex> {
    if reader.has_generation_catalog() {
        use crate::store::project_index::{require_catalog_embedding, with_catalog_pair};
        return with_catalog_pair(reader, project, |registry, local, index| {
            require_catalog_embedding(local.as_ref(), model_id)?;
            before_metadata(
                local.as_ref().map_or(&*registry, |local| &local.db),
                project.id,
                model_id,
            )?;
            Ok(index)
        });
    }
    lifecycle::with_project_read_lease(cache, project.id, || {
        before_metadata(&reader, project.id, model_id)?;
        tools::snapshot_info(&reader, project, None)
    })
}

fn catalog_requires_index(active: &Config, desired: &Config) -> Result<bool> {
    Ok(
        crate::index::outcome::recipe_id(active) != crate::index::outcome::recipe_id(desired)
            || configured_catalog_embedding(active)? != configured_catalog_embedding(desired)?,
    )
}

fn catalog_job_project(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: &Value,
    detached: bool,
) -> Result<crate::store::models::Project> {
    if let Some(fixed) = &shared.website {
        let bound = session
            .and_then(|id| shared.sessions.get(id))
            .and_then(|s| s.project_id);
        shared.admit_binding(bound)?;
        shared.admit_binding(opt_i64(args, "project_id")?.or(bound))?;
        return Ok(fixed.project.clone());
    }
    if detached {
        let db = shared.db();
        let Some(id) = opt_i64(args, "project_id")? else {
            return bound_project(shared, &db, session);
        };
        return registry::project_by_id(&db, id)?.ok_or_else(|| Error::Project {
            message: format!("unknown project id {id}"),
            instruction: Some("call list_projects".into()),
        });
    }
    let path = match opt_str(args, "path")?.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => session_cwd(shared, session)?.join(path),
        None => session_cwd(shared, session)?,
    };
    let root = registry::project_root_for(&path, &shared.cache)?;
    let mut db = shared.open_work_registry()?;
    if let Some(project) = registry::find_by_root(db.connection(), &root)? {
        return Ok(project);
    }
    shared.config_for_root(&root)?;
    registry::ensure_project(&mut db, &root)
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
    tools::search_docs_with_ranking_settings(
        &tools::SearchContext {
            stages: &stages,
            dense: &shared.dense,
            cache: &shared.cache,
            project: &project,
            window,
            ann_search: config.ann_search,
            payload_budget: &shared.payload_budget,
            operation,
        },
        crate::search::metadata::MetadataSource::Prepared(&shared.readers),
        args,
        || slot.preparation_lease(&config),
        config.as_ref().into(),
    )
}

fn route_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
    operation: Option<crate::search::operation::SearchOperation>,
    activity: Option<Arc<Activity>>,
) -> Result<Value> {
    if name == "search_docs" {
        return route_search(
            shared,
            session_id,
            args,
            operation.ok_or_else(|| Error::internal("search request context missing"))?,
        );
    }
    route_metadata_tool_in(shared, session_id, name, args, activity)
}

#[cfg(test)]
fn route_metadata_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
) -> Result<Value> {
    route_metadata_tool_in(shared, session_id, name, args, None)
}

#[allow(clippy::too_many_lines)] // Exhaustive tool dispatcher.
fn route_metadata_tool_in(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
    activity: Option<Arc<Activity>>,
) -> Result<Value> {
    if name == "server_status" {
        let fixed = shared.website.as_ref().ok_or_else(Shared::profile_denial)?;
        let bound = session_id
            .and_then(|id| shared.sessions.get(id))
            .and_then(|session| session.project_id);
        shared.admit_binding(bound)?;
        if !args.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(Error::Protocol {
                message: "server_status accepts no arguments".to_owned(),
            });
        }
        let config = shared.project_config(&fixed.project)?;
        let status = super::readiness::observe(
            &shared.readers,
            &fixed.project,
            &config,
            shared.restart_notice(),
        )?;
        return serde_json::to_value(status)
            .map_err(|err| Error::internal_with_source("serialize server readiness", err));
    }
    if name == "index_project" && shared.website.is_some() {
        return Err(Shared::profile_denial());
    }
    if matches!(
        name,
        "status"
            | "panel_projects"
            | "list_projects"
            | "list_docs"
            | "read_neighbors"
            | "read_neighbors_at"
    ) {
        if shared.catalog_epoch.is_some() {
            if shared.website.is_none()
                && matches!(name, "status" | "list_projects" | "panel_projects")
            {
                return catalog_project_views(shared, name);
            }
            let reader = shared.readers.try_acquire()?;
            let project = match &shared.website {
                Some(fixed) => fixed.project.clone(),
                None => bound_project(shared, &reader, session_id)?,
            };
            return crate::store::project_index::with_catalog_pair(
                reader,
                &project,
                |registry, local, index| {
                    let metadata = local.as_ref().map_or(&*registry, |local| &local.db);
                    route_index_metadata(
                        shared,
                        session_id,
                        name,
                        args,
                        &registry,
                        metadata,
                        Some(index),
                    )
                },
            );
        }
        let db = shared.db();
        return route_index_metadata(shared, session_id, name, args, &db, &db, None);
    }
    let db = shared.db();
    match name {
        "panel_info" => Ok(json!(crate::ipc::panel::PanelInfo {
            capabilities: {
                let mut capabilities = vec![
                    protocol::PANEL_READ_CAPABILITY.into(),
                    protocol::SEARCH_CONTEXT_CAPABILITY.into(),
                    protocol::LITERAL_QUERY_CAPABILITY.into(),
                ];
                if shared.website.is_some() {
                    capabilities.push(protocol::SERVER_STATUS_CAPABILITY.into());
                }
                if shared.catalog_epoch.is_some() {
                    capabilities.push(protocol::CHUNK_REF_CAPABILITY.into());
                }
                capabilities
            },
            request_timeout_ms: shared.global_config.search.request_timeout_ms,
            max_request_payload_bytes: shared.global_config.search.max_request_payload_bytes,
            max_retained_payload_bytes: shared.global_config.search.max_retained_payload_bytes,
            dedupe_max_candidates: shared.global_config.search.dedupe_max_candidates,
            max_hits: crate::index::tantivy_index::MAX_HITS,
        })),
        "panel_config" => {
            let project = bound_project(shared, &db, session_id)?;
            let config = shared.project_config(&project)?;
            Ok(json!(crate::ipc::panel::ConfigView::from_config(&config)))
        }
        "get_doc" => {
            let project = bound_project(shared, &db, session_id)?;
            let config = shared.project_config(&project)?;
            tools::get_doc(&project, &config, args)
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
            let (config, stages) = shared.project_runtime(&project)?;
            let source = shared.vector_source_for(project.id, &config);
            let (job_id, created) = tools::enqueue_sync_with_retention(
                &job_db,
                &project,
                shared.daemon_settings().max_sync_jobs,
            )?;
            let mut just_indexed = false;
            if created {
                tools::run_sync_job_in_with_vectors(
                    &mut job_db,
                    &shared.cache,
                    job_id,
                    &project,
                    &config,
                    &shared.dense.limits(),
                    &stages,
                    &source,
                )?;
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
            if shared.website.is_some() {
                let bound = bound_project(shared, &db, session_id)?;
                shared.admit_binding(opt_i64(args, "project_id")?.or(Some(bound.id)))?;
            }
            let project = match opt_i64(args, "project_id")? {
                Some(id) => registry::project_by_id(&db, id)?.ok_or_else(|| Error::Project {
                    message: format!("unknown project id {id}"),
                    instruction: Some("call list_projects".to_owned()),
                })?,
                None => bound_project(shared, &db, session_id)?,
            };
            shared.admit_project(&project)?;
            registry::ensure_root_present(&project)?;
            // Provider construction may load models; release the shared DB
            // guard before preparing the coherent runtime snapshot.
            drop(db);
            let (config, stages) = shared.project_runtime(&project)?;
            let source = shared.vector_source_for(project.id, &config);
            let db = shared.db();
            let sync_permit = if shared.website.is_some() {
                if shared.stopping.load(Ordering::SeqCst) {
                    return Err(Error::Admission {
                        message: "daemon is shutting down".to_owned(),
                    });
                }
                if let Ok(permit) = Arc::clone(&shared.sync_slot).try_acquire_owned() {
                    Some(permit)
                } else {
                    if let Some(job) =
                        crate::store::repo::active_sync_job(db.connection(), project.id)?
                    {
                        return current_job(&db, job.id);
                    }
                    return Err(Error::Admission {
                        message: "fixed-project sync is still finishing".to_owned(),
                    });
                }
            } else {
                None
            };
            let (job_id, created) = tools::enqueue_sync_with_retention(
                &db,
                &project,
                shared.daemon_settings().max_sync_jobs,
            )?;
            if created {
                spawn_sync_job(
                    Arc::clone(shared),
                    job_id,
                    project.id,
                    config,
                    stages,
                    source,
                    (sync_permit, activity),
                );
            }
            current_job(&db, job_id)
        }
        "sync_status" => {
            if shared.website.is_some() {
                let id = opt_i64(args, "job_id")?.ok_or_else(|| Error::Protocol {
                    message: "sync_status: missing job_id".to_owned(),
                })?;
                let job = crate::store::repo::sync_job(db.connection(), id)?
                    .ok_or_else(Shared::profile_denial)?;
                shared.admit_binding(Some(job.project_id))?;
            }
            tools::sync_status(&db, args)
        }
        other => Err(Error::internal(format!(
            "tool {other:?} is not implemented yet"
        ))),
    }
}

struct CatalogDiagnostics {
    ann: (AnnState, Option<&'static str>),
    resident: Option<(usize, u64)>,
    vectors: Option<CatalogCoverage>,
}

struct CatalogCoverage {
    hashes: hybrid_jobs::CoverageHashes,
    source: crate::vector_cache::VectorCacheSource,
    model: CatalogModel,
}

struct CatalogModel {
    key: String,
    ranking: Ranking,
    // Keep the charge until the model key and the owned coverage are dropped.
    permit: crate::search::budget::PayloadPermit,
}

fn capture_catalog_model(
    slot: &ProjectCell,
    config: &Config,
    budget: &crate::search::budget::PayloadBudget,
) -> Result<Option<CatalogModel>> {
    let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(stages) = state.search_stages.as_ref().or(state.stages.as_ref()) {
        let Some(embedder) = stages.embedder.as_ref() else {
            return Ok(None);
        };
        let model = embedder.model_id();
        let permit = budget.reserve(2 * model.len() as u64)?;
        return Ok(Some(CatalogModel {
            key: model.to_owned(),
            ranking: stages.ranking,
            permit,
        }));
    }
    let Some(model) = config
        .hybrid_cfg
        .embedding
        .as_ref()
        .filter(|_| config.hybrid_cfg.enabled)
    else {
        return Ok(None);
    };
    // The canonical keys include the provider, model, numeric fields and
    // optional local revision. Allow growth of the formatted key buffer and
    // one SQLite binding without constructing the configured provider.
    let key_bytes = model.provider.len() + model.model.len() + 64;
    let permit = budget.reserve(3 * key_bytes as u64)?;
    Ok(Some(CatalogModel {
        key: config.providers.embedding_model_id(model)?,
        ranking: config.hybrid_cfg.ranking,
        permit,
    }))
}

impl CatalogDiagnostics {
    fn without_vectors(state: AnnState, reason: &'static str) -> Self {
        Self {
            ann: (state, Some(reason)),
            resident: None,
            vectors: None,
        }
    }
}

fn catalog_project_summary(
    shared: &Shared,
    project: &Project,
    diagnostics: bool,
) -> Result<Option<CatalogProjectInfo<CatalogDiagnostics>>> {
    let config = diagnostics
        .then(|| shared.project_config(project).ok())
        .flatten();
    let slot = config.as_ref().map(|_| shared.project_slot(project.id));
    let runtime = slot.as_ref().zip(config.as_ref());
    let preparation = runtime.and_then(|(slot, config)| slot.preparation_lease(config).ok());
    let reader = shared.readers.try_acquire()?;
    catalog_project_info(&reader, project, |db, local, index, counts| {
        let Some((_, (slot, config))) = preparation.as_ref().zip(runtime) else {
            return CatalogDiagnostics::without_vectors(
                AnnState::Unknown,
                "project runtime unavailable",
            );
        };
        let mut model = match capture_catalog_model(slot, config, &shared.payload_budget) {
            Ok(Some(model)) => model,
            Ok(None) => {
                return CatalogDiagnostics::without_vectors(AnnState::Disabled, "hybrid off");
            }
            Err(_) => {
                return CatalogDiagnostics::without_vectors(
                    AnnState::Unknown,
                    "diagnostics payload budget exhausted",
                );
            }
        };
        if require_catalog_embedding(local, &model.key).is_err() {
            return CatalogDiagnostics::without_vectors(
                AnnState::Stale,
                "published embedding recipe does not match",
            );
        }
        let (ann, resident) = if model.ranking == Ranking::Rrf {
            let diagnostics = shared
                .dense
                .ann_diagnostics_for(index, &model.key, config.ann_build);
            let ann = tools::classify_ann_outcome(&shared.dense, diagnostics.outcome, true);
            (ann, diagnostics.resident)
        } else {
            ((AnnState::Disabled, Some("lexical ranking")), None)
        };
        let vectors =
            hybrid_jobs::CoverageHashes::capture(db, project.id, counts.chunks, &mut model.permit)
                .ok()
                .map(|hashes| CatalogCoverage {
                    model,
                    source: shared.vector_source_for(project.id, config),
                    hashes,
                });
        CatalogDiagnostics {
            ann,
            resident,
            vectors,
        }
    })
}

fn catalog_project_views(shared: &Shared, name: &str) -> Result<Value> {
    use crate::store::{models::ProjectStatus, repo::ProjectCounts};

    let empty = ProjectCounts { docs: 0, chunks: 0 };
    let mut entries = Vec::new();
    let projects = {
        let reader = shared.readers.try_acquire()?;
        registry::list_projects(&reader)?
    };
    for mut project in projects {
        let selected = catalog_project_summary(shared, &project, name != "list_projects");
        let (project, counts, revision, details, unavailable) = match selected {
            Ok(Some(CatalogProjectInfo {
                project,
                counts,
                index,
                details,
            })) => (
                project,
                counts,
                Some(index.content_revision()),
                Some(details),
                None,
            ),
            Ok(None) => {
                project.status = ProjectStatus::NotIndexed;
                (project, empty, None, None, None)
            }
            Err(error) if lifecycle::is_lease_conflict(&error) => {
                project.status = ProjectStatus::Indexing;
                (project, empty, None, None, None)
            }
            Err(error @ Error::Project { .. }) => {
                project.status = ProjectStatus::Error;
                (project, empty, None, None, Some(error.to_string()))
            }
            Err(error) => return Err(error),
        };
        let details = details.unwrap_or_else(|| {
            CatalogDiagnostics::without_vectors(AnnState::Unknown, "published index unavailable")
        });
        let coverage = details.vectors.and_then(|vectors| {
            vectors
                .hashes
                .count(&vectors.source, &shared.cache, &vectors.model.key)
                .ok()
        });
        let watched = shared.watchers.watching(project.id);
        let mut entry = if name == "panel_projects" {
            json!(crate::ipc::panel::ProjectView {
                watched,
                cached_vectors: coverage.map(|coverage| coverage.cached),
                vector_chunks: coverage.map(|coverage| coverage.total),
                ann_state: Some(details.ann.0.as_str().to_owned()),
                id: project.id,
                name: project.name.clone(),
                root: project.canonical_root.to_string_lossy().into_owned(),
                status: project.status.as_str().into(),
                docs: counts.docs,
                chunks: counts.chunks,
            })
        } else {
            let reader = shared.readers.try_acquire()?;
            tools::project_entry_with_counts(
                &reader,
                &project,
                counts,
                (name == "status").then_some(watched),
                name == "status",
            )?
        };
        if name == "status" {
            entry["root_state"] = json!(registry::root_state(&project.canonical_root).as_str());
            entry["index_generation"] = json!(revision.unwrap_or(0));
            entry["embed_coverage"] = coverage.map_or(
                Value::Null,
                |coverage| json!({"cached": coverage.cached, "total": coverage.total}),
            );
            entry["ann"] = tools::ann_state_json(details.ann, details.resident);
        }
        if let Some(error) = unavailable
            && name != "panel_projects"
        {
            entry["last_error"] = json!(error);
        }
        entries.push(entry);
    }
    if name != "status" {
        return Ok(json!(entries));
    }
    let reader = shared.readers.try_acquire()?;
    let status = tools::status_document(
        &reader,
        &entries,
        &shared.sessions.stats(),
        shared.watchers.count(),
        shared.restart_notice().as_deref(),
        Some(&shared.dense),
    )?;
    extend_runtime_status(shared, status)
}

// The admitted registry supplies installation state (schema and sync history).
// Indexed project data always comes from one metadata connection.
fn route_index_metadata(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
    registry_db: &Db,
    metadata: &Db,
    index: Option<ProjectIndex>,
) -> Result<Value> {
    match name {
        "status" => runtime_status(shared, registry_db, metadata),
        "panel_projects" => {
            let projects = shared
                .visible_projects(metadata)?
                .into_iter()
                .map(|p| {
                    let count = crate::store::repo::project_counts(metadata.connection(), p.id)?;
                    let config = if shared.website.is_none() {
                        shared.project_config(&p).ok()
                    } else {
                        None
                    };
                    let coverage = config.as_ref().and_then(|config| {
                        hybrid_jobs::configured_coverage(
                            metadata,
                            config,
                            p.id,
                            &shared.vector_source_for(p.id, config),
                        )
                        .ok()
                        .flatten()
                    });
                    Ok(crate::ipc::panel::ProjectView {
                        watched: shared.watchers.watching(p.id),
                        cached_vectors: coverage.map(|c| c.cached),
                        vector_chunks: coverage.map(|c| c.total),
                        ann_state: config.as_ref().map(|_| {
                            tools::classified_ann_state(&shared.dense, p.id, coverage.is_some())
                                .0
                                .as_str()
                                .to_owned()
                        }),
                        id: p.id,
                        name: p.name,
                        root: p.canonical_root.to_string_lossy().into_owned(),
                        status: p.status.as_str().into(),
                        docs: count.docs,
                        chunks: count.chunks,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(json!(projects))
        }
        "list_projects" => {
            tools::list_projects_from(registry_db, metadata, &shared.visible_projects(metadata)?)
        }
        "list_docs" => {
            let project = bound_project(shared, metadata, session_id)?;
            tools::list_docs(metadata, &project, args)
        }
        "read_neighbors" => {
            let project = bound_project(shared, metadata, session_id)?;
            tools::read_neighbors(metadata, &project, args)
        }
        "read_neighbors_at" => {
            let index = index.ok_or_else(crate::search::chunk_ref::unavailable)?;
            let project = bound_project(shared, metadata, session_id)?;
            tools::read_neighbors_at(metadata, &project, index, args, None)
        }
        _ => Err(Error::internal("unexpected index metadata tool")),
    }
}

fn runtime_status(shared: &Shared, registry_db: &Db, metadata: &Db) -> Result<Value> {
    let sessions = shared.sessions.stats();
    let notice = shared.restart_notice();
    let projects = shared.visible_projects(metadata)?;
    let status = tools::status_for_projects(
        registry_db,
        metadata,
        &projects,
        &sessions,
        shared.watchers.count(),
        notice.as_deref(),
        &|id| shared.watchers.watching(id),
        &|project| {
            if shared.website.is_some() {
                return None;
            }
            let Ok(config) = shared.project_config(project) else {
                return None;
            };
            hybrid_jobs::configured_coverage(
                metadata,
                &config,
                project.id,
                &shared.vector_source_for(project.id, &config),
            )
            .ok()
            .flatten()
        },
        if shared.website.is_none() {
            Some(&shared.dense)
        } else {
            None
        },
    )?;
    extend_runtime_status(shared, status)
}

fn extend_runtime_status(shared: &Shared, mut status: Value) -> Result<Value> {
    let mut execution = shared.workers.snapshot();
    if let (Some(target), Some(payload)) = (
        execution.as_object_mut(),
        shared.payload_budget.snapshot().as_object(),
    ) {
        target.extend(payload.clone());
    }
    #[cfg(feature = "hybrid-remote")]
    if let Some(target) = execution.as_object_mut() {
        let remote = serde_json::to_value(shared.remote.snapshot())
            .map_err(|error| Error::internal_with_source("serialize remote execution", error))?;
        if let Some(remote) = remote.as_object() {
            target.extend(remote.clone());
        }
    }
    status["remote_execution"] = execution;
    Ok(status)
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

fn spawn_sync_job(
    shared: Arc<Shared>,
    job_id: i64,
    project_id: i64,
    config: Arc<Config>,
    stages: Arc<HybridStages>,
    source: crate::vector_cache::VectorCacheSource,
    ownership: (
        Option<tokio::sync::OwnedSemaphorePermit>,
        Option<Arc<Activity>>,
    ),
) {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => {
            request::spawn(ownership.0, ownership.1, move || {
                match tools::execute_sync_job_with_vectors(
                    &shared.cache,
                    job_id,
                    project_id,
                    &config,
                    &shared.db_settings,
                    &shared.dense.limits(),
                    &stages,
                    &source,
                ) {
                    Ok(project) => {
                        if let Err(err) = shared.ensure_watcher(&project) {
                            eprintln!("warning: cannot watch project {}: {err}", project.id);
                        }
                        let _ = shared.refresh_ann_after_sync(&project);
                    }
                    Err(err) => eprintln!("warning: sync job {job_id} failed: {err}"),
                }
                Ok(())
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
        .ok_or_else(|| {
            if shared.website.is_some() {
                Shared::profile_denial()
            } else {
                Error::Project {
                    message: "session is not bound to a project".to_owned(),
                    instruction: Some("register a session from your project directory".to_owned()),
                }
            }
        })?;
    shared.admit_binding(Some(project_id))?;
    let project = registry::project_by_id(db, project_id)?.ok_or_else(|| Error::Project {
        message: format!("project {project_id} disappeared from the registry"),
        instruction: None,
    })?;
    shared.admit_project(&project)?;
    Ok(project)
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

    #[test]
    fn invalid_remote_policy_is_fatal_before_legacy_shared_effects() {
        for policy in [
            "interactive_active=0",
            "attempt_timeout_ms=0",
            "unknown_remote_policy=1",
            // Valid TOML number, outside the complete policy's u64 domain;
            // owner decoding must fail rather than legacy fallback.
            "attempt_timeout_ms=18446744073709551616.0",
            "retry_attempts=0",
            "interactive_active='bad'",
            "interactive_active=9223372036854775807",
            "retry_attempts=4294967296",
            "retry_attempts=1\n[daemon]\nmax_blocking_threads=1",
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::write(
                root.path().join("config.toml"),
                format!("max_file_size='unrelated legacy error'\n[remote]\n{policy}\n"),
            )
            .unwrap();
            let cache = root.path().join("cache");
            let result = LaunchSnapshot::capture_from(root.path())
                .legacy(DaemonPatch::default())
                .and_then(|launch| {
                    Shared::open_with_launch_config(
                        &cache,
                        &crate::store::settings::standalone().unwrap(),
                        &crate::store::settings::standalone_read_pool().unwrap(),
                        crate::hybrid::dense_settings::standalone()
                            .unwrap()
                            .settings(),
                        launch,
                    )
                });
            assert!(result.is_err(), "recognized remote policy must fail closed");
            assert!(
                !cache.exists(),
                "remote failure must precede DB/state effects"
            );
        }
    }

    #[test]
    fn invalid_file_daemon_policy_cannot_fall_back_during_legacy_shared_open() {
        let mut violations = Vec::new();
        for policy in ["idle_grace_ms=0", "max_events=0"] {
            let root = tempfile::tempdir().unwrap();
            fs::write(
                root.path().join("config.toml"),
                format!(
                    "[daemon]\nprofile={{mode='website',project_root={}}}\n{policy}",
                    toml::Value::String(root.path().to_string_lossy().into_owned())
                ),
            )
            .unwrap();
            let cache = root.path().join("cache");
            let result = LaunchSnapshot::capture_from(root.path())
                .legacy(DaemonPatch::default())
                .and_then(|launch| {
                    Shared::open_with_launch_config(
                        &cache,
                        &crate::store::settings::standalone().unwrap(),
                        &crate::store::settings::standalone_read_pool().unwrap(),
                        crate::hybrid::dense_settings::standalone()
                            .unwrap()
                            .settings(),
                        launch,
                    )
                });
            if result.is_ok() || cache.exists() {
                violations.push(format!(
                    "{policy}: opened={}, cache={}",
                    result.is_ok(),
                    cache.exists()
                ));
            }
        }
        assert_eq!(violations, Vec::<String>::new());
    }

    #[test]
    fn legacy_shared_source_error_preserves_defaults_and_restart_diagnostic() {
        for text in [
            "not=[valid",
            "max_file_size=0\n[daemon]\nprofile={mode='shared'}\nmax_sessions=1",
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::write(root.path().join("config.toml"), text).unwrap();
            let cache = root.path().join("cache");
            let launch = LaunchSnapshot::capture_from(root.path())
                .legacy(DaemonPatch::default())
                .unwrap();
            assert!(launch.error.is_some());
            let shared = Shared::open_with_launch_config(
                &cache,
                &crate::store::settings::standalone().unwrap(),
                &crate::store::settings::standalone_read_pool().unwrap(),
                crate::hybrid::dense_settings::standalone()
                    .unwrap()
                    .settings(),
                launch,
            )
            .unwrap();
            assert_eq!(
                shared.daemon_settings().max_sessions,
                crate::daemon::settings::DaemonSpec::DEFAULTS.max_sessions
            );
            assert!(shared.restart_notice().unwrap().contains("invalid"));
            assert!(
                shared.config_for_root(root.path()).is_err(),
                "fallback must retain the old global-error guard"
            );
        }
    }

    #[tokio::test]
    async fn drain_poll_cannot_overshoot_its_original_deadline() {
        use std::cell::{Cell, RefCell};
        let policy =
            crate::config::resolve::resolve_component::<crate::daemon::settings::DaemonSpec>(
                &crate::config::bootstrap::DefaultsContext::standalone(),
                [(
                    crate::config::schema::SourceId::Cli,
                    DaemonPatch {
                        reply_drain_ms: Some(1),
                        drain_poll_ms: Some(60000),
                        ..DaemonPatch::default()
                    },
                )],
            )
            .unwrap();
        let stuck = AtomicUsize::new(2);
        let base = tokio::time::Instant::now();
        // Work after StopDaemon admission has already spent most of the budget.
        let elapsed = Cell::new(Duration::from_micros(900));
        let sleeps = RefCell::new(Vec::new());
        let result = wait_for_drain_with(
            &stuck,
            base + Duration::from_millis(policy.settings().reply_drain_ms),
            Duration::from_millis(policy.settings().drain_poll_ms),
            || base + elapsed.get(),
            |wake_at| {
                sleeps.borrow_mut().push(wake_at);
                elapsed.set(wake_at.duration_since(base));
                std::future::ready(())
            },
        )
        .await;
        assert!(!result);
        assert_eq!(elapsed.get(), Duration::from_millis(1));
        assert_eq!(*sleeps.borrow(), [base + Duration::from_millis(1)]);
    }

    #[tokio::test]
    async fn drain_timeout_is_bounded() {
        let stuck = AtomicUsize::new(2);
        let started = tokio::time::Instant::now();
        assert!(
            !wait_for_drain(
                &stuck,
                started + Duration::from_millis(50),
                Duration::from_millis(crate::daemon::settings::DaemonSpec::DEFAULTS.drain_poll_ms)
            )
            .await
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "drain must stop at the timeout"
        );

        let idle = AtomicUsize::new(1);
        assert!(
            wait_for_drain(
                &idle,
                tokio::time::Instant::now() + Duration::from_millis(50),
                Duration::from_millis(crate::daemon::settings::DaemonSpec::DEFAULTS.drain_poll_ms)
            )
            .await
        );
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

    const REFRESH_WATCHDOG: Duration = Duration::from_mins(5);

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
            .acquire(Instant::now() + Duration::from_mins(5))
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
    fn changing_only_ann_traversal_keeps_the_daemons_resident_dense_state() {
        let fixture = RefreshFixture::new(true, Some("rrf"), crate::hybrid::ann::ANN_MIN_VECTORS);
        let current = || {
            fixture
                .shared
                .open_project_config_with(&fixture.project, |_| Ok(HybridStages::default()))
                .unwrap()
        };
        let original = current();
        finish_refresh(fixture.shared.refresh_ann_after_sync(&fixture.project));
        let resident = || {
            fixture
                .shared
                .dense
                .get(&fixture.db, fixture.project.id, FakeEmbedder.model_id())
                .unwrap()
        };
        let deadline = Instant::now() + REFRESH_WATCHDOG;
        let graph = loop {
            if let DenseRows::Ann(graph) = resident() {
                break graph;
            }
            assert!(
                Instant::now() < deadline,
                "refresh did not publish the ANN graph"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let path = fixture.root.path().join(".docsbase.toml");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("\n[ann.search]\nef_base=16\nef_max=32\n");
        fs::write(&path, text).unwrap();
        let refreshed = current();
        assert!(original.ann_search.is_none());
        assert_eq!(refreshed.ann_search.unwrap().ef_max(), 32);
        assert!(matches!(resident(), DenseRows::Ann(reused) if Arc::ptr_eq(&graph, &reused)));
    }

    #[test]
    fn sync_refresh_builds_the_captured_ann_recipe_from_existing_vectors() {
        use crate::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore, chunk_set_digest};

        let mut fixture = RefreshFixture::new(true, Some("rrf"), 3);
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        shared.dense = DenseCache::with_policy_limits(
            AnnPolicy::BuildOrLoad,
            fixture.cache.path(),
            DenseCacheLimits {
                min_vectors: 2,
                max_ann_disk_bytes: 1,
                ..DenseCacheLimits::default()
            },
        );
        let path = fixture.root.path().join(".docsbase.toml");
        let original = fs::read_to_string(&path).unwrap();
        let project_id = fixture.project.id;
        let db = fixture.db.connection();
        let chunks = crate::store::repo::chunk_hashes(db, project_id).unwrap();
        let generation = crate::store::repo::index_generation(db, project_id).unwrap();
        let key = AnnKey {
            project_id,
            generation,
            model_id: FakeEmbedder.model_id().to_owned(),
            dim: 3,
            chunk_count: chunks.len(),
            chunk_set_digest: chunk_set_digest(&chunks),
            format_version: ANN_FORMAT_VERSION,
        };
        let store = AnnStore::new(fixture.cache.path().to_path_buf());
        let mut previous: Option<PathBuf> = None;
        for (m, ef_construction) in [(12, 64), (16, 98)] {
            fs::write(
                &path,
                format!("{original}\n[ann.build]\nm={m}\nef_construction={ef_construction}\n"),
            )
            .unwrap();
            let config = fixture
                .shared
                .open_project_config_with(&fixture.project, |_| Ok(HybridStages::default()))
                .unwrap();
            finish_refresh(fixture.shared.refresh_ann_after_sync(&fixture.project));
            fixture.shared.dense.join_job_builds_for_test().unwrap();
            let slot = store
                .load_with_settings(&key, config.ann_build)
                .unwrap()
                .expect("sync warm must build the captured recipe");
            assert_eq!(slot.meta.m, m);
            assert_eq!(slot.meta.ef_construction, ef_construction);
            let cold = DenseCache::with_policy_limits(
                AnnPolicy::LoadOnly,
                fixture.cache.path(),
                fixture.shared.dense.limits(),
            )
            .snapshot_with_build_settings(config.ann_build);
            assert!(matches!(
                cold.get(&fixture.db, project_id, FakeEmbedder.model_id()).unwrap(),
                DenseRows::Ann(graph) if graph.build_settings() == config.ann_build
            ));
            if let Some(previous) = previous.replace(slot.dir) {
                assert!(
                    !previous.exists(),
                    "discard the old recipe under disk pressure"
                );
            }
        }
        assert!(store.load(&key).unwrap().is_none());
        assert_eq!(
            crate::store::repo::index_generation(db, project_id).unwrap(),
            generation
        );
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

#[cfg(all(test, windows))]
#[path = "windows_transport_tests.rs"]
mod windows_transport_tests;
