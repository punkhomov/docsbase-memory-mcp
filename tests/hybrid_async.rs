#![cfg(feature = "hybrid-remote")]

use docsbase_memory::hybrid::openrouter::ReqwestTransport;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use docsbase_memory::error::Result;
use docsbase_memory::hybrid::remote::{
    AsyncTransport, RemoteCall, RemoteExecution, StageFuture, WireRequest,
};
use docsbase_memory::hybrid::remote_settings;
use docsbase_memory::hybrid::wire::TransportResponse;
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::search::operation::SearchOperation;

struct Scripted {
    statuses: Mutex<std::collections::VecDeque<u16>>,
    calls: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl AsyncTransport for Scripted {
    fn post_json<'a>(
        &'a self,
        _request: &'a WireRequest,
        _timeout: Duration,
    ) -> StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let status = self.statuses.lock().unwrap().pop_front().unwrap_or(200);
            let permit = self.gate.acquire().await.unwrap();
            permit.forget();
            if status == 0 {
                return Err(docsbase_memory::error::Error::internal(
                    "controlled transport failure",
                ));
            }
            Ok(TransportResponse {
                status,
                body: "{}".into(),
                retry_after: None,
            })
        })
    }
}

fn call() -> RemoteCall {
    let settings = docsbase_memory::search::settings::standalone().unwrap();
    RemoteCall::new(
        SearchOperation::new(settings.settings(), Instant::now(), Cancellation::new()).unwrap(),
    )
}

fn request() -> WireRequest {
    WireRequest {
        url: "https://openrouter.ai/api/v1/embeddings".into(),
        headers: Vec::new(),
        body: "{}".into(),
    }
}

fn immediate_policy() -> remote_settings::RemoteSettings {
    let mut policy = remote_settings::standalone().unwrap().into_settings();
    policy.retry_initial_ms = 0;
    policy.retry_max_ms = 0;
    policy.jitter_max_ms = 0;
    policy
}

fn scripted(statuses: &[u16], permits: usize) -> Arc<Scripted> {
    Arc::new(Scripted {
        statuses: Mutex::new(statuses.iter().copied().collect()),
        calls: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(permits),
    })
}

