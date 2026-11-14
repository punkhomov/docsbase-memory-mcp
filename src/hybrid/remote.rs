//! Async remote execution: one budget, bounded admission, no blocking retry waits.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::daemon::request::{self, ActivitySource};
use crate::daemon::settings::DaemonSettings;
use crate::daemon::worker_admission::WorkerAdmission;
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::search::operation::SearchOperation;

use super::remote_settings::{RemoteSettings, RemoteSpec};
use super::wire::{TransportResponse, error_for_status, is_retryable};

/// Object-safe provider future. No runtime or thread-local execution guard is retained.
pub type StageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Encoded request. Debug intentionally omits credentials and document/query input.
#[derive(Clone)]
pub struct WireRequest {
    /// Validated provider endpoint.
    pub url: String,
    /// Secret-bearing headers; never exposed through Debug or metrics.
    pub headers: Vec<(String, String)>,
    /// Encoded JSON body.
    pub body: String,
}

/// Async HTTP seam; implementation owns no fallback runtime.
pub trait AsyncTransport: Send + Sync {
    /// Perform one attempt, including bounded response consumption.
    fn post_json<'a>(
        &'a self,
        request: &'a WireRequest,
        timeout: Duration,
    ) -> StageFuture<'a, TransportResponse>;
}

/// Remote embedding capability of an existing provider stage.
pub trait AsyncEmbedder: Send + Sync {
    /// Embed owned document inputs without parking a blocking worker on I/O.
    fn embed<'a>(
        &'a self,
        texts: Arc<[String]>,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<Vec<f32>>>;
    /// Preserve the provider's query role.
    fn embed_query<'a>(&'a self, text: Arc<str>, call: &'a RemoteCall)
    -> StageFuture<'a, Vec<f32>>;
}

/// Remote rerank capability of an existing provider stage.
pub trait AsyncReranker: Send + Sync {
    /// Score the complete owned candidate window.
    fn rerank<'a>(
        &'a self,
        query: Arc<str>,
        documents: Arc<[String]>,
        top_n: usize,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<(usize, f32)>>;
}

pub use super::work::{RemoteCall, SharedInterest};

#[derive(Default)]
struct Counters {
    active: AtomicUsize,
    pending: AtomicUsize,
    attempts: AtomicU64,
    queue_ms: AtomicU64,
    network_ms: AtomicU64,
    backoff_ms: AtomicU64,
    cpu_ms: AtomicU64,
}

/// Bounded diagnostic snapshot, without payload/model/credential labels.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RemoteSnapshot {
    /// Admitted logical stages, including backoff.
    pub active: usize,
    /// Waiting logical stages.
    pub pending: usize,
    /// Actual HTTP sends, once per attempt.
    pub attempts: u64,
    /// Cumulative queue wait.
    pub queue_ms: u64,
    /// Cumulative attempt wait.
    pub network_ms: u64,
    /// Cumulative retry wait.
    pub backoff_ms: u64,
    /// Cumulative actual CPU closure time.
    pub cpu_ms: u64,
}

/// One owner for HTTP admission, shared transport, CPU dependencies and shutdown.
pub struct RemoteExecution {
    settings: RemoteSettings,
    transport: Arc<dyn AsyncTransport>,
    active: Arc<Semaphore>,
    pending: Arc<Semaphore>,
    counters: Arc<Counters>,
    shutdown: Cancellation,
    workers: Arc<WorkerAdmission>,
    activity: ActivitySource,
    #[cfg(test)]
    timing: Option<TestTiming>,
    #[cfg(test)]
    cpu_observer: Option<Arc<dyn Fn() + Send + Sync>>,
}

// Results can outlive an abandoned JoinHandle. Drop returned data before its charge.
struct CpuOutput<T> {
    value: Result<T>,
    _retention: RemoteCall,
}

