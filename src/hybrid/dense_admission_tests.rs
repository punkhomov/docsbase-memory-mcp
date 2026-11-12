//! Configuration leases protect actual work, never flight registry/waiters.

use super::*;
use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::SourceId;
use crate::hybrid::dense_metadata::MetadataFixture;
use crate::store::read_pool::ReadPool;
use crate::store::settings::{self, ReadPoolPatch, ReadPoolSpec};
use std::sync::atomic::AtomicUsize;
use std::sync::{RwLock, mpsc};

const MODEL: &str = "test/admitted@d3@l2";
const WATCHDOG: Duration = Duration::from_mins(5);

fn request(fixture: &MetadataFixture) -> DenseRequest<'_> {
    DenseRequest {
        cache_root: fixture.cache.path(),
        project_id: fixture.project,
        generation: repo::index_generation(fixture.writer.connection(), fixture.project)
            .expect("generation"),
        model_id: MODEL,
    }
}

fn pool(fixture: &MetadataFixture) -> ReadPool {
    let policy = resolve_component::<ReadPoolSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            ReadPoolPatch {
                max_connections: Some(1),
            },
        )],
    )
    .expect("pool policy");
    ReadPool::prepare(
        fixture.cache.path(),
        &settings::standalone().expect("DB policy"),
        &policy,
    )
    .expect("readers")
}

fn seed(fixture: &MetadataFixture) {
    let mut vectors = VectorCache::open(fixture.cache.path()).expect("vectors");
    for (_, hash) in fixture.original_hashes() {
        vectors.put(MODEL, &hash, &[1.0, 0.0, 0.0]).expect("vector");
    }
}

fn ready(rows: DenseRows) -> Arc<DenseIndex> {
    match rows {
        DenseRows::Ready(index) => index,
        _ => panic!("exact rows expected"),
    }
}

#[test]
fn follower_wait_has_no_revision_or_sql_lease_and_survives_leader_cancel() {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let request = request(&fixture);
    let cache = DenseCache::new();
    let pool = pool(&fixture);
    let leader_revision = RwLock::new(());
    let follower_revision = RwLock::new(());
    let leader_token = Cancellation::new();
    let follower_token = Cancellation::new();
    let loads = AtomicUsize::new(0);
    let (joined, joins) = mpsc::channel();
    cache.cold_flights.observe_joins(joined);
    let (captured, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let proceed = Mutex::new(proceed);
    let (leader, follower, observations) = std::thread::scope(|scope| {
        let leader = scope.spawn(|| {
            cache.get_with_metadata_admitted(
                request,
                &leader_token,
                || {
                    loads.fetch_add(1, AtomicOrdering::SeqCst);
                    let metadata = {
                        let reader = pool.try_acquire()?;
                        DenseMetadata::capture(&reader, request.project_id)?
                    };
                    captured.send(()).expect("owned metadata");
                    proceed
                        .lock()
                        .expect("factory gate")
                        .recv_timeout(WATCHDOG)
                        .expect("release factory");
                    Ok(metadata)
                },
                || Ok(leader_revision.read().expect("leader revision")),
            )
        });
        let first_join = joins.recv_timeout(WATCHDOG);
        let capture = observed.recv_timeout(WATCHDOG);
        let follower = scope.spawn(|| {
            cache.get_with_metadata_admitted(
                request,
                &follower_token,
                || {
                    loads.fetch_add(1, AtomicOrdering::SeqCst);
                    panic!("follower cannot load")
                },
                || Ok(follower_revision.read().expect("follower revision")),
            )
        });
        let second_join = joins.recv_timeout(WATCHDOG);
        let observations = (
            first_join.is_ok() && second_join.is_ok() && capture.is_ok(),
            pool.try_acquire().is_ok(),
            leader_revision.try_write().is_err(),
            follower_revision.try_write().is_ok(),
        );
        leader_token.cancel();
        let _ = release.send(());
        (
            leader.join().expect("leader worker"),
            follower.join().expect("follower worker"),
            observations,
        )
    });
    assert_eq!(observations, (true, true, true, true));
    assert!(matches!(leader, Err(Error::Admission { .. })));
    let result = ready(follower.expect("live follower continues"));
    assert_eq!(result.rows().len(), 2);
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 1);
    let retained = ready(
        cache
            .get_with_metadata(request, &Cancellation::new(), || panic!("hot lookup"))
            .expect("retained"),
    );
    assert!(Arc::ptr_eq(&result, &retained));
    assert!(leader_revision.try_write().is_ok());
    assert!(follower_revision.try_write().is_ok());
}