#[tokio::test]
async fn pending_overload_and_cancel_never_start_another_http_attempt() {
    use std::future::Future;
    use std::task::Poll;
    let mut policy = immediate_policy();
    policy.interactive_active = 1;
    policy.interactive_pending = 1;
    let transport = scripted(&[200, 200], 0);
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()
            .unwrap()
            .settings(),
        transport.clone(),
    )
    .unwrap();
    let first_call = call();
    let second_call = call();
    let req = request();
    let mut first = Box::pin(execution.post(&first_call, &req));
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut second = Box::pin(execution.post(&second_call, &req));
    std::future::poll_fn(|cx| {
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(execution.snapshot().pending, 1);
    assert!(execution.post(&call(), &req).await.is_err());
    second_call.operation().cancellation().cancel();
    assert!(second.await.is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution.snapshot().pending, 0);
    transport.gate.add_permits(1);
    assert!(first.await.is_ok());
}

#[tokio::test]
async fn retryable_and_terminal_statuses_keep_the_existing_policy() {
    for status in [
        0, 429, 500, 502, 503, 524, 529, 400, 401, 402, 403, 404, 413,
    ] {
        let transport = scripted(&[status, 200], 2);
        let execution = RemoteExecution::with_transport(
            &immediate_policy(),
            docsbase_memory::daemon::settings::standalone()
                .unwrap()
                .settings(),
            transport.clone(),
        )
        .unwrap();
        let retryable = status == 0 || docsbase_memory::hybrid::wire::is_retryable(status);
        assert_eq!(execution.post(&call(), &request()).await.is_ok(), retryable);
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            if retryable { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn final_attempt_returns_without_waiting_for_backoff() {
    use std::future::Future;
    use std::task::Poll;
    let mut policy = immediate_policy();
    policy.retry_attempts = 1;
    policy.retry_initial_ms = 3_600_000;
    policy.retry_max_ms = policy.retry_initial_ms;
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()
            .unwrap()
            .settings(),
        scripted(&[503], 1),
    )
    .unwrap();
    let call = call();
    let req = request();
    let mut work = Box::pin(execution.post(&call, &req));
    std::future::poll_fn(|cx| {
        assert!(matches!(work.as_mut().poll(cx), Poll::Ready(Err(_))));
        Poll::Ready(())
    })
    .await;
    assert_eq!(execution.snapshot().attempts, 1);
}

#[tokio::test]
async fn cancelled_backoff_starts_no_new_attempt() {
    use std::future::Future;
    use std::task::Poll;
    let mut policy = immediate_policy();
    policy.retry_initial_ms = 3_600_000;
    policy.retry_max_ms = policy.retry_initial_ms;
    let transport = scripted(&[503, 200], 2);
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()
            .unwrap()
            .settings(),
        transport.clone(),
    )
    .unwrap();
    let call = call();
    let req = request();
    let mut work = Box::pin(execution.post(&call, &req));
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    call.operation().cancellation().cancel();
    assert!(work.await.is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_original_budget_starts_no_provider_work() {
    let policy = docsbase_memory::search::settings::standalone().unwrap();
    let started = Instant::now()
        .checked_sub(Duration::from_millis(
            policy.settings().request_timeout_ms + 1,
        ))
        .unwrap();
    let call = RemoteCall::new(
        SearchOperation::new(policy.settings(), started, Cancellation::new()).unwrap(),
    );
    let transport = scripted(&[200], 1);
    let execution = RemoteExecution::with_transport(
        &immediate_policy(),
        docsbase_memory::daemon::settings::standalone()
            .unwrap()
            .settings(),
        transport.clone(),
    )
    .unwrap();
    assert!(execution.post(&call, &request()).await.is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

struct EchoCodec {
    requests: Mutex<Vec<serde_json::Value>>,
}
impl AsyncTransport for EchoCodec {
    fn post_json<'a>(
        &'a self,
        request: &'a WireRequest,
        _timeout: Duration,
    ) -> StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            self.requests.lock().unwrap().push(body.clone());
            let response = if let Some(input) = body["input"].as_array() {
                serde_json::json!({"data":input.iter().enumerate().map(|(index, _)| serde_json::json!({"index":index,"embedding":[3.0,4.0]})).collect::<Vec<_>>()})
            } else {
                serde_json::json!({"results":[{"index":2,"relevance_score":0.9},{"index":0,"relevance_score":0.1},{"index":1,"relevance_score":0.5}]})
            };
            Ok(TransportResponse {
                status: 200,
                body: response.to_string(),
                retry_after: None,
            })
        })
    }
}

#[tokio::test]
async fn async_provider_reuses_rig_roles_batches_normalization_and_rerank() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, RerankerCfg};
    use docsbase_memory::hybrid::openrouter::{
        BASE_URL, OpenRouterClient, OpenRouterEmbedder, OpenRouterReranker,
    };
    use docsbase_memory::hybrid::{Reranker, blocking};
    use docsbase_memory::vector_cache::Embedder;
    let mut policy = immediate_policy();
    policy.embed_batch_texts = 2;
    policy.embed_batch_bytes = 3;
    let transport = Arc::new(EchoCodec {
        requests: Mutex::new(Vec::new()),
    });
    let execution = Arc::new(
        RemoteExecution::with_transport(
            &policy,
            docsbase_memory::daemon::settings::standalone()
                .unwrap()
                .settings(),
            transport.clone(),
        )
        .unwrap(),
    );
    let client = Arc::new(
        OpenRouterClient::with_execution(BASE_URL, "secret-credential", execution).unwrap(),
    );
    let embedder = OpenRouterEmbedder::with_client(
        EmbeddingCfg {
            model: "fixture".into(),
            dimensions: 2,
            normalize: true,
            ..EmbeddingCfg::default()
        },
        client.clone(),
    );
    let inputs: Arc<[String]> = Arc::from(["a", "bb", "c", "dd", "e"].map(str::to_owned));
    let vectors = embedder
        .async_remote()
        .unwrap()
        .embed(inputs.clone(), &call())
        .await
        .unwrap();
    assert_eq!(vectors, vec![vec![0.6, 0.8]; 5]);
    let query = embedder
        .async_remote()
        .unwrap()
        .embed_query(Arc::from("query"), &call())
        .await
        .unwrap();
    assert_eq!(query, vec![0.6, 0.8]);
    assert!(
        blocking::enter(|| embedder.embed(&inputs)).is_err(),
        "async stages have no hidden synchronous fallback"
    );
    let reranker = OpenRouterReranker::with_client(RerankerCfg::default(), client);
    let ranked = reranker
        .async_remote()
        .unwrap()
        .rerank(
            Arc::from("query"),
            Arc::from(["one".into(), "two".into(), "three".into()]),
            99,
            &call(),
        )
        .await
        .unwrap();
    assert_eq!(ranked, vec![(2, 0.9), (1, 0.5), (0, 0.1)]);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0]["input"], serde_json::json!(["a", "bb"]));
    assert_eq!(requests[1]["input"], serde_json::json!(["c", "dd"]));
    assert_eq!(requests[2]["input"], serde_json::json!(["e"]));
    assert_eq!(requests[0]["input_type"], "search_document");
    assert_eq!(requests[3]["input_type"], "search_query");
    assert_eq!(requests[4]["top_n"], 3);
}