#[cfg(test)]
type TestSleep = Arc<dyn Fn(Duration) -> StageFuture<'static, ()> + Send + Sync>;
#[cfg(test)]
struct TestTiming {
    sleep: TestSleep,
    jitter: Arc<dyn Fn(u64) -> Duration + Send + Sync>,
}

impl RemoteExecution {
    /// Explicit standalone/test composition; all defaults were resolved by owners.
    ///
    /// # Errors
    /// Invalid complete policy.
    pub fn with_transport(
        settings: &RemoteSettings,
        daemon: &DaemonSettings,
        transport: Arc<dyn AsyncTransport>,
    ) -> Result<Self> {
        let workers = Arc::new(WorkerAdmission::with_policies(daemon, settings)?);
        let (activity, _events) = ActivitySource::channel(daemon.max_events);
        Self::with_owners(settings, transport, workers, activity)
    }

    /// Build the real client and resolver on the same worker and physical owners.
    ///
    /// # Errors
    /// Invalid complete policies or HTTP client construction failure.
    pub fn new(settings: &RemoteSettings, daemon: &DaemonSettings) -> Result<Self> {
        let workers = Arc::new(WorkerAdmission::with_policies(daemon, settings)?);
        let (activity, _events) = ActivitySource::channel(daemon.max_events);
        Self::real_with_owners(settings, workers, activity)
    }

    pub(crate) fn real_with_owners(
        settings: &RemoteSettings,
        workers: Arc<WorkerAdmission>,
        activity: ActivitySource,
    ) -> Result<Self> {
        Self::real_with_resolver(
            settings,
            workers,
            activity,
            |settings, workers, activity, shutdown| {
                crate::daemon::remote::BoundedResolver::with_owners(
                    settings, workers, activity, shutdown,
                )
                .map(Arc::new)
            },
        )
    }

    fn real_with_resolver(
        settings: &RemoteSettings,
        workers: Arc<WorkerAdmission>,
        activity: ActivitySource,
        resolver: impl FnOnce(
            &RemoteSettings,
            Arc<WorkerAdmission>,
            ActivitySource,
            Cancellation,
        ) -> Result<Arc<crate::daemon::remote::BoundedResolver>>,
    ) -> Result<Self> {
        let shutdown = Cancellation::new();
        let resolver = resolver(
            settings,
            Arc::clone(&workers),
            activity.clone(),
            shutdown.clone(),
        )?;
        let transport = Arc::new(super::openrouter::ReqwestTransport::with_settings(
            settings, resolver,
        )?);
        Self::with_shutdown(settings, transport, workers, activity, shutdown)
    }

    pub(crate) fn with_owners(
        settings: &RemoteSettings,
        transport: Arc<dyn AsyncTransport>,
        workers: Arc<WorkerAdmission>,
        activity: ActivitySource,
    ) -> Result<Self> {
        Self::with_shutdown(settings, transport, workers, activity, Cancellation::new())
    }

