//! One explicit owner for an operational direct command, outside its work future.
use crate::config::Config;
use crate::daemon::{
    job_service::{JobRoot, JobService},
    request::{self, Activity, ActivitySource},
    worker_admission::WorkerAdmission,
};
use crate::error::{Error, Result};
use crate::hybrid::work::{Cancellation, RemoteCall};
use crate::search::{budget::PayloadBudget, operation::SearchOperation};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub(super) struct Context {
    pub(super) global: Arc<Config>,
    pub(super) cache: std::path::PathBuf,
    pub(super) workers: Arc<WorkerAdmission>,
    pub(super) activity: ActivitySource,
    pub(super) jobs: JobService,
    pub(super) budget: PayloadBudget,
    pub(super) search:
        Arc<std::sync::Mutex<Option<Arc<crate::daemon::server::search_async::SearchSession>>>>,
    #[cfg(feature = "hybrid-remote")]
    remote: Arc<crate::hybrid::remote::RemoteExecution>,
}
impl Context {
    #[cfg_attr(
        not(feature = "hybrid-remote"),
        expect(
            clippy::unused_self,
            reason = "Only the remote factory needs this command execution owner"
        )
    )]
    pub(super) fn stages(&self, config: &Config) -> Result<crate::hybrid::HybridStages> {
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
    pub(super) async fn control<T: Send + 'static>(
        &self,
        operation: &SearchOperation,
        activity: &Arc<Activity>,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = RemoteCall::new(operation.clone())
            .wait(&Cancellation::new(), self.workers.control())
            .await?;
        let call = operation.clone();
        let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
            call.check()?;
            let _cpu = call.cpu_timer();
            work()
        });
        request::run(worker, Some(operation)).await
    }
}

pub(super) fn run<T, F: Future<Output = Result<T>>>(
    global: Config,
    work: impl FnOnce(Context) -> F,
) -> Result<T> {
    let run = move || {
        global.validate()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(global.daemon.max_blocking_threads)
            .enable_all()
            .build()
            .map_err(|e| Error::internal_with_source("build direct command runtime", e))?;
        let workers = Arc::new(WorkerAdmission::with_policies(
            &global.daemon,
            &global.remote,
        )?);
        let (activity, _events) = ActivitySource::channel(global.daemon.max_events);
        #[cfg(feature = "hybrid-remote")]
        let remote = Arc::new(crate::hybrid::remote::RemoteExecution::real_with_owners(
            &global.remote,
            workers.clone(),
            activity.clone(),
        )?);
        let mut root = JobRoot::new(&global.daemon);
        let context = Context {
            cache: crate::config::paths::cache_dir()?,
            budget: PayloadBudget::new(global.search.clone())?,
            jobs: root.service(),
            global: Arc::new(global),
            workers,
            activity,
            search: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(feature = "hybrid-remote")]
            remote,
        };
        // Keep execution/root/native receivers outside every dropped/panicking
        // work future. JobRoot first retires issued terminal/native ownership.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            root.run(&runtime, work(context.clone()))
        }));
        #[cfg(feature = "hybrid-remote")]
        context.remote.shutdown();
        context.workers.shutdown();
        runtime.block_on(
            context
                .activity
                .drain(Duration::from_millis(context.global.daemon.drain_poll_ms)),
        );
        // One concrete command component, installed on admitted CPU before
        // delivery. Taking it does not hold a lock across the physical wait.
        let search = context
            .search
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(search) = search {
            runtime.block_on(async {
                while Arc::strong_count(&search) > 1 {
                    tokio::time::sleep(Duration::from_millis(context.global.daemon.drain_poll_ms))
                        .await;
                }
            });
            drop(search); // readonly DB/model/ANN owners, outside block_on
        }
        context.activity.retire_bridge();
        match result {
            Ok(result) => result,
            Err(_) => Err(Error::internal("direct command work unwound")),
        }
    };
    match tokio::runtime::Handle::try_current() {
        Err(_)=>run(),
        Ok(handle) if handle.runtime_flavor()==tokio::runtime::RuntimeFlavor::MultiThread=>tokio::task::block_in_place(run),
        Ok(_)=>Err(Error::Admission{message:"synchronous direct command cannot enter a current-thread Tokio runtime; use the async API".into()}),
    }
}