#[tokio::test]
async fn response_cap_checks_actual_body_before_retaining_excess() {
    use docsbase_memory::hybrid::remote::AsyncTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        // Huge declared length cannot size allocation; actual data already exceeds cap.
        let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n012345678901234567890123456789").await;
    });
    let mut policy = immediate_policy();
    policy.max_response_bytes = 16;
    let resolver = Arc::new(
        docsbase_memory::daemon::remote::BoundedResolver::with_settings(
            &policy,
            docsbase_memory::daemon::settings::standalone()
                .unwrap()
                .settings(),
        )
        .unwrap(),
    );
    let transport = ReqwestTransport::with_settings(&policy, resolver).unwrap();
    let req = WireRequest {
        url: format!("http://{address}/"),
        headers: vec![],
        body: "{}".into(),
    };
    let error = AsyncTransport::post_json(&transport, &req, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        docsbase_memory::error::Error::Admission { .. }
    ));
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_local_cpu_waiter_does_not_release_running_physical_work() {
    use std::future::Future;
    use std::task::Poll;
    let mut daemon = docsbase_memory::daemon::settings::standalone()
        .unwrap()
        .into_settings();
    daemon.search_active = 1;
    let execution = Arc::new(
        RemoteExecution::with_transport(&immediate_policy(), &daemon, scripted(&[], 0)).unwrap(),
    );
    let (release, gate) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let child = execution.clone();
    let first = tokio::spawn(async move {
        child
            .cpu(&call(), move || {
                let _ = started.send(());
                gate.recv().unwrap();
                Ok(())
            })
            .await
    });
    ready.await.unwrap();
    first.abort();
    assert!(first.await.is_err());
    let entered = Arc::new(AtomicUsize::new(0));
    let observed = entered.clone();
    let operation = call();
    let mut second = Box::pin(execution.cpu(&operation, move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    std::future::poll_fn(|cx| {
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    second.await.unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 1);
}

struct Interest {
    deadline: Mutex<Option<Instant>>,
    notify: tokio::sync::Notify,
}
impl docsbase_memory::hybrid::remote::SharedInterest for Interest {
    fn deadline(&self) -> Option<Instant> {
        *self.deadline.lock().unwrap()
    }
    fn notify(&self) -> &tokio::sync::Notify {
        &self.notify
    }
}
struct TimedGate {
    timeout: Mutex<Option<Duration>>,
    calls: AtomicUsize,
    gate: tokio::sync::Semaphore,
}
impl AsyncTransport for TimedGate {
    fn post_json<'a>(
        &'a self,
        _: &'a WireRequest,
        timeout: Duration,
    ) -> StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.timeout.lock().unwrap() = Some(timeout);
            let _permit = self.gate.acquire().await.unwrap();
            Ok(TransportResponse {
                status: 200,
                body: "{}".into(),
                retry_after: None,
            })
        })
    }
}

