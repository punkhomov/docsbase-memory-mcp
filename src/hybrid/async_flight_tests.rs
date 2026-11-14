use super::*;
use crate::search::operation::SearchOperation;
use crate::search::settings::{SearchSettings, standalone};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

struct Gate {
    entered: AtomicUsize,
    ready: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            entered: AtomicUsize::new(0),
            ready: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
struct Finished(Arc<tokio::sync::Semaphore>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.add_permits(1);
    }
}

fn operation() -> SearchOperation {
    SearchOperation::new(
        standalone().unwrap().settings(),
        Instant::now(),
        Cancellation::new(),
    )
    .unwrap()
}
async fn join(
    registry: Arc<Flights<u64, usize>>,
    key: u64,
    operation: SearchOperation,
    policy: SearchSettings,
    gate: Arc<Gate>,
) -> Result<usize> {
    registry
        .run_async(
            key,
            &operation,
            &policy,
            || None,
            |value| std::future::ready(Ok(value)),
            move |call| async move {
                gate.entered.fetch_add(1, Ordering::SeqCst);
                gate.ready.add_permits(1);
                call.wait(&Cancellation::new(), async {
                    gate.release.acquire().await.unwrap().forget();
                    Ok(7)
                })
                .await
            },
        )
        .await
}
async fn poll_pending<F: Future>(work: std::pin::Pin<&mut F>) {
    let mut work = work;
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn async_flight_initiator_drop_keeps_peer_and_one_owned_leader() {
    let registry = Arc::new(Flights::default());
    let gate = Arc::new(Gate::default());
    let policy = standalone().unwrap().into_settings();
    let mut first = Box::pin(join(
        registry.clone(),
        1,
        operation(),
        policy.clone(),
        gate.clone(),
    ));
    let mut peer = Box::pin(join(registry.clone(), 1, operation(), policy, gate.clone()));
    poll_pending(first.as_mut()).await;
    poll_pending(peer.as_mut()).await;
    gate.ready.acquire().await.unwrap().forget();
    drop(first);
    gate.release.add_permits(1);
    assert_eq!(peer.await.unwrap(), 7);
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
    assert_eq!(registry.active(), 0);
}

#[tokio::test]
async fn async_flight_later_peer_survives_early_member_deadline_on_same_work() {
    let registry = Arc::new(Flights::default());
    let gate = Arc::new(Gate::default());
    let hard = standalone().unwrap().into_settings();
    let mut short = hard.clone();
    short.request_timeout_ms = 1_000;
    let early = SearchOperation::new(&short, Instant::now(), Cancellation::new()).unwrap();
    let mut first = Box::pin(join(registry.clone(), 1, early, hard.clone(), gate.clone()));
    poll_pending(first.as_mut()).await;
    gate.ready.acquire().await.unwrap().forget();
    let mut later = Box::pin(join(registry.clone(), 1, operation(), hard, gate.clone()));
    poll_pending(later.as_mut()).await;
    assert!(first.await.is_err());
    gate.release.add_permits(1);
    assert_eq!(later.await.unwrap(), 7);
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn async_flight_hard_cap_retires_later_member_without_restarting_work() {
    let registry = Arc::new(Flights::default());
    let gate = Arc::new(Gate::default());
    let mut hard = standalone().unwrap().into_settings();
    hard.request_timeout_ms = 1_000;
    let mut first = Box::pin(join(
        registry.clone(),
        1,
        operation(),
        hard.clone(),
        gate.clone(),
    ));
    let mut later = Box::pin(join(registry.clone(), 1, operation(), hard, gate.clone()));
    poll_pending(first.as_mut()).await;
    poll_pending(later.as_mut()).await;
    gate.ready.acquire().await.unwrap().forget();
    assert!(first.await.is_err());
    assert!(later.await.is_err());
    assert_eq!(registry.active(), 0);
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn async_flight_publication_at_selection_registration_and_wait_cannot_lose_wake() {
    for boundary in [
        Boundary::Selected,
        Boundary::Registered,
        Boundary::BeforeWait,
    ] {
        let registry = Arc::new(Flights::<u64, usize>::default());
        let weak = Arc::downgrade(&registry);
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = fired.clone();
        *registry.boundary_observer.lock().unwrap() = Some(Arc::new(move |current| {
            if std::mem::discriminant(&current) == std::mem::discriminant(&boundary)
                && !signal.swap(true, Ordering::SeqCst)
            {
                let registry = weak.upgrade().unwrap();
                let flight = registry.active.lock().unwrap().get(&1).unwrap().clone();
                registry.complete(&1, &flight, Completion::Finished(Ok(9)));
            }
        }));
        let gate = Arc::new(Gate::default());
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                join(
                    registry.clone(),
                    1,
                    operation(),
                    standalone().unwrap().into_settings(),
                    gate.clone()
                )
            )
            .await
            .unwrap()
            .unwrap(),
            9
        );
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(registry.active(), 0);
        tokio::task::yield_now().await;
        assert_eq!(gate.entered.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn async_flight_failure_and_unwind_wake_every_member_and_allow_fresh_work() {
    for unwind in [false, true] {
        let registry = Arc::new(Flights::<u64, usize>::default());
        let policy = standalone().unwrap().into_settings();
        let first_op = operation();
        let second_op = operation();
        let mut first = Box::pin(registry.run_async(
            1,
            &first_op,
            &policy,
            || None,
            |value| std::future::ready(Ok(value)),
            move |_| async move {
                assert!(!unwind, "controlled async leader unwind");
                Err(Error::internal_with_source(
                    "controlled async failure",
                    std::io::Error::other("retained cause"),
                ))
            },
        ));
        let mut second = Box::pin(registry.run_async(
            1,
            &second_op,
            &policy,
            || None,
            |value| std::future::ready(Ok(value)),
            |_| async { Ok(1) },
        ));
        poll_pending(first.as_mut()).await;
        poll_pending(second.as_mut()).await;
        let error = first.await.unwrap_err();
        let peer = second.await.unwrap_err();
        assert_eq!(error.mcp_code(), peer.mcp_code());
        if !unwind {
            assert!(format!("{error:?}").contains("retained cause"));
        }
        assert_eq!(registry.active(), 0);
        assert_eq!(
            registry
                .run_async(
                    1,
                    &operation(),
                    &policy,
                    || None,
                    |value| std::future::ready(Ok(value)),
                    |_| async { Ok(11) }
                )
                .await
                .unwrap(),
            11
        );
    }
}

#[tokio::test]
async fn async_flight_last_departure_and_late_join_preserve_new_matching_flight() {
    let registry = Arc::new(Flights::default());
    let old = Arc::new(Gate::default());
    let new = Arc::new(Gate::default());
    let policy = standalone().unwrap().into_settings();
    let mut first = Box::pin(join(
        registry.clone(),
        1,
        operation(),
        policy.clone(),
        old.clone(),
    ));
    poll_pending(first.as_mut()).await;
    old.ready.acquire().await.unwrap().forget();
    let next = Arc::new(Mutex::new(Some(Box::pin(join(
        registry.clone(),
        1,
        operation(),
        policy,
        new.clone(),
    )))));
    let slot = next.clone();
    *registry.boundary_observer.lock().unwrap() = Some(Arc::new(move |boundary| {
        if matches!(boundary, Boundary::Inactive) {
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(
                slot.lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .as_mut()
                    .poll(&mut context)
                    .is_pending()
            );
        }
    }));
    drop(first);
    assert_eq!(registry.active(), 1);
    *registry.boundary_observer.lock().unwrap() = None;
    new.ready.acquire().await.unwrap().forget();
    new.release.add_permits(1);
    let next = next.lock().unwrap().take().unwrap();
    assert_eq!(next.await.unwrap(), 7);
    assert_eq!(registry.active(), 0);
}

#[tokio::test]
async fn async_flight_departing_long_peer_shortens_interest_without_new_work() {
    let registry = Arc::new(Flights::default());
    let gate = Arc::new(Gate::default());
    let policy = standalone().unwrap().into_settings();
    let mut short = policy.clone();
    short.request_timeout_ms = 1_000;
    let early = SearchOperation::new(&short, Instant::now(), Cancellation::new()).unwrap();
    let mut first = Box::pin(join(
        registry.clone(),
        1,
        early,
        policy.clone(),
        gate.clone(),
    ));
    let mut long = Box::pin(join(registry.clone(), 1, operation(), policy, gate.clone()));
    poll_pending(first.as_mut()).await;
    poll_pending(long.as_mut()).await;
    gate.ready.acquire().await.unwrap().forget();
    drop(long);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
    assert_eq!(registry.active(), 0);
}

#[tokio::test]
async fn async_query_keys_own_config_and_stages_through_abandonment_and_unwind() {
    use crate::hybrid::dense::{DenseCache, QueryEmbeddingRequest, RuntimeRevision};
    for unwind in [false, true] {
        let cache = DenseCache::new();
        let config = Arc::new(crate::config::Config::default());
        let stages = Arc::new(crate::hybrid::HybridStages::default());
        let weak_config = Arc::downgrade(&config);
        let weak_stages = Arc::downgrade(&stages);
        let ready = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let finished = Arc::new(tokio::sync::Semaphore::new(0));
        let done = Finished(finished.clone());
        let entered = ready.clone();
        let gate = release.clone();
        let operation = operation();
        let policy = standalone().unwrap().into_settings();
        let mut request = Box::pin(cache.query_embedding_async(
            QueryEmbeddingRequest {
                model: "test/key@d3",
                text: "query",
                revision: RuntimeRevision::Captured { config, stages },
                operation: &operation,
                policy: &policy,
            },
            |publication| std::future::ready(publication.complete(&operation, || Ok(()))),
            move |_call| async move {
                let _finished = done;
                entered.add_permits(1);
                gate.acquire().await.unwrap().forget();
                assert!(!unwind, "controlled retained key unwind");
                Ok(vec![1.0, 0.0, 0.0])
            },
        ));
        poll_pending(request.as_mut()).await;
        ready.acquire().await.unwrap().forget();
        drop(request);
        assert!(weak_config.upgrade().is_some());
        assert!(weak_stages.upgrade().is_some());
        release.add_permits(1);
        finished.acquire().await.unwrap().forget();
        assert!(weak_config.upgrade().is_none());
        assert!(weak_stages.upgrade().is_none());
        assert_eq!(cache.query_cache_stats().entries, 0);
    }
}

#[tokio::test]
async fn async_query_different_models_do_not_share_inflight_or_completed_vectors() {
    use crate::hybrid::dense::{DenseCache, QueryEmbeddingRequest, RuntimeRevision};
    let cache = DenseCache::new();
    let first_op = operation();
    let second_op = operation();
    let policy = standalone().unwrap().into_settings();
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let work = || {
        let entered = entered.clone();
        let release = release.clone();
        move |_call| async move {
            entered.fetch_add(1, Ordering::SeqCst);
            release.acquire().await.unwrap().forget();
            Ok(vec![1.0, 0.0, 0.0])
        }
    };
    let mut first = Box::pin(cache.query_embedding_async(
        QueryEmbeddingRequest {
            model: "test/a@d3",
            text: "same",
            revision: RuntimeRevision::Compatibility,
            operation: &first_op,
            policy: &policy,
        },
        |publication| std::future::ready(publication.complete(&first_op, || Ok(()))),
        work(),
    ));
    let mut second = Box::pin(cache.query_embedding_async(
        QueryEmbeddingRequest {
            model: "test/b@d3",
            text: "same",
            revision: RuntimeRevision::Compatibility,
            operation: &second_op,
            policy: &policy,
        },
        |publication| std::future::ready(publication.complete(&second_op, || Ok(()))),
        work(),
    ));
    poll_pending(first.as_mut()).await;
    poll_pending(second.as_mut()).await;
    release.add_permits(2);
    let a = first.await.unwrap();
    let b = second.await.unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(cache.query_cache_stats().entries, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_query_completion_remains_discoverable_until_cache_publication() {
    use crate::hybrid::dense::{DenseCache, QueryEmbeddingRequest, RuntimeRevision};
    let cache = Arc::new(DenseCache::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let config = Arc::new(crate::config::Config::default());
    let stages = Arc::new(crate::hybrid::HybridStages::default());
    let revision = RuntimeRevision::Captured { config, stages };
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, proceed) = tokio::sync::oneshot::channel();
    let owner = cache.clone();
    let count = calls.clone();
    let first_revision = revision.clone();
    let first = tokio::spawn(async move {
        let entered = Mutex::new(Some(entered));
        let proceed = Mutex::new(Some(proceed));
        owner
            .query_embedding_async(
                QueryEmbeddingRequest {
                    model: "test/handoff@d3",
                    text: "same",
                    revision: first_revision,
                    operation: &operation(),
                    policy: &standalone().unwrap().into_settings(),
                },
                |publication| {
                    let entered = entered.lock().unwrap().take().unwrap();
                    let proceed = proceed.lock().unwrap().take().unwrap();
                    async move {
                        entered.send(()).unwrap();
                        proceed.await.unwrap();
                        publication.complete(&operation(), || Ok(()))
                    }
                },
                move |_| async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![1.0, 0.0, 0.0])
                },
            )
            .await
    });
    observed.await.unwrap();
    assert_eq!(cache.query_cache_stats().entries, 0);
    let late = cache
        .query_embedding_async(
            QueryEmbeddingRequest {
                model: "test/handoff@d3",
                text: "same",
                revision,
                operation: &operation(),
                policy: &standalone().unwrap().into_settings(),
            },
            |publication| std::future::ready(publication.complete(&operation(), || Ok(()))),
            {
                let calls = calls.clone();
                move |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![0.0, 1.0, 0.0])
                }
            },
        )
        .await;
    // Always release the original publication gate, including the RED path.
    release.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    let late = late.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&first, &late));
    assert!(Arc::ptr_eq(
        &first,
        &cache.query_vector("test/handoff@d3", "same").unwrap()
    ));
}
