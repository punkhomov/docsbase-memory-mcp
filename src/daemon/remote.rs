//! Explicit command runtime ownership and bounded, physically charged DNS.

#[cfg(any(feature = "hybrid-remote", test))]
use std::sync::Arc;
use std::time::Duration;

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::error::Error;
#[cfg(any(feature = "hybrid-remote", test))]
use crate::error::Result;

use super::request::{self, ActivitySource};
use super::settings::{DaemonSettings, DaemonSpec};
use super::worker_admission::WorkerAdmission;

/// Handle-only compatibility dependency. The runtime's root explicitly retires it.
pub(crate) struct SyncBridge {
    handle: tokio::runtime::Handle,
    retired: crate::hybrid::work::Cancellation,
}

impl SyncBridge {
    pub(crate) fn new(handle: tokio::runtime::Handle) -> Self {
        Self {
            handle,
            retired: crate::hybrid::work::Cancellation::new(),
        }
    }
    pub(crate) fn retire(&self) {
        self.retired.cancel();
    }
    pub(crate) fn block_on<T>(
        &self,
        work: impl std::future::Future<Output = crate::error::Result<T>>,
    ) -> crate::error::Result<T> {
        self.retired.check()?;
        self.handle.block_on(work)
    }
}

/// Execute one legacy synchronous command on an explicitly owned runtime.
/// Construction and destruction both remain outside its work future.
///
/// # Errors
/// Invalid policy, current-thread nesting, runtime/worker failure or command error.
pub fn with_sync_command<T, E>(
    policy: &DaemonSettings,
    work: impl FnOnce() -> std::result::Result<T, E> + Send + 'static,
) -> std::result::Result<T, E>
where
    T: Send + 'static,
    E: From<Error> + Send + 'static,
{
    DaemonSpec::validate(policy, &Capabilities::current()).map_err(E::from)?;
    let run = || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(policy.max_blocking_threads)
            .enable_all()
            .build()
            .map_err(|error| {
                E::from(Error::internal_with_source("build command runtime", error))
            })?;
        let (activity, _events) =
            ActivitySource::with_runtime(policy.max_events, runtime.handle().clone());
        let remote = crate::hybrid::remote_settings::standalone().map_err(E::from)?;
        let workers = WorkerAdmission::with_policies(policy, remote.settings()).map_err(E::from)?;
        // The owner and runtime live outside block_on, including work unwind.
        let owned = activity.track();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                let permit = workers.control().await.map_err(E::from)?;
                let bridge = activity.bridge.clone();
                let worker = request::spawn(Some(permit), Some(owned), move || {
                    crate::hybrid::blocking::with_bridge(bridge, || Ok(work()))
                });
                worker
                    .await
                    .map_err(|error| {
                        E::from(Error::internal_with_source("command worker join", error))
                    })?
                    .map_err(E::from)?
            })
        }));
        runtime.block_on(activity.drain(Duration::from_millis(policy.poll_interval_ms)));
        workers.shutdown();
        activity.retire_bridge();
        match result {
            Ok(result) => result,
            Err(_) => Err(E::from(Error::internal("command work unwound"))),
        }
        // Runtime is destroyed here, on the synchronous/block_in_place stack.
    };
    match tokio::runtime::Handle::try_current() {
        Err(_) => run(),
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(run)
        }
        Ok(_) => Err(E::from(Error::Admission {
            message:
                "synchronous command cannot enter a current-thread Tokio runtime; use the async API"
                    .to_owned(),
        })),
    }
}

#[cfg(feature = "hybrid-remote")]
type Lookup = dyn Fn(String) -> std::io::Result<Vec<std::net::SocketAddr>> + Send + Sync;

/// Reqwest resolver using the application's bounded helper lane and Activity owner.
#[cfg(feature = "hybrid-remote")]
pub struct BoundedResolver {
    workers: Arc<WorkerAdmission>,
    activity: ActivitySource,
    queue_timeout: Duration,
    lookup: Arc<Lookup>,
}

#[cfg(feature = "hybrid-remote")]
impl BoundedResolver {
    pub(crate) fn with_owners(
        settings: &crate::hybrid::remote_settings::RemoteSettings,
        workers: Arc<WorkerAdmission>,
        activity: ActivitySource,
    ) -> Result<Self> {
        crate::hybrid::remote_settings::RemoteSpec::validate(settings, &Capabilities::current())?;
        Ok(Self {
            workers,
            activity,
            queue_timeout: Duration::from_millis(settings.attempt_timeout_ms),
            lookup: Arc::new(|name| {
                use std::net::ToSocketAddrs;
                (name.as_str(), 0).to_socket_addrs().map(Iterator::collect)
            }),
        })
    }

