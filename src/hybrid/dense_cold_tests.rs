use super::*;
use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::SourceId;
use crate::hybrid::dense_metadata::{DenseMetadata, MetadataFixture};
use crate::store::read_pool::ReadPool;
use crate::store::settings::{self, ReadPoolPatch, ReadPoolSpec};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

const MODEL: &str = "test/cold@d3@l2";
const WATCHDOG: Duration = Duration::from_mins(5);

fn request(root: &std::path::Path, project_id: i64, generation: i64) -> DenseRequest<'_> {
    DenseRequest {
        cache_root: root,
        project_id,
        generation,
        model_id: MODEL,
    }
}

fn ready(rows: DenseRows) -> Arc<DenseIndex> {
    match rows {
        DenseRows::Ready(rows) => rows,
        _ => panic!("expected exact ready rows"),
    }
}

fn seed(fixture: &MetadataFixture, count: usize) {
    let mut cache = VectorCache::open(fixture.cache.path()).expect("vectors");
    for (_, hash) in fixture.original_hashes().into_iter().take(count) {
        cache.put(MODEL, &hash, &[1.0, 0.0, 0.0]).expect("vector");
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Error,
    Panic,
}
type Caught = std::thread::Result<Result<DenseRows>>;

fn assert_control_available(pool: &ReadPool, fixture: &MetadataFixture, generation: i64) {
    // Even with capacity1, waiters retain no metadata reader/transaction.
    let reader = pool
        .acquire(Instant::now() + WATCHDOG)
        .expect("independent reader");
    assert!(reader.connection().is_autocommit());
    assert_eq!(
        repo::index_generation(reader.connection(), fixture.project).expect("control generation"),
        generation
    );
    drop(reader);
    fixture
        .writer
        .connection()
        .execute(
            "UPDATE projects SET name='control' WHERE id=?1",
            [fixture.project],
        )
        .expect("independent writer");
}

fn single_reader_pool(fixture: &MetadataFixture) -> ReadPool {
    let db_policy = settings::standalone().expect("db policy");
    let pool_policy = resolve_component::<ReadPoolSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            ReadPoolPatch {
                max_connections: Some(1),
            },
        )],
    )
    .expect("pool policy");
    ReadPool::prepare(fixture.cache.path(), &db_policy, &pool_policy).expect("pool")
}

fn four_cold(outcome: Outcome, cancel: Option<usize>) -> Vec<Caught> {
    let fixture = MetadataFixture::new();
    seed(&fixture, 2);
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let request = request(fixture.cache.path(), fixture.project, generation);
    let cache = DenseCache::new();
    let shared = cache.snapshot();
    let flights = Arc::clone(&cache.cold_flights);
    let (joined, joins) = mpsc::channel();
    flights.observe_joins(joined);
    let pool = single_reader_pool(&fixture);
    let controls: Vec<_> = (0..4).map(|_| Cancellation::new()).collect();
    let loads = AtomicUsize::new(0);
    let results = std::thread::scope(|scope| {
        let (release, proceed) = mpsc::channel();
        let proceed = Arc::new(Mutex::new(proceed));
        let (loaded, loading) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let mut workers = Vec::new();
        for (index, control) in controls.iter().enumerate() {
            let cache = if index % 2 == 0 { &cache } else { &shared };
            let pool = &pool;
            let loads = &loads;
            let proceed = Arc::clone(&proceed);
            let loaded = loaded.clone();
            let done = done.clone();
            workers.push(scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cache.get_with_metadata(request, control, || {
                        loads.fetch_add(1, AtomicOrdering::SeqCst);
                        let metadata = {
                            let reader = pool.acquire(Instant::now() + WATCHDOG)?;
                            DenseMetadata::capture(&reader, request.project_id)?
                        };
                        loaded.send(()).expect("metadata loaded");
                        proceed
                            .lock()
                            .expect("gate")
                            .recv_timeout(WATCHDOG)
                            .map_err(|err| Error::internal_with_source("cold test gate", err))?;
                        match outcome {
                            Outcome::Success => Ok(metadata),
                            Outcome::Error => Err(Error::Project {
                                message: "cold loader failed".to_owned(),
                                instruction: None,
                            }),
                            Outcome::Panic => panic!("cold loader unwind"),
                        }
                    })
                }));
                done.send(index).expect("completion");
                result
            }));
            if index == 0 {
                loading
                    .recv_timeout(WATCHDOG)
                    .expect("leader loaded metadata");
            }
        }
        for _ in 0..4 {
            joins
                .recv_timeout(WATCHDOG)
                .expect("cold participant joined");
        }
        assert_control_available(&pool, &fixture, generation);
        if let Some(index) = cancel {
            controls[index].cancel();
            if index != 0 {
                assert_eq!(
                    completed
                        .recv_timeout(WATCHDOG)
                        .expect("waiter cancelled without waiting for cold work"),
                    index
                );
            }
        }
        // Dropping sender on assertion unwind releases every faulty duplicate
        // loader; extra tokens let a broken non-coalesced version finish.
        for _ in 0..4 {
            let _ = release.send(());
        }
        workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect()
    });
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(flights.active(), 0);
    for control in controls {
        assert_eq!(control.subscriber_count(), 0);
    }
    results
}

