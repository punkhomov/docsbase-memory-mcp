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
const WATCHDOG: Duration = Duration::from_secs(300);

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
    assert!(cache.lock_entries().is_empty());
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
        assert!(cache.lock_entries().is_empty());
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