    fn with_shutdown(
        settings: &RemoteSettings,
        transport: Arc<dyn AsyncTransport>,
        workers: Arc<WorkerAdmission>,
        activity: ActivitySource,
        shutdown: Cancellation,
    ) -> Result<Self> {
        RemoteSpec::validate(settings, &Capabilities::current())?;
        Ok(Self {
            settings: settings.clone(),
            transport,
            active: Arc::new(Semaphore::new(settings.interactive_active)),
            pending: Arc::new(Semaphore::new(settings.interactive_pending)),
            counters: Arc::new(Counters::default()),
            shutdown,
            workers,
            activity,
            #[cfg(test)]
            timing: None,
            #[cfg(test)]
            cpu_observer: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn observe_cpu(&mut self, observer: Arc<dyn Fn() + Send + Sync>) {
        self.cpu_observer = Some(observer);
    }

    /// Complete immutable owner policy.
    #[must_use]
    pub fn settings(&self) -> &RemoteSettings {
        &self.settings
    }

    /// Snapshot aggregate counters; durations can overlap and are not wall-clock partitions.
    #[must_use]
    pub fn snapshot(&self) -> RemoteSnapshot {
        let c = &self.counters;
        RemoteSnapshot {
            active: c.active.load(Ordering::Relaxed),
            pending: c.pending.load(Ordering::Relaxed),
            attempts: c.attempts.load(Ordering::Relaxed),
            queue_ms: c.queue_ms.load(Ordering::Relaxed),
            network_ms: c.network_ms.load(Ordering::Relaxed),
            backoff_ms: c.backoff_ms.load(Ordering::Relaxed),
            cpu_ms: c.cpu_ms.load(Ordering::Relaxed),
        }
    }

    /// Stop future/queued/async work. Started blocking closures keep physical ownership.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Wait for actual blocking owners using the caller's resolved lifecycle interval.
    pub async fn drain(&self, poll_interval: Duration) {
        self.activity.drain(poll_interval).await;
    }

    /// Acquire a logical provider stage through original operation admission.
    ///
    /// # Errors
    /// Full queue, cancellation, shutdown or expired original budget.
    pub async fn admit(&self, call: &RemoteCall) -> Result<RemoteStage<'_>> {
        call.check()?;
        self.shutdown.check()?;
        let started = Instant::now();
        let permit = if let Ok(permit) = Arc::clone(&self.active).try_acquire_owned() {
            permit
        } else {
            let pending = Arc::clone(&self.pending)
                .try_acquire_owned()
                .map_err(|_| denied("remote pending limit reached"))?;
            let _charge = CounterCharge::new(&self.counters.pending);
            let _queue = QueueCharge(&self.counters.queue_ms, call.operation(), started);
            let result = call
                .wait(&self.shutdown, async {
                    Arc::clone(&self.active)
                        .acquire_owned()
                        .await
                        .map_err(|_| denied("remote lane closed"))
                })
                .await;
            drop(pending);
            result?
        };
        call.check()?;
        self.shutdown.check()?;
        self.counters.active.fetch_add(1, Ordering::Relaxed);
        Ok(RemoteStage {
            execution: self,
            _permit: permit,
            _activity: self.activity.track(),
        })
    }

    /// Execute one bounded stage containing an HTTP retry chain.
    ///
    /// # Errors
    /// Admission, deadline, cancellation, transport or explicit provider error.
    pub async fn post(
        &self,
        call: &RemoteCall,
        request: &WireRequest,
    ) -> Result<TransportResponse> {
        self.admit(call).await?.post(call, request).await
    }

    /// Run bounded local inference or codec work; permits survive waiter cancellation/drop.
    ///
    /// # Errors
    /// Admission/cancellation, worker join or work failure.
    pub async fn cpu<T: Send + 'static>(
        &self,
        call: &RemoteCall,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mut cancelled = request::CancelOnDrop(Some(call.operation().cancellation().clone()));
        let permit = call
            .wait(&self.shutdown, self.workers.search(call.operation()))
            .await?;
        let counters = Arc::clone(&self.counters);
        let worker_call = call.clone();
        let shutdown = self.shutdown.clone();
        #[cfg(test)]
        let cpu_observer = self.cpu_observer.clone();
        let worker = request::spawn_retained(
            Some(permit),
            Some(self.activity.track()),
            call.activity(),
            move || {
                // Declare retention before work so input captures drop first on every exit.
                let (retention, body) = (worker_call, work);
                retention.check()?;
                shutdown.check()?;
                let _time = DurationCharge(&counters.cpu_ms, Instant::now());
                #[cfg(test)]
                if let Some(observer) = cpu_observer {
                    observer();
                }
                Ok(CpuOutput {
                    value: body(),
                    _retention: retention,
                })
            },
        );
        let result = call
            .wait(&self.shutdown, async {
                let output = worker.await.map_err(|error| {
                    Error::internal_with_source("remote CPU task join", error)
                })??;
                output.value
            })
            .await;
        if result.is_ok() {
            cancelled.0 = None;
        }
        result
    }
}

/// Provider permit retained throughout a multi-batch logical stage and all retries.
pub struct RemoteStage<'a> {
    execution: &'a RemoteExecution,
    _permit: OwnedSemaphorePermit,
    _activity: Arc<crate::daemon::request::Activity>,
}

