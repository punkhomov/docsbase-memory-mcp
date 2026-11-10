use super::*;
use crate::hybrid::work::Cancellation;
use crate::vector_cache::Embedder;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

const WATCHDOG: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Error,
    SourceError,
    Panic,
}

struct GatedEmbedder {
    id: &'static str,
    calls: AtomicUsize,
    entered: mpsc::Sender<()>,
    proceed: Mutex<mpsc::Receiver<()>>,
    outcome: Outcome,
}

impl Embedder for GatedEmbedder {
    fn model_id(&self) -> &str {
        self.id
    }

    fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::internal("fixture requires query-specific path"))
    }

    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        assert!(crate::hybrid::blocking::in_context());
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        self.entered.send(()).expect("provider entry");
        self.proceed
            .lock()
            .expect("gate")
            .recv_timeout(WATCHDOG)
            .map_err(|err| Error::internal_with_source("provider test gate", err))?;
        match self.outcome {
            Outcome::Success => Ok(vec![1.0, 2.0, 3.0]),
            Outcome::Error => Err(Error::Project {
                message: "gated embedding failure".to_owned(),
                instruction: Some("retry the model".to_owned()),
            }),
            Outcome::SourceError => Err(Error::internal_with_source(
                "gated internal failure",
                std::io::Error::other("fixture I/O cause"),
            )),
            Outcome::Panic => panic!("provider panic"),
        }
    }
}

struct ImmediateEmbedder(AtomicUsize);
impl Embedder for ImmediateEmbedder {
    fn model_id(&self) -> &'static str {
        "test/query@d3@l2"
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::internal("fixture requires query path"))
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        self.0.fetch_add(1, AtomicOrdering::SeqCst);
        Ok(vec![1.0, 2.0, 3.0])
    }
}

type Caught = std::thread::Result<Result<Arc<[f32]>>>;

fn four_requests(outcome: Outcome, cancel: Option<usize>) -> (DenseCache, Vec<Caught>) {
    let cache = DenseCache::new();
    let flights = Arc::clone(&cache.lock_query_cache().flights);
    let (joined, joins) = mpsc::channel();
    flights.observe_joins(joined);
    let other = cache.sharing_query_cache();
    let controls: Vec<_> = (0..4).map(|_| Cancellation::new()).collect();
    let results = std::thread::scope(|scope| {
        // Sender drops before scope joins on assertion unwind, unblocking the
        // fake provider. Extra tokens also let a broken duplicate-call version
        // finish so the call-count assertion can fail without hanging.
        let (release, proceed) = mpsc::channel();
        let (entered, entry) = mpsc::channel();
        let embedder = Arc::new(GatedEmbedder {
            id: "test/query@d3@l2",
            calls: AtomicUsize::new(0),
            entered,
            proceed: Mutex::new(proceed),
            outcome,
        });
        let (done, completed) = mpsc::channel();
        let mut workers = Vec::new();
        for (index, control) in controls.iter().enumerate() {
            let embedder = Arc::clone(&embedder);
            let cache = if index % 2 == 0 { &cache } else { &other };
            let done = done.clone();
            workers.push(scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cache.query_embedding(embedder.as_ref(), "same query", control)
                }));
                assert!(!crate::hybrid::blocking::in_context());
                done.send(index).expect("completion");
                result
            }));
            if index == 0 {
                entry
                    .recv_timeout(WATCHDOG)
                    .expect("leader provider entered");
            }
        }
        for _ in 0..4 {
            joins
                .recv_timeout(WATCHDOG)
                .expect("participant joined flight");
        }
        if let Some(index) = cancel {
            controls[index].cancel();
            if index != 0 {
                assert_eq!(
                    completed
                        .recv_timeout(WATCHDOG)
                        .expect("waiter cancelled before provider release"),
                    index
                );
            }
        }
        for _ in 0..4 {
            let _ = release.send(());
        }
        let results = workers
            .into_iter()
            .map(|worker| worker.join().expect("test worker"))
            .collect();
        assert_eq!(embedder.calls.load(AtomicOrdering::SeqCst), 1);
        results
    });
    assert_eq!(flights.active(), 0, "no retired query flights");
    for control in controls {
        assert_eq!(control.subscriber_count(), 0);
    }
    (cache, results)
}

#[test]
fn simultaneous_query_misses_share_one_provider_result() {
    let (cache, results) = four_requests(Outcome::Success, None);
    let vectors: Vec<_> = results
        .into_iter()
        .map(|r| r.expect("no panic").expect("vector"))
        .collect();
    for vector in &vectors {
        assert!(
            Arc::ptr_eq(&vectors[0], vector),
            "one owned vector, not four copies"
        );
        assert_eq!(vector.as_ref(), &[1.0, 2.0, 3.0]);
    }
    assert_eq!(
        cache.query_cache_stats().misses,
        4,
        "lookup miss semantics retained"
    );
    assert_eq!(cache.query_cache_stats().entries, 1);
}