#[test]
fn cold_callers_share_one_loader_and_one_row_snapshot() {
    let rows: Vec<_> = four_cold(Outcome::Success, None)
        .into_iter()
        .map(|r| ready(r.expect("no panic").expect("rows")))
        .collect();
    assert_eq!(rows[0].rows().len(), 2);
    for row in &rows {
        assert!(Arc::ptr_eq(&rows[0], row));
    }
}

#[test]
fn cold_waiter_cancellation_preserves_live_callers() {
    for (index, result) in four_cold(Outcome::Success, Some(1)).into_iter().enumerate() {
        let result = result.expect("no panic");
        if index == 1 {
            assert!(matches!(result, Err(Error::Admission { .. })));
        } else {
            assert_eq!(ready(result.expect("rows")).rows().len(), 2);
        }
    }
}

#[test]
fn cold_leader_cancellation_preserves_live_callers() {
    for (index, result) in four_cold(Outcome::Success, Some(0)).into_iter().enumerate() {
        let result = result.expect("no panic");
        if index == 0 {
            assert!(matches!(result, Err(Error::Admission { .. })));
        } else {
            assert_eq!(ready(result.expect("rows")).rows().len(), 2);
        }
    }
}

#[test]
fn cold_loader_error_is_shared_without_waiter_leaks() {
    for result in four_cold(Outcome::Error, None) {
        let error = result.expect("no panic").expect_err("loader failed");
        assert_eq!(error.mcp_code(), -32012);
        assert_eq!(error.inner_message(), "cold loader failed");
    }
}

#[test]
fn cold_loader_unwind_wakes_waiters() {
    let mut results = four_cold(Outcome::Panic, None);
    assert!(results.remove(0).is_err());
    for result in results {
        assert!(result.expect("waiter did not panic").is_err());
    }
}

#[test]
fn wrong_metadata_identity_has_no_vector_io() {
    let fixture = MetadataFixture::new();
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let cache = DenseCache::new();
    for (project, expected) in [
        (fixture.project + 1, generation),
        (fixture.project, generation + 1),
    ] {
        let result = cache.get_with_metadata(
            request(fixture.cache.path(), project, expected),
            &Cancellation::new(),
            || DenseMetadata::capture(&fixture.reader(), fixture.project),
        );
        assert!(result.is_err());
        assert!(
            !fixture
                .cache
                .path()
                .join(crate::vector_cache::VECTOR_DB_FILE)
                .exists()
        );
        assert_eq!(cache.lock_entries().len(), 0);
        assert_eq!(cache.cold_flights.active(), 0);
    }
}

#[test]
fn one_dense_cache_rejects_a_different_metadata_root_before_reuse() {
    let first = MetadataFixture::new();
    let second = MetadataFixture::new();
    let cache = DenseCache::new();
    cache
        .get(&first.reader(), first.project, MODEL)
        .expect("bind first");
    assert!(cache.get(&second.reader(), second.project, MODEL).is_err());
    assert!(
        !second
            .cache
            .path()
            .join(crate::vector_cache::VECTOR_DB_FILE)
            .exists()
    );
}

#[test]
fn warm_refreshes_ready_vector_coverage_at_the_same_generation() {
    let fixture = MetadataFixture::new();
    seed(&fixture, 1);
    let cache = DenseCache::new();
    let reader = fixture.reader();
    assert_eq!(
        ready(cache.get(&reader, fixture.project, MODEL).expect("first"))
            .rows()
            .len(),
        1
    );
    seed(&fixture, 2);
    let generation =
        repo::index_generation(reader.connection(), fixture.project).expect("generation");
    cache
        .warm_with_metadata(
            request(fixture.cache.path(), fixture.project, generation),
            &Cancellation::new(),
            || DenseMetadata::capture(&reader, fixture.project),
        )
        .expect("lazy refresh");
    assert_eq!(
        ready(
            cache
                .get(&reader, fixture.project, MODEL)
                .expect("lazy refreshed")
        )
        .rows()
        .len(),
        2
    );
    cache
        .warm(&reader, fixture.project, MODEL)
        .expect("refresh");
    assert_eq!(
        ready(
            cache
                .get(&reader, fixture.project, MODEL)
                .expect("refreshed")
        )
        .rows()
        .len(),
        2
    );
}