#[tokio::test]
async fn shared_member_changes_keep_the_same_attempt_and_hard_timeout() {
    use std::future::Future;
    use std::task::Poll;
    let mut search = docsbase_memory::search::settings::standalone()
        .unwrap()
        .into_settings();
    search.request_timeout_ms = 300_000;
    let mut policy = immediate_policy();
    policy.attempt_timeout_ms = 120_000;
    policy.retry_attempts = 1;
    let interest = Arc::new(Interest {
        deadline: Mutex::new(Some(Instant::now() + Duration::from_secs(60))),
        notify: tokio::sync::Notify::new(),
    });
    let operation = SearchOperation::new(&search, Instant::now(), Cancellation::new()).unwrap();
    let call = RemoteCall::shared(operation, interest.clone());
    let transport = Arc::new(TimedGate {
        timeout: Mutex::new(None),
        calls: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(0),
    });
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()
            .unwrap()
            .settings(),
        transport.clone(),
    )
    .unwrap();
    let req = request();
    let mut work = Box::pin(execution.post(&call, &req));
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(
        *transport.timeout.lock().unwrap(),
        Some(Duration::from_secs(120))
    );
    *interest.deadline.lock().unwrap() = Some(Instant::now() + Duration::from_secs(180));
    interest.notify.notify_waiters();
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    transport.gate.add_permits(1);
    assert!(work.await.is_ok());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_a_cpu_waiter_before_executor_start_skips_its_work() {
    queued_cpu_work_is_skipped(false);
}

#[test]
fn shutdown_before_cpu_executor_start_skips_its_work() {
    queued_cpu_work_is_skipped(true);
}

fn queued_cpu_work_is_skipped(shutdown: bool) {
    use std::future::Future;
    use std::task::Poll;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (release, gate) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            gate.recv().unwrap();
        });
        ready.await.unwrap();
        let execution = RemoteExecution::with_transport(
            &immediate_policy(),
            docsbase_memory::daemon::settings::standalone()
                .unwrap()
                .settings(),
            scripted(&[], 0),
        )
        .unwrap();
        let entered = Arc::new(AtomicUsize::new(0));
        let child = entered.clone();
        let call = call();
        let mut work = Box::pin(execution.cpu(&call, move || {
            child.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        if shutdown {
            execution.shutdown();
            assert!(work.await.is_err());
        } else {
            drop(work);
        }
        release.send(()).unwrap();
        occupied.await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            execution.drain(Duration::from_millis(1)),
        )
        .await
        .unwrap();
        assert_eq!(
            entered.load(Ordering::SeqCst),
            0,
            "retired queued CPU work must not execute"
        );
    });
}

#[tokio::test]
async fn async_retry_consumes_exactly_the_owner_attempt_budget() -> Result<()> {
    let mut policy = remote_settings::standalone()?.into_settings();
    policy.retry_attempts = 3;
    policy.retry_initial_ms = 0;
    policy.retry_max_ms = 0;
    policy.jitter_max_ms = 0;
    let transport = Arc::new(Scripted {
        statuses: Mutex::new([429, 503, 200].into()),
        calls: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(3),
    });
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()?.settings(),
        transport.clone(),
    )?;
    assert_eq!(execution.post(&call(), &request()).await?.status, 200);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
    assert_eq!(execution.snapshot().attempts, 3);
    Ok(())
}

#[tokio::test]
async fn cancellation_during_network_releases_stage_without_a_retry() -> Result<()> {
    use std::future::Future;
    use std::task::Poll;
    let policy = remote_settings::standalone()?.into_settings();
    let transport = Arc::new(Scripted {
        statuses: Mutex::new([200].into()),
        calls: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(0),
    });
    let execution = RemoteExecution::with_transport(
        &policy,
        docsbase_memory::daemon::settings::standalone()?.settings(),
        transport.clone(),
    )?;
    let call = call();
    let request = request();
    let mut work = Box::pin(execution.post(&call, &request));
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    call.operation().cancellation().cancel();
    assert!(work.await.is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution.snapshot().active, 0);
    Ok(())
}

#[tokio::test]
async fn operational_transport_drop_in_async_context_is_safe() {
    let transport = ReqwestTransport::new().expect("construct runtime-free HTTP transport");
    drop(transport);
}
