//! Actual dispatch gates; the synchronous entry exists only to expose the old hostage.
use super::*;
use crate::hybrid::remote::{
    AsyncEmbedder, AsyncTransport, RemoteCall, RemoteExecution, StageFuture, WireRequest,
};
use crate::hybrid::wire::TransportResponse;
use crate::vector_cache::Embedder;

struct WireGate {
    calls: AtomicUsize,
    timeouts: Mutex<Vec<Duration>>,
    status: AtomicUsize,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}
fn wire_gate() -> Arc<WireGate> {
    Arc::new(WireGate {
        calls: AtomicUsize::new(0),
        timeouts: Mutex::new(Vec::new()),
        status: AtomicUsize::new(200),
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
    })
}
fn remote_fixture(transport: &Arc<WireGate>) -> Arc<RemoteFixture> {
    let mut policy = crate::hybrid::remote_settings::standalone()
        .unwrap()
        .into_settings();
    policy.retry_attempts = 1;
    Arc::new(RemoteFixture {
        execution: RemoteExecution::with_transport(
            &policy,
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap(),
        legacy: tokio::runtime::Handle::current(),
        sync_calls: AtomicUsize::new(0),
    })
}
fn start_actual(
    fixture: &search_phase_tests::Fixture,
    operation: crate::search::operation::SearchOperation,
) -> tokio::task::JoinHandle<Result<Value>> {
    let shared = fixture.shared.clone();
    let session = fixture.session;
    tokio::spawn(async move {
        call_tool_in(
            &shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets"}),
            Some(operation),
            None,
        )
        .await
    })
}
async fn joins_observed(receiver: std::sync::mpsc::Receiver<()>, count: usize) {
    tokio::task::spawn_blocking(move || {
        for _ in 0..count {
            receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    })
    .await
    .unwrap();
}
impl AsyncTransport for WireGate {
    fn post_json<'a>(
        &'a self,
        _: &'a WireRequest,
        timeout: Duration,
    ) -> StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.timeouts.lock().unwrap().push(timeout);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Ok(TransportResponse {
                status: u16::try_from(self.status.load(Ordering::SeqCst)).unwrap(),
                body: "{}".into(),
                retry_after: None,
            })
        })
    }
}