#[test]
fn cold_loader_failure_can_be_retried() {
    let fixture = MetadataFixture::new();
    seed(&fixture, 2);
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let request = request(fixture.cache.path(), fixture.project, generation);
    let cache = DenseCache::new();
    assert!(
        cache
            .get_with_metadata(request, &Cancellation::new(), || Err(Error::internal(
                "loader error"
            )))
            .is_err()
    );
    assert_eq!(cache.cold_flights.active(), 0);
    let rows = cache
        .get_with_metadata(request, &Cancellation::new(), || {
            DenseMetadata::capture(&fixture.reader(), fixture.project)
        })
        .expect("retry");
    assert_eq!(ready(rows).rows().len(), 2);
}

#[test]
fn wrong_snapshot_root_is_rejected_before_vector_io() {
    let first = MetadataFixture::new();
    let other = MetadataFixture::new();
    let generation =
        repo::index_generation(first.writer.connection(), first.project).expect("generation");
    let cache = DenseCache::new();
    let result = cache.get_with_metadata(
        request(first.cache.path(), first.project, generation),
        &Cancellation::new(),
        || DenseMetadata::capture(&other.reader(), other.project),
    );
    assert!(result.is_err());
    assert!(
        !first
            .cache
            .path()
            .join(crate::vector_cache::VECTOR_DB_FILE)
            .exists()
    );
    assert!(
        !other
            .cache
            .path()
            .join(crate::vector_cache::VECTOR_DB_FILE)
            .exists()
    );
    assert_eq!(cache.lock_entries().len(), 0);
}

#[test]
fn cached_rows_skip_loader_and_query_only_sharing_keeps_cold_state_separate() {
    let fixture = MetadataFixture::new();
    seed(&fixture, 2);
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let request = request(fixture.cache.path(), fixture.project, generation);
    let cache = DenseCache::new();
    let original = ready(
        cache
            .get_with_metadata(request, &Cancellation::new(), || {
                DenseMetadata::capture(&fixture.reader(), fixture.project)
            })
            .expect("first"),
    );
    let same = ready(
        cache
            .get_with_metadata(request, &Cancellation::new(), || {
                panic!("cached rows must not read metadata")
            })
            .expect("cached"),
    );
    assert!(Arc::ptr_eq(&original, &same));
    let fresh = cache.sharing_query_cache();
    let loads = AtomicUsize::new(0);
    let independent = ready(
        fresh
            .get_with_metadata(request, &Cancellation::new(), || {
                loads.fetch_add(1, AtomicOrdering::SeqCst);
                DenseMetadata::capture(&fixture.reader(), fixture.project)
            })
            .expect("independent cold state"),
    );
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 1);
    assert!(!Arc::ptr_eq(&original, &independent));
}

#[test]
fn cancelled_cold_request_skips_loader_and_cached_rows() {
    let fixture = MetadataFixture::new();
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let request = request(fixture.cache.path(), fixture.project, generation);
    let cache = DenseCache::new();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    for warm in [false, true] {
        let result = if warm {
            cache.warm_with_metadata(request, &cancelled, || panic!("cancelled loader"))
        } else {
            cache
                .get_with_metadata(request, &cancelled, || panic!("cancelled loader"))
                .map(|_| ())
        };
        assert!(matches!(result, Err(Error::Admission { .. })));
    }
    cache
        .get_with_metadata(request, &Cancellation::new(), || {
            DenseMetadata::capture(&fixture.reader(), fixture.project)
        })
        .expect("initial rows");
    assert!(matches!(
        cache.get_with_metadata(request, &cancelled, || panic!("cached loader")),
        Err(Error::Admission { .. })
    ));
    assert_eq!(cache.cold_flights.active(), 0);
    assert_eq!(cancelled.subscriber_count(), 0);
}

#[test]
fn warm_building_state_skips_loader_without_refreshing_recency() {
    let fixture = MetadataFixture::new();
    let generation =
        repo::index_generation(fixture.writer.connection(), fixture.project).expect("generation");
    let request = request(fixture.cache.path(), fixture.project, generation);
    let cache = DenseCache::new();
    cache
        .stash(
            fixture.project,
            generation,
            MODEL,
            DenseRows::Building(Arc::new(BuildingAnn {
                hashes: Arc::new(fixture.original_hashes()),
                bytes: 1,
            })),
        )
        .expect("fixture building view");
    let before = cache.lock_entries()[&fixture.project].last_used;
    cache
        .warm_with_metadata(request, &Cancellation::new(), || panic!("already building"))
        .expect("idempotent warm");
    assert_eq!(cache.lock_entries()[&fixture.project].last_used, before);
}