#[test]
fn query_flight_error_wakes_waiters_without_caching_failure() {
    let (cache, results) = four_requests(Outcome::Error, None);
    for result in results {
        let error = result.expect("no panic").expect_err("provider failed");
        assert_eq!(error.mcp_code(), -32012);
        assert_eq!(error.inner_message(), "gated embedding failure");
        assert_eq!(error.instruction(), Some("retry the model"));
    }
    assert_eq!(cache.query_cache_stats().entries, 0);
    let retry = ImmediateEmbedder(AtomicUsize::new(0));
    assert!(
        cache
            .query_embedding(&retry, "same query", &Cancellation::new())
            .is_ok()
    );
    assert_eq!(retry.0.load(AtomicOrdering::SeqCst), 1);
}

#[test]
fn query_flight_error_retains_the_original_source_chain() {
    let (_, results) = four_requests(Outcome::SourceError, None);
    for result in results {
        let error = result.expect("no panic").expect_err("provider failed");
        assert_eq!(error.mcp_code(), -32603);
        assert_eq!(error.inner_message(), "gated internal failure");
        let mut source = std::error::Error::source(&error);
        let mut found = false;
        while let Some(cause) = source {
            found |= cause.to_string() == "fixture I/O cause";
            source = cause.source();
        }
        assert!(found, "waiter must retain the actual provider cause");
    }
}

#[test]
fn cancelled_query_waiter_leaves_live_participants_running() {
    let (cache, results) = four_requests(Outcome::Success, Some(1));
    for (index, result) in results.into_iter().enumerate() {
        let result = result.expect("no panic");
        if index == 1 {
            assert!(matches!(result, Err(Error::Admission { .. })));
        } else {
            assert!(result.is_ok());
        }
    }
    assert_eq!(cache.query_cache_stats().entries, 1);
}

#[test]
fn cancelled_query_leader_still_delivers_shared_result() {
    let (cache, results) = four_requests(Outcome::Success, Some(0));
    for (index, result) in results.into_iter().enumerate() {
        let result = result.expect("no panic");
        if index == 0 {
            assert!(matches!(result, Err(Error::Admission { .. })));
        } else {
            assert!(result.is_ok());
        }
    }
    assert_eq!(cache.query_cache_stats().entries, 1);
}

#[test]
fn query_leader_unwind_wakes_waiters_and_allows_retry() {
    let (cache, mut results) = four_requests(Outcome::Panic, None);
    assert!(results.remove(0).is_err(), "leader keeps its unwind");
    for result in results {
        assert!(result.expect("waiter did not panic").is_err());
    }
    assert_eq!(cache.query_cache_stats().entries, 0);
    let retry = ImmediateEmbedder(AtomicUsize::new(0));
    assert!(
        cache
            .query_embedding(&retry, "same query", &Cancellation::new())
            .is_ok()
    );
    assert_eq!(retry.0.load(AtomicOrdering::SeqCst), 1);
}

#[test]
fn cancelled_query_never_calls_provider_or_uses_completed_cache() {
    let cache = DenseCache::new();
    let embedder = ImmediateEmbedder(AtomicUsize::new(0));
    cache.store_query_vector(embedder.model_id(), "same query", &[1.0, 2.0, 3.0]);
    let control = Cancellation::new();
    control.cancel();
    assert!(
        cache
            .query_embedding(&embedder, "same query", &control)
            .is_err()
    );
    assert_eq!(embedder.0.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(control.subscriber_count(), 0);
}

#[test]
fn completed_query_vector_is_shared_without_warming_dense_rows() {
    let cache = DenseCache::new();
    let other = cache.sharing_query_cache();
    let embedder = ImmediateEmbedder(AtomicUsize::new(0));
    let first = cache
        .query_embedding(&embedder, "same query", &Cancellation::new())
        .expect("first");
    let second = other
        .query_embedding(&embedder, "same query", &Cancellation::new())
        .expect("second");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(embedder.0.load(AtomicOrdering::SeqCst), 1);
    assert!(other.lock_entries().is_empty());
}

#[test]
fn different_embedding_models_do_not_share_a_flight() {
    let cache = DenseCache::new();
    std::thread::scope(|scope| {
        let (entered, entry) = mpsc::channel();
        let mut providers = Vec::new();
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for id in ["test/model-a@d3@l2", "test/model-b@d3@l2"] {
            let (release, proceed) = mpsc::channel();
            let provider = Arc::new(GatedEmbedder {
                id,
                calls: AtomicUsize::new(0),
                entered: entered.clone(),
                proceed: Mutex::new(proceed),
                outcome: Outcome::Success,
            });
            providers.push(Arc::clone(&provider));
            releases.push(release);
            let cache = &cache;
            workers.push(scope.spawn(move || {
                cache.query_embedding(provider.as_ref(), "same query", &Cancellation::new())
            }));
        }
        for _ in 0..2 {
            entry
                .recv_timeout(WATCHDOG)
                .expect("independent model provider entered");
        }
        for release in releases {
            release.send(()).expect("release model");
        }
        let vectors: Vec<_> = workers
            .into_iter()
            .map(|w| w.join().expect("worker").expect("vector"))
            .collect();
        assert!(!Arc::ptr_eq(&vectors[0], &vectors[1]));
        for provider in providers {
            assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);
        }
    });
    assert_eq!(cache.query_cache_stats().entries, 2);
}