#[test]
fn stale_admission_prevents_cold_load_warm_and_hot_return() {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let request = request(&fixture);
    let cache = DenseCache::new();
    let deny = || -> Result<()> {
        Err(Error::Project {
            message: "obsolete revision".to_owned(),
            instruction: Some("retry search".to_owned()),
        })
    };
    assert!(matches!(
        cache.get_with_metadata_admitted(
            request,
            &Cancellation::new(),
            || panic!("obsolete cold loader"),
            deny
        ),
        Err(Error::Project { .. })
    ));
    assert_eq!(cache.lock_entries().len(), 0);
    assert!(
        cache
            .warm_with_metadata_admitted(
                request,
                &Cancellation::new(),
                || panic!("obsolete warmer"),
                deny
            )
            .is_err()
    );
    let first = ready(
        cache
            .get_with_metadata(request, &Cancellation::new(), || {
                DenseMetadata::capture(&fixture.writer, fixture.project)
            })
            .expect("current state"),
    );
    assert!(
        cache
            .get_with_metadata_admitted(request, &Cancellation::new(), || panic!("hot load"), deny)
            .is_err()
    );
    let retained = ready(
        cache
            .get_with_metadata(request, &Cancellation::new(), || panic!("still hot"))
            .expect("unchanged resident"),
    );
    assert!(Arc::ptr_eq(&first, &retained));
}

#[test]
fn error_and_unwind_release_leader_revision_and_flight_for_retry() {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let request = request(&fixture);
    for panic in [false, true] {
        let cache = DenseCache::new();
        let revision = RwLock::new(());
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            cache.get_with_metadata_admitted(
                request,
                &Cancellation::new(),
                || {
                    assert!(!panic, "injected loader unwind");
                    Err(Error::internal("injected loader error"))
                },
                || Ok(revision.read().expect("revision")),
            )
        }));
        assert!(if panic {
            outcome.is_err()
        } else {
            outcome.expect("returned error").is_err()
        });
        assert!(revision.try_write().is_ok());
        assert_eq!(cache.lock_entries().len(), 0);
        let retry = cache
            .get_with_metadata_admitted(
                request,
                &Cancellation::new(),
                || DenseMetadata::capture(&fixture.writer, fixture.project),
                || Ok(revision.read().expect("retry revision")),
            )
            .expect("retry");
        assert_eq!(ready(retry).rows().len(), 2);
    }
}

#[test]
fn revision_admission_never_runs_under_flight_registry_mutex() {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let cache = DenseCache::new();
    let rows = cache
        .get_with_metadata_admitted(
            request(&fixture),
            &Cancellation::new(),
            || DenseMetadata::capture(&fixture.writer, fixture.project),
            || {
                assert!(
                    cache.cold_flights.registry_available(),
                    "configuration admission under flight registry can deadlock"
                );
                Ok(())
            },
        )
        .expect("cold work");
    assert_eq!(ready(rows).rows().len(), 2);
}

#[test]
fn all_cancelled_during_metadata_skip_dense_materialization() {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let request = request(&fixture);
    let cache = DenseCache::new();
    let pool = pool(&fixture);
    let leader_token = Cancellation::new();
    let follower_token = Cancellation::new();
    let loads = AtomicUsize::new(0);
    let (joined, joins) = mpsc::channel();
    cache.cold_flights.observe_joins(joined);
    let (captured, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let proceed = Mutex::new(proceed);
    let (leader, follower, observed_both) = std::thread::scope(|scope| {
        let leader = scope.spawn(|| {
            cache.get_with_metadata(request, &leader_token, || {
                loads.fetch_add(1, AtomicOrdering::SeqCst);
                let metadata = {
                    let reader = pool.try_acquire()?;
                    DenseMetadata::capture(&reader, request.project_id)?
                };
                captured.send(()).expect("owned metadata");
                proceed
                    .lock()
                    .expect("metadata gate")
                    .recv_timeout(WATCHDOG)
                    .expect("release metadata");
                Ok(metadata)
            })
        });
        let capture = observed.recv_timeout(WATCHDOG);
        let first = joins.recv_timeout(WATCHDOG);
        let follower = scope.spawn(|| {
            cache.get_with_metadata(request, &follower_token, || {
                loads.fetch_add(1, AtomicOrdering::SeqCst);
                panic!("follower must not load metadata")
            })
        });
        let second = joins.recv_timeout(WATCHDOG);
        leader_token.cancel();
        follower_token.cancel();
        // A cancelled follower leaves before the non-preemptible leader phase.
        let follower = follower.join().expect("cancelled follower");
        let _ = release.send(());
        (
            leader.join().expect("cancelled leader"),
            follower,
            capture.is_ok() && first.is_ok() && second.is_ok(),
        )
    });
    assert!(observed_both);
    assert!(matches!(leader, Err(Error::Admission { .. })));
    assert!(matches!(follower, Err(Error::Admission { .. })));
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 1);
    assert!(
        cache.lock_entries().is_empty(),
        "cancelled metadata must not start dense materialization or publication"
    );
    assert_eq!(cache.cold_flights.active(), 0);
    assert_eq!(leader_token.subscriber_count(), 0);
    assert_eq!(follower_token.subscriber_count(), 0);
    let retry = cache
        .get_with_metadata(request, &Cancellation::new(), || {
            loads.fetch_add(1, AtomicOrdering::SeqCst);
            DenseMetadata::capture(&fixture.writer, fixture.project)
        })
        .expect("live retry initializes");
    assert_eq!(ready(retry).rows().len(), 2);
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 2);
}