impl RemoteStage<'_> {
    /// Send with preserved status/retry semantics and cancellable absolute waits.
    ///
    /// # Errors
    /// Deadline/cancellation, transport or explicit provider failure.
    pub async fn post(
        &self,
        call: &RemoteCall,
        request: &WireRequest,
    ) -> Result<TransportResponse> {
        let execution = self.execution;
        let policy = &execution.settings;
        let mut delay = Duration::from_millis(policy.retry_initial_ms);
        for attempt in 1..=policy.retry_attempts {
            call.check()?;
            execution.shutdown.check()?;
            let timeout = call.attempt_timeout(policy)?;
            execution.counters.attempts.fetch_add(1, Ordering::Relaxed);
            let result = {
                let _time = DurationCharge(&execution.counters.network_ms, Instant::now());
                call.wait(&execution.shutdown, async {
                    tokio::time::timeout(timeout, execution.transport.post_json(request, timeout))
                        .await
                        .map_err(|error| {
                            Error::internal_with_source("remote HTTP attempt timed out", error)
                        })?
                })
                .await
            };
            // Deadline/cancellation is terminal, never another transport retry.
            call.check()?;
            execution.shutdown.check()?;
            let (error, after) = match result {
                Ok(response) if (200..300).contains(&response.status) => return Ok(response),
                Ok(response) => {
                    let retryable = is_retryable(response.status);
                    let after = (response.status == 429)
                        .then_some(response.retry_after)
                        .flatten();
                    let error = execution
                        .cpu(call, move || {
                            Ok(error_for_status(response.status, &response.body))
                        })
                        .await?;
                    if !retryable {
                        return Err(error);
                    }
                    (error, after)
                }
                Err(error @ Error::Admission { .. }) => return Err(error),
                Err(error) => (error, None),
            };
            if attempt == policy.retry_attempts {
                return Err(error);
            }
            let noise = {
                #[cfg(test)]
                if let Some(timing) = &execution.timing {
                    (timing.jitter)(policy.jitter_max_ms)
                } else {
                    jitter(policy.jitter_max_ms)
                }
                #[cfg(not(test))]
                jitter(policy.jitter_max_ms)
            };
            let pause = after
                .map_or(delay, |after| {
                    after.min(Duration::from_millis(policy.max_retry_after_ms))
                })
                .saturating_add(noise);
            let wake = Instant::now()
                .checked_add(pause)
                .ok_or_else(|| denied("remote retry clock range exceeded"))?;
            {
                let _time = DurationCharge(&execution.counters.backoff_ms, Instant::now());
                #[cfg(test)]
                let sleep = execution
                    .timing
                    .as_ref()
                    .map(|timing| (timing.sleep)(pause));
                call.wait(&execution.shutdown, async {
                    #[cfg(test)]
                    if let Some(sleep) = sleep {
                        return sleep.await;
                    }
                    tokio::time::sleep_until(tokio::time::Instant::from_std(wake)).await;
                    Ok(())
                })
                .await?;
            }
            delay = delay
                .saturating_mul(policy.retry_multiplier)
                .min(Duration::from_millis(policy.retry_max_ms));
        }
        Err(Error::internal("validated remote retry policy exhausted"))
    }
}

impl Drop for RemoteStage<'_> {
    fn drop(&mut self) {
        self.execution
            .counters
            .active
            .fetch_sub(1, Ordering::Relaxed);
    }
}