struct RemoteFixture {
    execution: RemoteExecution,
    legacy: tokio::runtime::Handle,
    sync_calls: AtomicUsize,
}
impl Embedder for RemoteFixture {
    fn model_id(&self) -> &'static str {
        "test/remote-dispatch@d3@l2"
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(vec![])
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        let policy = crate::search::settings::standalone().unwrap();
        let call = RemoteCall::new(
            crate::search::operation::SearchOperation::new(
                policy.settings(),
                Instant::now(),
                crate::hybrid::work::Cancellation::new(),
            )
            .unwrap(),
        );
        self.legacy
            .block_on(AsyncEmbedder::embed_query(self, Arc::from(text), &call))
    }
    fn async_remote(&self) -> Option<&dyn AsyncEmbedder> {
        Some(self)
    }
}
impl AsyncEmbedder for RemoteFixture {
    fn embed<'a>(&'a self, _: Arc<[String]>, _: &'a RemoteCall) -> StageFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn embed_query<'a>(&'a self, _: Arc<str>, call: &'a RemoteCall) -> StageFuture<'a, Vec<f32>> {
        Box::pin(async move {
            self.execution
                .post(
                    call,
                    &WireRequest {
                        url: "https://openrouter.ai/api/v1/embeddings".into(),
                        headers: vec![],
                        body: "{}".into(),
                    },
                )
                .await?;
            Ok(vec![1.0, 0.0, 0.0])
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_query_wait_releases_search_capacity_for_actual_lexical_dispatch() {
    let transport = wire_gate();
    let remote = Arc::new(RemoteFixture {
        execution: RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap(),
        legacy: tokio::runtime::Handle::current(),
        sync_calls: AtomicUsize::new(0),
    });
    let mut fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let shared = Arc::get_mut(&mut fixture.shared).unwrap();
    let mut policy = shared.global_config.daemon.clone();
    policy.search_active = 1;
    shared.workers = Arc::new(
        crate::daemon::worker_admission::WorkerAdmission::with_policies(
            &policy,
            &shared.global_config.remote,
        )
        .unwrap(),
    );
    let shared = fixture.shared.clone();
    let session = fixture.session;
    let first = tokio::spawn(async move {
        call_tool(
            &shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets"}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), transport.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    // Publish a lexical revision while the first request retains its remote stage.
    let slot = fixture.shared.project_slot(fixture.project.id);
    {
        let mut state = slot.state.lock().unwrap();
        state.config = Some(Arc::new(Config::default()));
        state.stages = Some(Arc::new(HybridStages::default()));
    }
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        call_tool(
            &fixture.shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets", "rerank":false}),
        ),
    )
    .await;
    // Always release provider work before assertions/runtime teardown in the RED path.
    transport.release.add_permits(1);
    let _first = first.await.unwrap();
    let lexical = second
        .expect("lexical dispatch must finish while the first HTTP attempt is held")
        .unwrap();
    assert_eq!(lexical.as_array().unwrap().len(), 1);
    assert_eq!(
        remote.sync_calls.load(Ordering::SeqCst),
        0,
        "async dispatch must never enter a live synchronous bridge"
    );
}

struct RemoteReranker {
    execution: RemoteExecution,
    sync_calls: AtomicUsize,
}
impl crate::hybrid::Reranker for RemoteReranker {
    fn async_remote(&self) -> Option<&dyn crate::hybrid::remote::AsyncReranker> {
        Some(self)
    }
    fn rerank(&self, _: &str, _: &[String], _: usize) -> Result<Vec<(usize, f32)>> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::internal(
            "operational remote rerank must use async capability",
        ))
    }
}
impl crate::hybrid::remote::AsyncReranker for RemoteReranker {
    fn rerank<'a>(
        &'a self,
        _: Arc<str>,
        documents: Arc<[String]>,
        top_n: usize,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<(usize, f32)>> {
        Box::pin(async move {
            assert_eq!(documents.len(), 1);
            assert_eq!(top_n, 1);
            assert!(documents[0].contains("old widgets"));
            self.execution
                .post(
                    call,
                    &WireRequest {
                        url: "https://openrouter.ai/api/v1/rerank".into(),
                        headers: vec![],
                        body: "{}".into(),
                    },
                )
                .await?;
            Ok(vec![(0, 1.0)])
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_rerank_finishes_owned_snapshot_after_writer_and_runtime_reload() {
    let transport = wire_gate();
    let reranker = Arc::new(RemoteReranker {
        execution: RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap(),
        sync_calls: AtomicUsize::new(0),
    });
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        reranker: Some(reranker.clone()),
        ..HybridStages::default()
    });
    let shared = fixture.shared.clone();
    let session = fixture.session;
    let mut work = tokio::spawn(async move {
        call_tool(
            &shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets", "snippet":true}),
        )
        .await
    });
    let completed = tokio::select! {
        result = &mut work => Some(result.unwrap()),
        permit = transport.entered.acquire() => { permit.unwrap().forget(); None }
    };
    let held = completed.is_none();
    if held {
        assert!(fixture.shared.db.try_lock().is_ok());
        assert!(fixture.shared.readers.try_acquire().is_ok());
        let slot = fixture.shared.project_slot(fixture.project.id);
        assert!(slot.preparation.try_write().is_ok());
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Guide\n\nnew widgets text\n",
        )
        .unwrap();
        tools::run_project_index(
            &mut fixture.shared.db(),
            &fixture.shared.cache,
            &fixture.project,
            &Config::default(),
        )
        .unwrap();
        let mut state = slot.state.lock().unwrap();
        state.config = Some(Arc::new(Config::default()));
        state.stages = Some(Arc::new(HybridStages::default()));
        state.search_stages = None;
    }
    transport.release.add_permits(1);
    let result = match completed {
        Some(result) => result,
        None => work.await.unwrap(),
    };
    assert!(
        held,
        "actual dispatch completed through the synchronous rerank entry instead of awaiting async HTTP"
    );
    let result = result.unwrap();
    assert!(result.to_string().contains("old widgets"));
    assert!(!result.to_string().contains("new widgets"));
    assert_eq!(reranker.sync_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_actual_remote_misses_share_one_flight_despite_initiator_and_follower_abort() {
    let transport = wire_gate();
    let remote = Arc::new(RemoteFixture {
        execution: RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap(),
        legacy: tokio::runtime::Handle::current(),
        sync_calls: AtomicUsize::new(0),
    });
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let (joined, joins) = std::sync::mpsc::channel();
    fixture.shared.dense.observe_query_joins(joined);
    let start = || {
        let shared = fixture.shared.clone();
        let session = fixture.session;
        tokio::spawn(async move {
            call_tool(
                &shared,
                Some(session),
                "search_docs",
                json!({"query":"widgets"}),
            )
            .await
        })
    };
    let first = start();
    tokio::time::timeout(Duration::from_secs(5), transport.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let second = start();
    let follower = start();
    let fourth = start();
    tokio::task::spawn_blocking(move || {
        for _ in 0..4 {
            joins.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    })
    .await
    .unwrap();
    first.abort();
    follower.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(follower.await.unwrap_err().is_cancelled());
    assert_eq!(
        transport.entered.available_permits(),
        0,
        "no duplicate provider attempts"
    );
    transport.release.add_permits(1);
    assert!(second.await.unwrap().is_ok());
    assert!(fourth.await.unwrap().is_ok());
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 1);
    assert_eq!(fixture.shared.dense.query_cache_stats().misses, 4);
    assert_eq!(remote.sync_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_member_deadline_does_not_poison_later_peer_or_restart_same_http_attempt() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let now = Instant::now();
    let early = crate::search::operation::SearchOperation::constrained(
        &fixture.shared.global_config.search,
        now,
        now + Duration::from_secs(1),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    fixture.shared.dense.observe_query_joins(sender);
    let first = start_actual(&fixture, early);
    transport.entered.acquire().await.unwrap().forget();
    let later = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    joins_observed(receiver, 2).await;
    assert!(first.await.unwrap().is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(
        transport.timeouts.lock().unwrap()[0] > Duration::from_secs(1),
        "fixed HTTP timeout must use shared hard cap, not first member budget"
    );
    transport.release.add_permits(1);
    assert!(later.await.unwrap().is_ok());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_revision_success_never_publishes_into_current_query_cache() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    fixture
        .shared
        .open_project_config_with(&fixture.project, |_| {
            Ok(HybridStages {
                embedder: Some(remote.clone()),
                ranking: Ranking::Rrf,
                reranker: None,
            })
        })
        .unwrap();
    transport.release.add_permits(1);
    let error = first.await.unwrap().unwrap_err();
    assert!(error.inner_message().contains("configuration changed"));
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 0);
    let next = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    transport.release.add_permits(1);
    assert!(next.await.unwrap().is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_runtime_revisions_have_distinct_active_query_flights() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    fixture
        .shared
        .open_project_config_with(&fixture.project, |_| {
            Ok(HybridStages {
                embedder: Some(remote.clone()),
                ranking: Ranking::Rrf,
                reranker: None,
            })
        })
        .unwrap();
    let next = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), transport.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    transport.release.add_permits(2);
    assert!(first.await.unwrap().is_err());
    assert!(next.await.unwrap().is_ok());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_remote_waiter_departure_allows_late_request_without_old_leader_removal() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let next = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), transport.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    transport.release.add_permits(1);
    assert!(next.await.unwrap().is_ok());
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_remote_failure_is_not_a_success_cache_or_stale_flight() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    transport.status.store(400, Ordering::SeqCst);
    transport.release.add_permits(1);
    assert!(first.await.unwrap().is_err());
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 0);
    transport.status.store(200, Ordering::SeqCst);
    let next = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    transport.release.add_permits(1);
    assert!(next.await.unwrap().is_ok());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_change_while_embedding_is_held_returns_retry_before_payload_capture() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    assert!(fixture.shared.readers.try_acquire().is_ok());
    let slot = fixture.shared.project_slot(fixture.project.id);
    assert!(slot.preparation.try_write().is_ok());
    std::fs::write(
        fixture.project.canonical_root.join("guide.md"),
        "# Guide\n\nnew widgets generation\n",
    )
    .unwrap();
    tools::run_project_index(
        &mut fixture.shared.db(),
        &fixture.shared.cache,
        &fixture.project,
        &Config::default(),
    )
    .unwrap();
    transport.release.add_permits(1);
    let error = first.await.unwrap().unwrap_err();
    assert!(error.inner_message().contains("generation changed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_cpu_result_keeps_payload_charge_until_returned_data_is_destroyed() {
    struct Output {
        budget: crate::search::budget::PayloadBudget,
        destroyed: Option<tokio::sync::oneshot::Sender<bool>>,
    }
    impl Drop for Output {
        fn drop(&mut self) {
            let charged = self.budget.reserve(1).is_err();
            let _ = self.destroyed.take().unwrap().send(charged);
        }
    }
    let mut settings = crate::search::settings::standalone()
        .unwrap()
        .into_settings();
    settings.max_request_payload_bytes = 4096;
    settings.max_retained_payload_bytes = 4096;
    let budget = crate::search::budget::PayloadBudget::new(settings).unwrap();
    let call = RemoteCall::new(
        crate::search::operation::SearchOperation::new(
            crate::search::settings::standalone().unwrap().settings(),
            Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )
        .unwrap(),
    )
    .with_payload_reservation(Arc::new(budget.reserve(4096).unwrap()));
    let execution = Arc::new(
        RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            wire_gate(),
        )
        .unwrap(),
    );
    let owner = execution.clone();
    let probe = budget.clone();
    let (release, gate) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (destroyed, observe) = tokio::sync::oneshot::channel();
    let work = tokio::spawn(async move {
        owner
            .cpu(&call, move || {
                let _ = started.send(());
                gate.recv().unwrap();
                Ok(Output {
                    budget: probe,
                    destroyed: Some(destroyed),
                })
            })
            .await
    });
    ready.await.unwrap();
    work.abort();
    assert!(work.await.is_err_and(|error| error.is_cancelled()));
    release.send(()).unwrap();
    let charged = tokio::time::timeout(Duration::from_secs(5), observe)
        .await
        .unwrap()
        .unwrap();
    execution.drain(Duration::from_millis(1)).await;
    assert!(
        charged,
        "abandoned CPU output outlived its input byte reservation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_flight_followers_wait_async_and_running_inference_keeps_physical_charge() {
    let (release, gate) = std::sync::mpsc::channel();
    let (entered, ready) = std::sync::mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let local = Arc::new(search_phase_tests::TestEmbedder {
        entered: Some(entered),
        release: Mutex::new(gate),
        calls: calls.clone(),
    });
    let mut fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(local),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let shared = Arc::get_mut(&mut fixture.shared).unwrap();
    let mut policy = shared.global_config.daemon.clone();
    policy.search_active = 2;
    shared.workers = Arc::new(
        crate::daemon::worker_admission::WorkerAdmission::with_policies(
            &policy,
            &shared.global_config.remote,
        )
        .unwrap(),
    );
    let (joined, joins) = std::sync::mpsc::channel();
    fixture.shared.dense.observe_query_joins(joined);
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    let second = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    let third = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    let fourth = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    joins_observed(joins, 4).await;
    first.abort();
    second.abort();
    third.abort();
    fourth.abort();
    for work in [first, second, third, fourth] {
        assert!(work.await.unwrap_err().is_cancelled());
    }
    let probe = search_operation(&fixture.shared, "search_docs")
        .unwrap()
        .unwrap();
    let spare = fixture.shared.workers.search(&probe).await.unwrap();
    let mut another = Box::pin(fixture.shared.workers.search(&probe));
    std::future::poll_fn(|cx| {
        assert!(
            another.as_mut().poll(cx).is_pending(),
            "running local inference must retain one of the two CPU permits"
        );
        std::task::Poll::Ready(())
    })
    .await;
    drop(another);
    drop(spare);
    release.send(()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.shared.activity.drain(Duration::from_millis(1)),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let first = fixture.shared.workers.search(&probe).await.unwrap();
    let second = fixture.shared.workers.search(&probe).await.unwrap();
    drop((first, second));
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_or_expired_queued_search_starts_no_phase_or_provider() {
    use std::future::Future;
    for expiry in [false, true] {
        let transport = wire_gate();
        let remote = remote_fixture(&transport);
        let mut fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(remote),
            ranking: Ranking::Rrf,
            reranker: None,
        });
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut policy = shared.global_config.daemon.clone();
        policy.search_active = 1;
        shared.workers = Arc::new(
            crate::daemon::worker_admission::WorkerAdmission::with_policies(
                &policy,
                &shared.global_config.remote,
            )
            .unwrap(),
        );
        let occupied_op = search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap();
        let permit = fixture.shared.workers.search(&occupied_op).await.unwrap();
        let (release, gate) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let occupied = request::spawn(
            Some(permit),
            Some(fixture.shared.activity.track()),
            move || {
                let _ = started.send(());
                gate.recv().unwrap();
                Ok(())
            },
        );
        ready.await.unwrap();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(if expiry { 1 } else { 30 });
        let op = crate::search::operation::SearchOperation::constrained(
            &fixture.shared.global_config.search,
            now,
            deadline,
            crate::hybrid::work::Cancellation::new(),
        )
        .unwrap();
        let mut work = Box::pin(call_tool_in(
            &fixture.shared,
            Some(fixture.session),
            "search_docs",
            json!({"query":"widgets"}),
            Some(op.clone()),
            None,
        ));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        if !expiry {
            op.cancellation().cancel();
        }
        let error = tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap_err();
        release.send(()).unwrap();
        occupied.await.unwrap().unwrap();
        assert!(
            error
                .inner_message()
                .contains(if expiry { "deadline" } else { "cancelled" })
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.shared.dense.query_cache_stats().misses, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_request_deadline_spans_embedding_preparation_and_remote_rerank() {
    let embedding = wire_gate();
    let remote = remote_fixture(&embedding);
    let rerank = wire_gate();
    let reranker = Arc::new(RemoteReranker {
        execution: RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            rerank.clone(),
        )
        .unwrap(),
        sync_calls: AtomicUsize::new(0),
    });
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote.clone()),
        ranking: Ranking::Rrf,
        reranker: Some(reranker.clone()),
    });
    let now = Instant::now();
    let op = crate::search::operation::SearchOperation::constrained(
        &fixture.shared.global_config.search,
        now,
        now + Duration::from_secs(1),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let work = start_actual(&fixture, op);
    embedding.entered.acquire().await.unwrap().forget();
    embedding.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), rerank.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let error = tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.inner_message().contains("deadline"));
    assert_eq!(embedding.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rerank.calls.load(Ordering::SeqCst), 1);
    assert_eq!(remote.sync_calls.load(Ordering::SeqCst), 0);
    assert_eq!(reranker.sync_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_rerank_retained_peak_refuses_before_clone_or_provider() {
    let transport = wire_gate();
    let reranker = Arc::new(RemoteReranker {
        execution: RemoteExecution::with_transport(
            crate::hybrid::remote_settings::standalone()
                .unwrap()
                .settings(),
            crate::daemon::settings::standalone().unwrap().settings(),
            transport.clone(),
        )
        .unwrap(),
        sync_calls: AtomicUsize::new(0),
    });
    let mut fixture = search_phase_tests::Fixture::new(HybridStages {
        reranker: Some(reranker.clone()),
        ..HybridStages::default()
    });
    std::fs::write(
        fixture.project.canonical_root.join("guide.md"),
        format!("# Guide\\n\\nold widgets {}", "x".repeat(500)),
    )
    .unwrap();
    tools::run_project_index(
        &mut fixture.shared.db(),
        &fixture.shared.cache,
        &fixture.project,
        &Config::default(),
    )
    .unwrap();
    let shared = Arc::get_mut(&mut fixture.shared).unwrap();
    let mut settings = shared.global_config.search.clone();
    settings.max_request_payload_bytes = 2048;
    settings.max_retained_payload_bytes = 2048;
    shared.payload_budget = crate::search::budget::PayloadBudget::new(settings).unwrap();
    let error = call_tool(
        &fixture.shared,
        Some(fixture.session),
        "search_docs",
        json!({"query":"widgets"}),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .inner_message()
            .contains("remote rerank input exceeds")
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(reranker.sync_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.shared.payload_budget.reserve(2048).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stage_revision_is_checked_before_query_cache_publication() {
    let transport = wire_gate();
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let first = start_actual(
        &fixture,
        search_operation(&fixture.shared, "search_docs")
            .unwrap()
            .unwrap(),
    );
    transport.entered.acquire().await.unwrap().forget();
    fixture
        .shared
        .project_slot(fixture.project.id)
        .state
        .lock()
        .unwrap()
        .stages = Some(Arc::new(HybridStages::default()));
    transport.release.add_permits(1);
    let error = first.await.unwrap().unwrap_err();
    assert!(error.inner_message().contains("runtime changed"));
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_only_rig_stages_run_through_actual_async_search_without_sync_bridge() {
    use crate::hybrid::openrouter::{
        BASE_URL, OpenRouterClient, OpenRouterEmbedder, OpenRouterReranker,
    };
    struct Codec(Mutex<Vec<Value>>);
    impl AsyncTransport for Codec {
        fn post_json<'a>(
            &'a self,
            request: &'a WireRequest,
            _: Duration,
        ) -> StageFuture<'a, TransportResponse> {
            Box::pin(async move {
                let body: Value = serde_json::from_str(&request.body).unwrap();
                self.0.lock().unwrap().push(body);
                let body = if request.url.ends_with("embeddings") {
                    json!({"data":[{"index":0,"embedding":[3.0,0.0,0.0]}]})
                } else {
                    json!({"results":[{"index":0,"relevance_score":0.8}]})
                };
                Ok(TransportResponse {
                    status: 200,
                    body: body.to_string(),
                    retry_after: None,
                })
            })
        }
    }
    let fixture = search_phase_tests::Fixture::new(HybridStages::default());
    let transport = Arc::new(Codec(Mutex::new(Vec::new())));
    let execution = Arc::new(
        RemoteExecution::with_owners(
            &fixture.shared.global_config.remote,
            transport.clone(),
            fixture.shared.workers.clone(),
            fixture.shared.activity.clone(),
        )
        .unwrap(),
    );
    let client =
        Arc::new(OpenRouterClient::with_execution(BASE_URL, "fixture-secret", execution).unwrap());
    let embedder = Arc::new(OpenRouterEmbedder::with_client(
        crate::hybrid::EmbeddingCfg {
            model: "fixture".into(),
            dimensions: 3,
            normalize: true,
            ..crate::hybrid::EmbeddingCfg::default()
        },
        client.clone(),
    ));
    let reranker = Arc::new(OpenRouterReranker::with_client(
        crate::hybrid::RerankerCfg::default(),
        client,
    ));
    let stages = Arc::new(HybridStages {
        embedder: Some(embedder),
        ranking: Ranking::Rrf,
        reranker: Some(reranker),
    });
    fixture
        .shared
        .project_slot(fixture.project.id)
        .state
        .lock()
        .unwrap()
        .stages = Some(stages);
    let result = call_tool(
        &fixture.shared,
        Some(fixture.session),
        "search_docs",
        json!({"query":"widgets","explain":true}),
    )
    .await
    .unwrap();
    assert!(!result.to_string().contains("fixture-secret"));
    let requests = transport.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["input_type"], "search_query");
    assert_eq!(requests[1]["top_n"], 1);
    assert!(
        requests[1]["documents"][0]
            .as_str()
            .unwrap()
            .contains("old widgets")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_document_scope_never_starts_query_embedding() {
    let transport = wire_gate();
    transport.release.add_permits(1);
    let remote = remote_fixture(&transport);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(remote),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let result = call_tool(
        &fixture.shared,
        Some(fixture.session),
        "search_docs",
        json!({"query":"widgets","path_prefix":"missing/"}),
    )
    .await
    .unwrap();
    assert_eq!(result.as_array().unwrap().len(), 0);
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        0,
        "empty scoped corpus must preserve the existing no-provider early exit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_remote_codec_retains_original_connection_until_physical_completion() {
    use crate::hybrid::openrouter::{BASE_URL, OpenRouterClient};
    for embedding in [false, true] {
        for unwind in [false, true] {
            let fixture = search_phase_tests::Fixture::new(HybridStages::default());
            let transport = wire_gate();
            let mut execution = RemoteExecution::with_owners(
                &fixture.shared.global_config.remote,
                transport.clone(),
                fixture.shared.workers.clone(),
                fixture.shared.activity.clone(),
            )
            .unwrap();
            let (entered, observed) = tokio::sync::oneshot::channel();
            let entered = Mutex::new(Some(entered));
            let (release, proceed) = std::sync::mpsc::channel();
            let proceed = Mutex::new(proceed);
            execution.observe_cpu(Arc::new(move || {
                entered.lock().unwrap().take().unwrap().send(()).unwrap();
                proceed.lock().unwrap().recv().unwrap();
                assert!(!unwind, "controlled actual codec CPU unwind");
            }));
            let execution = Arc::new(execution);
            let client = Arc::new(
                OpenRouterClient::with_execution(BASE_URL, "fixture-secret", execution.clone())
                    .unwrap(),
            );
            let stages = codec_stages(client, embedding);
            fixture
                .shared
                .project_slot(fixture.project.id)
                .state
                .lock()
                .unwrap()
                .stages = Some(Arc::new(stages));
            let slots = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = Arc::new(slots.clone().try_acquire_owned().unwrap());
            let scope = scoped::Scope {
                activity: Activity::new(
                    &fixture.shared.activity.counter,
                    &fixture.shared.activity.events,
                    Some(&permit),
                ),
                operation: search_operation(&fixture.shared, "search_docs").unwrap(),
            };
            drop(permit);
            let mut peer = Peer {
                hello_done: true,
                session_id: Some(fixture.session),
                cancel_rx: None,
            };
            let (mut client, server) = tokio::io::duplex(1024);
            let mut reader = FrameReader::new(BufReader::new(server));
            let mut request = Box::pin(scoped::call(
                &fixture.shared,
                &mut peer,
                json!({"query":"widgets", "rerank":!embedding}),
                &scope,
                &mut reader,
            ));
            tokio::select! {
                result = &mut request => panic!("search completed before codec CPU gate: {result:?}"),
                result = observed => result.unwrap(),
            }
            client.shutdown().await.unwrap();
            let (response, close) = request.as_mut().await;
            assert!(matches!(response, Response::Error { .. }));
            assert!(close);
            assert!(
                scope
                    .operation
                    .as_ref()
                    .unwrap()
                    .cancellation()
                    .is_cancelled()
            );
            drop(request);
            drop(scope);
            // A shared embedding leader must also have observed last departure.
            tokio::time::timeout(Duration::from_secs(5), async {
                while execution.snapshot().active != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let retained = slots.clone().try_acquire_owned().is_err();
            let no_http = transport.calls.load(Ordering::SeqCst) == 0;
            // Cleanup precedes the RED assertion, including controlled unwind.
            release.send(()).unwrap();
            let retired = tokio::time::timeout(Duration::from_secs(5), slots.acquire())
                .await
                .unwrap()
                .unwrap();
            assert!(
                retained,
                "original connection freed during physical codec work"
            );
            assert!(no_http);
            drop(retired);
        }
    }
}

fn codec_stages(
    client: Arc<crate::hybrid::openrouter::OpenRouterClient>,
    embedding: bool,
) -> HybridStages {
    use crate::hybrid::openrouter::{OpenRouterEmbedder, OpenRouterReranker};
    if embedding {
        HybridStages {
            embedder: Some(Arc::new(OpenRouterEmbedder::with_client(
                crate::hybrid::EmbeddingCfg {
                    model: "fixture".into(),
                    dimensions: 3,
                    normalize: true,
                    ..crate::hybrid::EmbeddingCfg::default()
                },
                client,
            ))),
            ranking: Ranking::Rrf,
            reranker: None,
        }
    } else {
        HybridStages {
            reranker: Some(Arc::new(OpenRouterReranker::with_client(
                crate::hybrid::RerankerCfg::default(),
                client,
            ))),
            ..HybridStages::default()
        }
    }
}

#[tokio::test]
async fn reloaded_flight_is_last_real_client_owner_and_destroys_it_in_async_waiter() {
    use crate::hybrid::dense::{QueryEmbeddingRequest, RuntimeRevision};
    use crate::hybrid::openrouter::{BASE_URL, OpenRouterClient};
    let fixture = search_phase_tests::Fixture::new(HybridStages::default());
    let execution = Arc::new(
        RemoteExecution::real_with_owners(
            &fixture.shared.global_config.remote,
            fixture.shared.workers.clone(),
            fixture.shared.activity.clone(),
        )
        .unwrap(),
    );
    let retired_execution = Arc::downgrade(&execution);
    let client =
        Arc::new(OpenRouterClient::with_execution(BASE_URL, "fixture-secret", execution).unwrap());
    let retired_client = Arc::downgrade(&client);
    let stages = Arc::new(codec_stages(client, true));
    let retired_stages = Arc::downgrade(&stages);
    let slot = fixture.shared.project_slot(fixture.project.id);
    let config = slot.state.lock().unwrap().config.clone().unwrap();
    slot.state.lock().unwrap().search_stages = Some(stages.clone());
    let operation = search_operation(&fixture.shared, "search_docs")
        .unwrap()
        .unwrap();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = release.clone();
    let mut work = Box::pin(fixture.shared.dense.query_embedding_async(
        QueryEmbeddingRequest {
            model: "openrouter/fixture@d3@l2",
            text: "reload lifetime",
            revision: RuntimeRevision::Captured {
                config: config.clone(),
                stages,
            },
            operation: &operation,
            policy: &fixture.shared.global_config.search,
        },
        |publication| {
            std::future::ready(publication.complete(&operation, || slot.preparation_lease(&config)))
        },
        move |_| async move {
            entered.send(()).unwrap();
            gate.acquire().await.unwrap().forget();
            Ok(vec![1.0, 0.0, 0.0])
        },
    ));
    tokio::select! {
        result = &mut work => panic!("flight completed before reload: {result:?}"),
        result = observed => result.unwrap(),
    }
    fixture
        .shared
        .open_project_config_with(&fixture.project, |_| Ok(HybridStages::default()))
        .unwrap();
    // No external client/execution Arc remains; only the old flight's retained
    // stage revision owns the real reqwest client. No HTTP or DNS is requested.
    assert_eq!(retired_client.strong_count(), 1);
    assert_eq!(retired_execution.strong_count(), 1);
    assert_eq!(retired_execution.upgrade().unwrap().snapshot().attempts, 0);
    release.add_permits(1);
    // This current-thread runtime completes the leader's poll before scheduling
    // this waiter. Its final key/membership Drop therefore destroys the actual
    // client in this awaited future: a runtime-Drop panic fails this test.
    assert_eq!(&*work.await.unwrap(), &[1.0, 0.0, 0.0]);
    assert!(retired_stages.upgrade().is_none());
    assert!(retired_client.upgrade().is_none());
    assert!(retired_execution.upgrade().is_none());
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 0);
}

#[test]
fn publication_contention_keeps_single_thread_runtime_and_original_budget_live() {
    for expiry in [false, true] {
        publication_contention_case(expiry);
    }
}

fn publication_contention_case(expiry: bool) {
    let (attempted, attempt) = std::sync::mpsc::channel();
    let (progress, progressed) = std::sync::mpsc::channel();
    let (returned, result) = std::sync::mpsc::channel();
    let (configured, configuration) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let (go, begin) = tokio::sync::oneshot::channel();
    let warmed = Arc::new(AtomicUsize::new(usize::MAX));
    let observed_cache = warmed.clone();
    let runtime = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let transport = wire_gate();
            let fixture = search_phase_tests::Fixture::new(HybridStages {
                embedder: Some(remote_fixture(&transport)),
                ranking: Ranking::Rrf,
                reranker: None,
            });
            let mut policy = fixture.shared.global_config.search.clone();
            if expiry {
                policy.request_timeout_ms = 1_000;
            }
            let operation = crate::search::operation::SearchOperation::new(
                &policy,
                Instant::now(),
                crate::hybrid::work::Cancellation::new(),
            )
            .unwrap();
            configured.send(operation.cancellation().clone()).unwrap();
            let search = start_actual(&fixture, operation);
            transport.entered.acquire().await.unwrap().forget();
            let slot = fixture.shared.project_slot(fixture.project.id);
            *slot.preparation_observer.lock().unwrap() = Some(attempted);
            let (held, acquired) = std::sync::mpsc::channel();
            let holder = std::thread::spawn(move || {
                let _publication = slot.preparation.write().unwrap();
                held.send(()).unwrap();
                proceed.recv().unwrap();
            });
            acquired.recv_timeout(Duration::from_secs(5)).unwrap();
            transport.release.add_permits(1);
            begin.await.unwrap();
            let control = call_tool(&fixture.shared, None, "list_projects", json!({})).await;
            progress.send(control.is_ok()).unwrap();
            returned
                .send(search.await.unwrap().unwrap_err().inner_message().clone())
                .unwrap();
            holder.join().unwrap();
            fixture
                .shared
                .activity
                .drain(Duration::from_millis(1))
                .await;
            observed_cache.store(
                fixture.shared.dense.query_cache_stats().entries,
                Ordering::SeqCst,
            );
        });
    });
    let cancellation = configuration.recv_timeout(Duration::from_secs(5)).unwrap();
    let entered = attempt.recv_timeout(Duration::from_secs(5)).is_ok();
    go.send(()).unwrap();
    let control = progressed
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or(false);
    if !expiry {
        cancellation.cancel();
    }
    let outcome = result.recv_timeout(Duration::from_secs(5));
    // Release the actual project publication writer before every RED assertion.
    release.send(()).unwrap();
    runtime.join().unwrap();
    assert_eq!(
        warmed.load(Ordering::SeqCst),
        0,
        "cancelled publication warmed cache after writer release"
    );
    assert!(entered);
    assert!(
        control,
        "publication lock blocked independent Tokio/control progress"
    );
    assert!(
        outcome
            .unwrap()
            .contains(if expiry { "deadline" } else { "cancelled" })
    );
}