fn finish_ann_workers(cache: &DenseCache) -> usize {
    cache.test_build_gate.as_ref().expect("build gate").open();
    let workers = std::mem::take(&mut *cache.test_workers.lock().expect("workers"));
    let count = workers.len();
    for worker in workers {
        worker.join().expect("ANN worker");
    }
    count
}

struct SecondAnnPhase {
    cache: DenseCache,
    outcomes: Arc<Mutex<Vec<bool>>>,
    loading: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

fn second_ann_phase(
    fixture: &MetadataFixture,
    policy: AnnPolicy,
    repair_after_first: bool,
) -> SecondAnnPhase {
    let request = request(fixture);
    let chunks = fixture.original_hashes();
    let key = ann_key(
        request.project_id,
        request.generation,
        MODEL,
        3,
        chunks.len(),
        ann_store::chunk_set_digest(&chunks),
    );
    let rows: Vec<_> = chunks
        .iter()
        .map(|(id, _)| (*id, Arc::<[f32]>::from([1.0, 0.0, 0.0])))
        .collect();
    let store = AnnStore::new(fixture.cache.path().to_path_buf());
    let slot = AnnIndex::build(&rows, 3)
        .expect("real ANN")
        .dump(&store, &key)
        .expect("persisted ANN");
    let graph = slot.dir.join("index.hnsw.graph");
    let valid_graph = std::fs::read(&graph).expect("saved graph");
    std::fs::write(&graph, b"invalid graph").expect("corrupt graph only");
    assert!(store.load(&key).expect("valid slot metadata").is_some());

    let mut cache = DenseCache::with_policy_limits(
        policy,
        fixture.cache.path(),
        DenseCacheLimits {
            min_vectors: 1,
            ..DenseCacheLimits::default()
        },
    );
    cache.test_build_gate = Some(TestBuildGate::new());
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let observed_outcomes = Arc::clone(&outcomes);
    let (loaded, loading) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let proceed = Mutex::new(proceed);
    cache.test_ann_load = Some(Arc::new(move |slot, succeeded| {
        let probe = {
            let mut outcomes = observed_outcomes.lock().expect("ANN outcomes");
            outcomes.push(succeeded);
            outcomes.len()
        };
        if probe == 1 && repair_after_first {
            // First graph load really failed. A valid artifact becomes available
            // between probes, so the later real load can reach publication.
            std::fs::write(slot.dir.join("index.hnsw.graph"), &valid_graph)
                .expect("repair graph between probes");
        }
        if probe == 2 {
            // Hold the actual synchronous load's result before it returns to
            // resolve_rows, including its error result on the build/fallback path.
            loaded.send(()).expect("second ANN phase");
            proceed
                .lock()
                .expect("ANN phase gate")
                .recv_timeout(WATCHDOG)
                .expect("release second ANN phase");
        }
    }));
    SecondAnnPhase {
        cache,
        outcomes,
        loading,
        release,
    }
}

fn second_ann_load_interest(policy: AnnPolicy, repair_after_first: bool, live_follower: bool) {
    let fixture = MetadataFixture::new();
    seed(&fixture);
    let request = request(&fixture);
    let pool = pool(&fixture);
    let SecondAnnPhase {
        cache,
        outcomes,
        loading,
        release,
    } = second_ann_phase(&fixture, policy, repair_after_first);
    let leader_token = Cancellation::new();
    let follower_token = Cancellation::new();
    let metadata_loads = AtomicUsize::new(0);
    let revision = RwLock::new(());
    let (joined, joins) = mpsc::channel();
    cache.cold_flights.observe_joins(joined);
    let capture = || {
        metadata_loads.fetch_add(1, AtomicOrdering::SeqCst);
        let reader = pool.try_acquire()?;
        DenseMetadata::capture(&reader, request.project_id)
    };
    let (leader, follower, reached) = std::thread::scope(|scope| {
        let leader = scope.spawn(|| {
            cache.get_with_metadata_admitted(request, &leader_token, capture, || {
                Ok(revision.read().expect("leader revision"))
            })
        });
        let second_load = loading.recv_timeout(WATCHDOG);
        let first_join = joins.recv_timeout(WATCHDOG);
        let follower = scope.spawn(|| {
            // A background warm is an independent live participant too.
            cache.warm_with_metadata_admitted(
                request,
                &follower_token,
                || panic!("joined follower must not capture metadata"),
                || Ok(revision.read().expect("follower revision")),
            )
        });
        let second_join = joins.recv_timeout(WATCHDOG);
        leader_token.cancel();
        let follower = if live_follower {
            let _ = release.send(());
            follower.join().expect("live follower")
        } else {
            follower_token.cancel();
            let result = follower.join().expect("cancelled follower");
            let _ = release.send(());
            result
        };
        (
            leader.join().expect("leader worker"),
            follower,
            second_load.is_ok() && first_join.is_ok() && second_join.is_ok(),
        )
    });
    let empty = cache.lock_entries().is_empty();
    let charged = cache.active_build_bytes.load(AtomicOrdering::Acquire);
    let workers = finish_ann_workers(&cache);
    assert!(
        reached,
        "both callers must reach the actual second ANN phase"
    );
    assert_eq!(
        *outcomes.lock().expect("load outcomes"),
        [false, repair_after_first]
    );
    assert!(matches!(leader, Err(Error::Admission { .. })));
    assert_eq!(metadata_loads.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(cache.cold_flights.active(), 0);
    assert_eq!(leader_token.subscriber_count(), 0);
    assert_eq!(follower_token.subscriber_count(), 0);
    assert!(revision.try_write().is_ok());
    assert!(pool.try_acquire().is_ok());
    if live_follower {
        follower.expect("live warmer survives leader cancellation");
        assert_live_ann(&cache, request, repair_after_first, workers);
    } else {
        assert!(matches!(follower, Err(Error::Admission { .. })));
        assert_eq!(workers, 0, "all-cancelled ANN IO must not start a build");
        assert!(
            empty,
            "all-cancelled ANN IO must not publish loaded or exact rows"
        );
        assert_eq!(charged, 0);
        assert_ann_retry(&cache, request, policy, repair_after_first, capture);
        assert_eq!(metadata_loads.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(cache.cold_flights.active(), 0);
    }
    assert_eq!(cache.active_build_bytes.load(AtomicOrdering::Acquire), 0);
}

fn assert_live_ann(
    cache: &DenseCache,
    request: DenseRequest<'_>,
    repair_after_first: bool,
    workers: usize,
) {
    if repair_after_first {
        let DenseRows::Ann(retained) = cache
            .get_with_metadata(request, &Cancellation::new(), || panic!("hot ANN"))
            .expect("retained ANN")
        else {
            panic!("ANN must stay resident");
        };
        assert_eq!(retained.count(), 2);
        assert_eq!(workers, 0);
    } else {
        assert_eq!(workers, 1);
        assert!(cache.ann_snapshot(request.project_id).is_some());
    }
}

fn assert_ann_retry(
    cache: &DenseCache,
    request: DenseRequest<'_>,
    policy: AnnPolicy,
    repair_after_first: bool,
    capture: impl FnOnce() -> Result<DenseMetadata>,
) {
    let retry = cache.get_with_metadata(request, &Cancellation::new(), capture);
    let retry_workers = finish_ann_workers(cache);
    let retry = retry.expect("live retry initializes after abandoned ANN phase");
    if repair_after_first {
        assert!(matches!(retry, DenseRows::Ann(_)));
    } else if policy == AnnPolicy::BuildOrLoad {
        assert!(matches!(retry, DenseRows::Building(_)));
        assert_eq!(retry_workers, 1);
        assert!(cache.ann_snapshot(request.project_id).is_some());
    } else {
        assert_eq!(ready(retry).rows().len(), 2);
        assert_eq!(retry_workers, 0);
    }
}

#[test]
fn all_cancelled_during_second_ann_load_skip_publication() {
    second_ann_load_interest(AnnPolicy::BuildOrLoad, true, false);
}

#[test]
fn all_cancelled_during_second_ann_load_skip_build() {
    second_ann_load_interest(AnnPolicy::BuildOrLoad, false, false);
}

#[test]
fn all_cancelled_during_second_ann_load_skip_exact_fallback() {
    second_ann_load_interest(AnnPolicy::LoadOnly, false, false);
}

#[test]
fn cancelled_leader_during_second_ann_load_preserves_live_warm_publication() {
    second_ann_load_interest(AnnPolicy::BuildOrLoad, true, true);
}

#[test]
fn cancelled_leader_during_second_ann_load_preserves_live_warm_build() {
    second_ann_load_interest(AnnPolicy::BuildOrLoad, false, true);
}