#[test]
fn paused_old_metadata_completion_cannot_replace_new_generation() {
    let fixture = MetadataFixture::new();
    seed(&fixture, 2);
    let old = DenseMetadata::capture(&fixture.reader(), fixture.project).expect("old metadata");
    let old_generation = old.generation();
    let cache = DenseCache::new();
    let shared = cache.snapshot();
    let request = request(fixture.cache.path(), fixture.project, old_generation);
    std::thread::scope(|scope| {
        let (loaded, loading) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let worker = scope.spawn(move || {
            shared.get_with_metadata(request, &Cancellation::new(), || {
                loaded.send(()).expect("old loader");
                proceed
                    .recv_timeout(WATCHDOG)
                    .map_err(|err| Error::internal_with_source("old generation gate", err))?;
                Ok(old)
            })
        });
        loading.recv_timeout(WATCHDOG).expect("old loader admitted");
        let transaction = fixture
            .writer
            .connection()
            .unchecked_transaction()
            .expect("writer");
        transaction
            .execute(
                "UPDATE chunks SET text='changed' WHERE doc_id=?1 AND seq=0",
                [fixture.doc],
            )
            .expect("change");
        repo::bump_index_generation(&transaction, fixture.project).expect("bump");
        transaction.commit().expect("commit");
        let new = ready(
            cache
                .get(&fixture.reader(), fixture.project, MODEL)
                .expect("new generation"),
        );
        assert_eq!(new.generation(), old_generation + 1);
        assert_eq!(new.rows().len(), 1);
        release.send(()).expect("release old");
        let old = ready(worker.join().expect("worker").expect("old result"));
        assert_eq!(old.generation(), old_generation);
        assert_eq!(old.rows().len(), 2);
        let retained = ready(
            cache
                .get(&fixture.reader(), fixture.project, MODEL)
                .expect("retained"),
        );
        assert!(Arc::ptr_eq(&new, &retained));
        assert_eq!(cache.cold_flights.active(), 0);
    });
}

#[test]
fn owned_root_aliases_share_one_cold_flight_and_metadata_identity() {
    let fixture = MetadataFixture::new();
    let owner = VectorStore::acquire(
        fixture.cache.path(),
        crate::vector_cache::settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    {
        let mut vectors = owner.connect().unwrap();
        for (_, hash) in fixture.original_hashes() {
            vectors.put(MODEL, &hash, &[1.0, 0.0, 0.0]).unwrap();
        }
    }
    let cache = DenseCache::with_vector_store(
        AnnPolicy::Disabled,
        &owner,
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
    )
    .unwrap();
    let generation = repo::index_generation(fixture.writer.connection(), fixture.project).unwrap();
    let child = fixture.cache.path().join("alias-child");
    std::fs::create_dir(&child).unwrap();
    let roots = [fixture.cache.path().to_path_buf(), child.join("..")];
    let pool = single_reader_pool(&fixture);
    let loads = AtomicUsize::new(0);
    let (joined, joins) = mpsc::channel();
    cache.cold_flights.observe_joins(joined);
    let project = fixture.project;
    let results = std::thread::scope(|scope| {
        let (release, proceed) = mpsc::channel();
        let proceed = Arc::new(Mutex::new(proceed));
        let (loaded, loading) = mpsc::channel();
        let mut workers = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            let (cache, pool, loads) = (&cache, &pool, &loads);
            let proceed = Arc::clone(&proceed);
            let loaded = loaded.clone();
            workers.push(scope.spawn(move || {
                cache.get_with_metadata(
                    request(root, project, generation),
                    &Cancellation::new(),
                    || {
                        loads.fetch_add(1, AtomicOrdering::SeqCst);
                        let metadata = {
                            let reader = pool.acquire(Instant::now() + WATCHDOG)?;
                            DenseMetadata::capture(&reader, project)?
                        };
                        loaded.send(()).unwrap();
                        proceed
                            .lock()
                            .unwrap()
                            .recv_timeout(WATCHDOG)
                            .map_err(|err| Error::internal_with_source("alias gate", err))?;
                        Ok(metadata)
                    },
                )
            }));
            if index == 0 {
                loading.recv_timeout(WATCHDOG).unwrap();
            }
        }
        for _ in 0..2 {
            joins.recv_timeout(WATCHDOG).unwrap();
        }
        // Multiple tokens also release an accidentally duplicated initializer.
        for _ in 0..2 {
            let _ = release.send(());
        }
        workers
            .into_iter()
            .map(|worker| ready(worker.join().unwrap().unwrap()))
            .collect::<Vec<_>>()
    });
    assert_eq!(loads.load(AtomicOrdering::SeqCst), 1);
    assert!(Arc::ptr_eq(&results[0], &results[1]));
    assert_eq!(results[0].rows().len(), 2);
}