    /// Standalone compatibility composition, with complete explicit owner policies.
    ///
    /// # Errors
    /// Invalid policy.
    pub fn with_settings(
        settings: &crate::hybrid::remote_settings::RemoteSettings,
        daemon: &DaemonSettings,
    ) -> Result<Self> {
        let workers = Arc::new(WorkerAdmission::with_policies(daemon, settings)?);
        let (activity, _events) = ActivitySource::channel(daemon.max_events);
        Self::with_owners(settings, workers, activity)
    }
}

#[cfg(feature = "hybrid-remote")]
impl reqwest::dns::Resolve for BoundedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let workers = Arc::clone(&self.workers);
        let activity = self.activity.clone();
        let timeout = self.queue_timeout;
        let lookup = Arc::clone(&self.lookup);
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let failed = |message: &'static str| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::other(message))
            };
            let deadline = std::time::Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| failed("DNS helper deadline overflow"))?;
            let permit = workers
                .helper(deadline)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            let worker = request::spawn(Some(permit), Some(activity.track()), move || {
                lookup(name)
                    .map_err(|error| Error::internal_with_source("provider DNS lookup", error))
            });
            let addresses = worker
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_runtime_destroys_on_its_synchronous_owner() {
        let policy = crate::daemon::settings::standalone().unwrap();
        let result: Result<_> = with_sync_command(policy.settings(), || Ok(7));
        assert_eq!(result.unwrap(), 7);
    }

    #[cfg(feature = "hybrid-remote")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retired_compatibility_transport_rejects_work_and_drops_safely() {
        let policy = crate::daemon::settings::standalone().unwrap();
        let transport: Result<_> = with_sync_command(policy.settings(), || {
            crate::hybrid::openrouter::ReqwestTransport::new()
        });
        let transport = Arc::new(transport.unwrap());
        let result = crate::hybrid::blocking::enter(|| {
            crate::hybrid::wire::Transport::post_json(
                &*transport,
                "https://openrouter.ai/api/v1/embeddings",
                &[],
                "{}",
            )
        });
        assert!(
            result.is_err(),
            "retired bridge must reject before polling HTTP"
        );
        tokio::spawn(async move {
            drop(transport);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn current_thread_command_rejection_precedes_work_construction() {
        let policy = crate::daemon::settings::standalone().unwrap();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child = Arc::clone(&called);
        let result: Result<()> = with_sync_command(policy.settings(), move || {
            child.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multi_thread_command_error_and_unwind_destroy_the_runtime_safely() {
        let policy = crate::daemon::settings::standalone().unwrap();
        let result: Result<()> = with_sync_command(policy.settings(), || {
            Err(Error::internal("controlled error"))
        });
        assert!(result.is_err());
        let result: Result<()> =
            with_sync_command(policy.settings(), || panic!("controlled worker unwind"));
        assert!(result.is_err());
    }

    #[cfg(feature = "hybrid-remote")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_dns_waiter_keeps_physical_capacity_until_actual_finish() {
        use reqwest::dns::Resolve;
        use std::sync::Mutex;
        use std::sync::atomic::Ordering;
        use std::task::Poll;
        let daemon = crate::daemon::settings::standalone().unwrap();
        let mut policy = crate::hybrid::remote_settings::standalone()
            .unwrap()
            .into_settings();
        policy.interactive_active = 1;
        let workers = Arc::new(WorkerAdmission::with_policies(daemon.settings(), &policy).unwrap());
        let (activity, mut events) = ActivitySource::channel(daemon.settings().max_events);
        let mut resolver =
            BoundedResolver::with_owners(&policy, workers, activity.clone()).unwrap();
        let (release, gate) = std::sync::mpsc::channel();
        let gate = Arc::new(Mutex::new(gate));
        let (started, ready) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        resolver.lookup = Arc::new(move |_| {
            if let Some(started) = started.lock().unwrap().take() {
                let _ = started.send(());
            }
            gate.lock().unwrap().recv().unwrap();
            Ok(vec!["127.0.0.1:0".parse().unwrap()])
        });
        let first = tokio::spawn(resolver.resolve("openrouter.ai".parse().unwrap()));
        ready.await.unwrap();
        first.abort();
        assert!(first.await.is_err());
        assert_eq!(activity.counter.load(Ordering::SeqCst), 1);
        let mut second = resolver.resolve("openrouter.ai".parse().unwrap());
        std::future::poll_fn(|cx| {
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(
            resolver
                .resolve("openrouter.ai".parse().unwrap())
                .await
                .is_err()
        );
        resolver.workers.shutdown();
        assert!(second.await.is_err());
        assert!(
            resolver
                .resolve("openrouter.ai".parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(activity.counter.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while activity.counter.load(Ordering::SeqCst) != 0 {
                events.recv().await.unwrap();
            }
        })
        .await
        .expect("physical DNS retirement");
    }
}