struct CounterCharge<'a>(&'a AtomicUsize);
struct DurationCharge<'a>(&'a AtomicU64, Instant);
struct QueueCharge<'a>(&'a AtomicU64, &'a SearchOperation, Instant);
impl Drop for QueueCharge<'_> {
    fn drop(&mut self) {
        let elapsed = self.2.elapsed();
        record(self.0, elapsed);
        self.1.record_queue(elapsed);
    }
}
impl Drop for DurationCharge<'_> {
    fn drop(&mut self) {
        record(self.0, self.1.elapsed());
    }
}
impl<'a> CounterCharge<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}
impl Drop for CounterCharge<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn record(counter: &AtomicU64, elapsed: Duration) {
    let milliseconds = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(milliseconds))
    });
}

fn jitter(maximum: u64) -> Duration {
    if maximum == 0 {
        return Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()));
    Duration::from_millis(nanos % maximum)
}

fn denied(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn large_error_envelope_obeys_actual_cpu_admission_and_cancellation() {
        struct LargeError(String);
        impl AsyncTransport for LargeError {
            fn post_json<'a>(
                &'a self,
                _: &'a WireRequest,
                _: Duration,
            ) -> StageFuture<'a, TransportResponse> {
                Box::pin(async move {
                    Ok(TransportResponse {
                        status: 429,
                        body: self.0.clone(),
                        retry_after: Some(Duration::ZERO),
                    })
                })
            }
        }
        for cancel in [false, true] {
            let mut daemon = crate::daemon::settings::standalone()
                .unwrap()
                .into_settings();
            daemon.search_active = 1;
            daemon.search_pending = 1;
            let mut policy = super::super::remote_settings::standalone()
                .unwrap()
                .into_settings();
            policy.retry_attempts = 1;
            let workers = Arc::new(WorkerAdmission::with_policies(&daemon, &policy).unwrap());
            let (activity, _) = ActivitySource::channel(daemon.max_events);
            let body = serde_json::json!({"padding": vec!["bulk".repeat(64); 4096], "error":{"message":"safe fixture failure"}}).to_string();
            assert!(body.len() > 1024 * 1024);
            let mut execution = RemoteExecution::with_owners(
                &policy,
                Arc::new(LargeError(body)),
                workers.clone(),
                activity.clone(),
            )
            .unwrap();
            let decoded = Arc::new(AtomicUsize::new(0));
            let observed = decoded.clone();
            execution.observe_cpu(Arc::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            }));
            let call = call();
            let permit = workers.search(call.operation()).await.unwrap();
            let (release, proceed) = std::sync::mpsc::channel();
            let occupied = request::spawn(Some(permit), Some(activity.track()), move || {
                proceed.recv().unwrap();
                Ok(())
            });
            let request = wire_request();
            let mut work = Box::pin(execution.post(&call, &request));
            let completion =
                std::future::poll_fn(|cx| std::task::Poll::Ready(work.as_mut().poll(cx))).await;
            let pending = completion.is_pending();
            assert_eq!(decoded.load(Ordering::SeqCst), 0);
            if cancel {
                call.operation().cancellation().cancel();
            }
            // Free the actual occupied CPU closure before every RED assertion.
            release.send(()).unwrap();
            occupied.await.unwrap().unwrap();
            let result = match completion {
                std::task::Poll::Ready(result) => result,
                std::task::Poll::Pending => work.await,
            };
            assert!(
                pending,
                "large non-2xx decoding bypassed occupied CPU admission"
            );
            let error = result.unwrap_err();
            if cancel {
                assert!(error.inner_message().contains("cancelled"));
                assert_eq!(decoded.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(
                    error.inner_message(),
                    "OpenRouter error 429: safe fixture failure"
                );
                assert_eq!(decoded.load(Ordering::SeqCst), 1);
            }
            assert_eq!(execution.snapshot().attempts, 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execution_shutdown_retires_retained_real_resolver_without_other_lanes() {
        use reqwest::dns::Resolve;
        use std::task::Poll;
        let daemon = crate::daemon::settings::standalone().unwrap();
        let mut policy = super::super::remote_settings::standalone()
            .unwrap()
            .into_settings();
        policy.interactive_active = 1;
        let workers = Arc::new(WorkerAdmission::with_policies(daemon.settings(), &policy).unwrap());
        let (activity, _events) = ActivitySource::channel(daemon.settings().max_events);
        let (release, gate) = std::sync::mpsc::channel();
        let gate = Mutex::new(gate);
        let (started, ready) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let lookups = Arc::new(AtomicUsize::new(0));
        let lookup_count = lookups.clone();
        let mut captured = None;
        let execution = RemoteExecution::real_with_resolver(
            &policy,
            workers,
            activity.clone(),
            |settings, workers, activity, shutdown| {
                let resolver = Arc::new(
                    crate::daemon::remote::BoundedResolver::with_owners(
                        settings, workers, activity, shutdown,
                    )
                    .unwrap()
                    .with_lookup(move |_| {
                        if lookup_count.fetch_add(1, Ordering::SeqCst) == 0 {
                            let _ = started.lock().unwrap().take().unwrap().send(());
                            gate.lock().unwrap().recv().unwrap();
                        }
                        Ok(vec!["127.0.0.1:0".parse().unwrap()])
                    }),
                );
                captured = Some(resolver.clone());
                Ok(resolver)
            },
        )
        .unwrap();
        let resolver = captured.unwrap();
        let first = tokio::spawn(resolver.resolve("openrouter.ai".parse().unwrap()));
        ready.await.unwrap();
        first.abort();
        assert!(first.await.is_err());
        let mut second = resolver.resolve("openrouter.ai".parse().unwrap());
        std::future::poll_fn(|cx| {
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        execution.shutdown();
        let pending_rejected = std::future::poll_fn(|cx| {
            Poll::Ready(matches!(second.as_mut().poll(cx), Poll::Ready(Err(_))))
        })
        .await;
        // Always release the native gate before assertions, including the RED path.
        release.send(()).unwrap();
        execution.drain(Duration::from_millis(1)).await;
        drop(second);
        let new_rejected = resolver
            .resolve("openrouter.ai".parse().unwrap())
            .await
            .is_err();
        assert!(execution.workers.control().await.is_ok());
        assert!(execution.workers.search(call().operation()).await.is_ok());
        assert!(
            pending_rejected,
            "execution shutdown must reject a retained DNS admission waiter"
        );
        assert!(
            new_rejected,
            "retired execution must reject new DNS work after drain"
        );
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
        assert_eq!(activity.counter.load(Ordering::SeqCst), 0);
    }

    struct Responses(Mutex<std::collections::VecDeque<(u16, Option<Duration>)>>);
    impl AsyncTransport for Responses {
        fn post_json<'a>(
            &'a self,
            _: &'a WireRequest,
            _: Duration,
        ) -> StageFuture<'a, TransportResponse> {
            Box::pin(async move {
                let (status, retry_after) = self.0.lock().unwrap().pop_front().unwrap();
                Ok(TransportResponse {
                    status,
                    retry_after,
                    body: "{}".into(),
                })
            })
        }
    }

    fn call() -> RemoteCall {
        RemoteCall::new(
            SearchOperation::new(
                crate::search::settings::standalone().unwrap().settings(),
                Instant::now(),
                Cancellation::new(),
            )
            .unwrap(),
        )
    }

    fn observed_execution(
        policy: &RemoteSettings,
        responses: Vec<(u16, Option<Duration>)>,
        delays: Arc<Mutex<Vec<Duration>>>,
    ) -> RemoteExecution {
        let mut execution = RemoteExecution::with_transport(
            policy,
            crate::daemon::settings::standalone().unwrap().settings(),
            Arc::new(Responses(Mutex::new(responses.into()))),
        )
        .unwrap();
        execution.timing = Some(TestTiming {
            jitter: Arc::new(|maximum| Duration::from_millis(maximum.saturating_sub(1))),
            sleep: Arc::new(move |delay| {
                delays.lock().unwrap().push(delay);
                Box::pin(async { Ok(()) })
            }),
        });
        execution
    }

    fn expiring_call() -> RemoteCall {
        let mut policy = crate::search::settings::standalone()
            .unwrap()
            .into_settings();
        policy.request_timeout_ms = 1_000;
        RemoteCall::new(SearchOperation::new(&policy, Instant::now(), Cancellation::new()).unwrap())
    }

    fn wire_request() -> WireRequest {
        WireRequest {
            url: "https://openrouter.ai/api/v1/embeddings".into(),
            headers: vec![],
            body: "{}".into(),
        }
    }

    fn assert_expired(result: Result<TransportResponse>, call: &RemoteCall) {
        assert!(
            result
                .unwrap_err()
                .inner_message()
                .contains("deadline expired")
        );
        assert!(call.operation().cancellation().is_cancelled());
    }

    #[tokio::test]
    async fn original_deadline_expires_while_provider_admission_is_queued() {
        use std::task::Poll;
        let mut policy = super::super::remote_settings::standalone()
            .unwrap()
            .into_settings();
        policy.interactive_active = 1;
        let execution = RemoteExecution::with_transport(
            &policy,
            crate::daemon::settings::standalone().unwrap().settings(),
            Arc::new(Responses(Mutex::new(vec![(200, None)].into()))),
        )
        .unwrap();
        let held = execution.admit(&call()).await.unwrap();
        let expiring = expiring_call();
        let request = wire_request();
        let mut work = Box::pin(execution.post(&expiring, &request));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(execution.snapshot().pending, 1);
        assert_expired(
            tokio::time::timeout(Duration::from_secs(5), work)
                .await
                .unwrap(),
            &expiring,
        );
        assert_eq!(execution.snapshot().pending, 0);
        assert_eq!(execution.snapshot().attempts, 0);
        drop(held);
        assert!(execution.post(&call(), &request).await.is_ok());
        assert_eq!(execution.snapshot().attempts, 1);
    }

    #[derive(Default)]
    struct HeldRequest(Mutex<Vec<Duration>>);
    impl AsyncTransport for HeldRequest {
        fn post_json<'a>(
            &'a self,
            _: &'a WireRequest,
            timeout: Duration,
        ) -> StageFuture<'a, TransportResponse> {
            Box::pin(async move {
                self.0.lock().unwrap().push(timeout);
                std::future::pending().await
            })
        }
    }

    #[tokio::test]
    async fn original_deadline_expires_during_a_held_network_attempt() {
        use std::task::Poll;
        let transport = Arc::new(HeldRequest::default());
        let execution = RemoteExecution::with_transport(
            super::super::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap();
        let expiring = expiring_call();
        let request = wire_request();
        let mut work = Box::pin(execution.post(&expiring, &request));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(transport.0.lock().unwrap().len(), 1);
        assert_expired(
            tokio::time::timeout(Duration::from_secs(5), work)
                .await
                .unwrap(),
            &expiring,
        );
        assert_eq!(transport.0.lock().unwrap().len(), 1);
        assert_eq!(execution.snapshot().active, 0);
    }

    #[tokio::test]
    async fn original_deadline_expires_during_backoff_including_jitter() {
        for cancel in [false, true] {
            let policy = super::super::remote_settings::standalone().unwrap();
            let delays = Arc::new(Mutex::new(Vec::new()));
            let observed = delays.clone();
            let mut execution = observed_execution(
                policy.settings(),
                vec![(429, Some(Duration::from_secs(90)))],
                delays.clone(),
            );
            let (entered, backoff) = tokio::sync::oneshot::channel();
            let entered = Mutex::new(Some(entered));
            execution.timing.as_mut().unwrap().sleep = Arc::new(move |delay| {
                observed.lock().unwrap().push(delay);
                entered.lock().unwrap().take().unwrap().send(()).unwrap();
                Box::pin(std::future::pending())
            });
            let expiring = expiring_call();
            let request = wire_request();
            let mut work = Box::pin(execution.post(&expiring, &request));
            tokio::select! {
                result = &mut work => panic!("request completed before backoff entry: {result:?}"),
                result = backoff => result.unwrap(),
            }
            assert_eq!(*delays.lock().unwrap(), vec![Duration::from_millis(30_049)]);
            if cancel {
                expiring.operation().cancellation().cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(5), work)
                .await
                .unwrap();
            if cancel {
                assert!(result.unwrap_err().inner_message().contains("cancelled"));
            } else {
                assert_expired(result, &expiring);
            }
            assert_eq!(execution.snapshot().attempts, 1);
            assert_eq!(execution.snapshot().active, 0);
        }
    }

    #[tokio::test]
    async fn ordinary_partially_consumed_budget_caps_the_actual_attempt_timeout() {
        use std::task::Poll;
        for attempt_timeout_ms in [30_000, 73] {
            let mut policy = super::super::remote_settings::standalone()
                .unwrap()
                .into_settings();
            policy.attempt_timeout_ms = attempt_timeout_ms;
            let transport = Arc::new(HeldRequest::default());
            let execution = RemoteExecution::with_transport(
                &policy,
                crate::daemon::settings::standalone().unwrap().settings(),
                transport.clone(),
            )
            .unwrap();
            let search = crate::search::settings::standalone().unwrap();
            let started = Instant::now()
                .checked_sub(Duration::from_millis(
                    search.settings().request_timeout_ms / 3,
                ))
                .unwrap();
            let call = RemoteCall::new(
                SearchOperation::new(search.settings(), started, Cancellation::new()).unwrap(),
            );
            let before = call.operation().remaining().unwrap();
            let request = wire_request();
            let mut work = Box::pin(execution.post(&call, &request));
            std::future::poll_fn(|cx| {
                assert!(work.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let after = call.operation().remaining().unwrap();
            let observed = transport.0.lock().unwrap()[0];
            let policy_cap = Duration::from_millis(attempt_timeout_ms);
            assert!(observed <= before.min(policy_cap));
            assert!(observed >= after.min(policy_cap));
            call.operation().cancellation().cancel();
            assert!(work.await.is_err());
            assert_eq!(transport.0.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn actual_retry_chain_observes_default_retry_after_backoff_and_jitter() {
        let policy = super::super::remote_settings::standalone().unwrap();
        let delays = Arc::new(Mutex::new(Vec::new()));
        let execution = observed_execution(
            policy.settings(),
            vec![
                (429, Some(Duration::from_secs(90))),
                (500, None),
                (503, None),
                (529, None),
            ],
            delays.clone(),
        );
        let request = WireRequest {
            url: "https://openrouter.ai/api/v1/embeddings".into(),
            headers: vec![],
            body: "{}".into(),
        };
        assert!(execution.post(&call(), &request).await.is_err());
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                Duration::from_millis(30_049),
                Duration::from_millis(549),
                Duration::from_millis(1_049)
            ]
        );
        assert_eq!(execution.snapshot().attempts, 4);
    }

    #[tokio::test]
    async fn actual_retry_chain_consumes_overridden_multiplier_and_cap() {
        let mut policy = super::super::remote_settings::standalone()
            .unwrap()
            .into_settings();
        policy.retry_initial_ms = 3_000;
        policy.retry_multiplier = 3;
        policy.retry_max_ms = 4_000;
        policy.jitter_max_ms = 0;
        let delays = Arc::new(Mutex::new(Vec::new()));
        let execution = observed_execution(
            &policy,
            vec![(503, None), (503, None), (503, None), (200, None)],
            delays.clone(),
        );
        let request = WireRequest {
            url: "https://openrouter.ai/api/v1/embeddings".into(),
            headers: vec![],
            body: "{}".into(),
        };
        assert!(execution.post(&call(), &request).await.is_ok());
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                Duration::from_millis(3_000),
                Duration::from_millis(4_000),
                Duration::from_millis(4_000)
            ]
        );
    }
}
