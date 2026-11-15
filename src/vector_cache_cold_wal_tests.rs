//! Controlled real cold journal-mode contention, separate from initialized allocators.
use super::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

type InitializationObserver = Box<dyn Fn(Phase)>;

thread_local! {
    static BUSY:RefCell<Option<Box<dyn Fn()>>>=RefCell::new(None);
    static NOW:RefCell<Option<Instant>>=const{RefCell::new(None)};
    static TIMEOUTS:RefCell<Vec<i64>>=const{RefCell::new(Vec::new())};
    static INITIALIZATION:RefCell<Option<InitializationObserver>>=RefCell::new(None);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    BeforeSqlite,
    AfterWal,
    Contended,
}
pub(super) fn observe_phase(phase: Phase) {
    INITIALIZATION.with(|slot| {
        if let Some(observer) = slot.borrow().as_ref() {
            observer(phase);
        }
    });
}
struct InitializationHook;
impl InitializationHook {
    fn new(observer: impl Fn(Phase) + 'static) -> Self {
        INITIALIZATION.with(|slot| *slot.borrow_mut() = Some(Box::new(observer)));
        Self
    }
}
impl Drop for InitializationHook {
    fn drop(&mut self) {
        INITIALIZATION.with(|slot| *slot.borrow_mut() = None);
    }
}
pub(super) fn clock_now() -> Option<Instant> {
    NOW.with(|clock| *clock.borrow())
}
pub(super) fn observe_busy(result: &rusqlite::Result<()>, conn: &Connection) {
    let timeout = conn
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .unwrap();
    TIMEOUTS.with(|values| values.borrow_mut().push(timeout));
    if matches!(result,Err(rusqlite::Error::SqliteFailure(code,_)) if code.code==rusqlite::ErrorCode::DatabaseBusy)
    {
        BUSY.with(|slot| {
            if let Some(observer) = slot.borrow().as_ref() {
                observer();
            }
        });
    }
}
struct Hook;
impl Hook {
    fn new(clock: Option<Instant>, observer: impl Fn() + 'static) -> Self {
        NOW.with(|now| *now.borrow_mut() = clock);
        TIMEOUTS.with(|values| values.borrow_mut().clear());
        BUSY.with(|slot| *slot.borrow_mut() = Some(Box::new(observer)));
        Self
    }
    fn timeouts() -> Vec<i64> {
        TIMEOUTS.with(|values| values.borrow().clone())
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        BUSY.with(|slot| *slot.borrow_mut() = None);
        NOW.with(|now| *now.borrow_mut() = None);
        TIMEOUTS.with(|values| values.borrow_mut().clear());
    }
}
fn busy(error: &Error) -> bool {
    std::error::Error::source(error).and_then(|source|source.downcast_ref::<rusqlite::Error>()).is_some_and(|error|matches!(error,rusqlite::Error::SqliteFailure(code,_) if code.code==rusqlite::ErrorCode::DatabaseBusy))
}
fn initializer(root: &Path) -> Connection {
    let conn = Connection::open(root.join(VECTOR_DB_FILE)).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    conn
}

#[test]
fn cold_constructor_serializes_sqlite_until_schema_initialization_finishes() {
    let root = tempfile::tempdir().unwrap();
    let (entered, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let path = root.path().to_path_buf();
    let first = std::thread::spawn(move || {
        let _hook = InitializationHook::new(move |phase| {
            if phase == Phase::AfterWal {
                entered.send(()).unwrap();
                proceed.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        });
        VectorCache::open(&path)
    });
    let first_entered = observed.recv_timeout(Duration::from_secs(10));
    let (decision, observed) = mpsc::channel();
    let path = root.path().to_path_buf();
    let second = std::thread::spawn(move || {
        let first = std::cell::Cell::new(true);
        let _hook = InitializationHook::new(move |phase| {
            if phase != Phase::AfterWal && first.replace(false) {
                decision.send(phase).unwrap();
            }
        });
        VectorCache::open(&path)
    });
    let second_phase = observed.recv_timeout(Duration::from_secs(10));
    let _ = release.send(());
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert!(
        first_entered.is_ok(),
        "actual first WAL phase must be reached"
    );
    assert_eq!(
        second_phase.unwrap(),
        Phase::Contended,
        "second constructor must wait before SQLite while first owns WAL/schema initialization"
    );
    first.unwrap();
    second.unwrap();
}

fn held_owned_initialization(
    owner: VectorStore,
) -> (
    mpsc::Sender<()>,
    std::thread::JoinHandle<Result<VectorCache>>,
) {
    let (entered, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _hook = InitializationHook::new(move |phase| {
            if phase == Phase::AfterWal {
                entered.send(()).unwrap();
                proceed.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        });
        owner.connect()
    });
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    (release, worker)
}

#[test]
fn queued_initialization_zero_and_expiry_refuse_before_sqlite_without_blocking_other_roots() {
    for milliseconds in [0, 73] {
        let root = tempfile::tempdir().unwrap();
        let policy = settings::VectorStoreSettings {
            busy_timeout: Duration::from_millis(milliseconds),
            ..settings::VectorStoreSettings::owner_defaults()
        };
        let owner = VectorStore::acquire(root.path(), &policy).unwrap();
        let (release, worker) = held_owned_initialization(owner.clone());
        let independent = tempfile::tempdir().unwrap();
        let other = VectorCache::open(independent.path());
        let start = Instant::now();
        let _clock = Hook::new(Some(start), || {
            panic!("queued refusal must not reach SQLite")
        });
        let waiting = Rc::new(std::cell::Cell::new(0));
        let native = Rc::new(std::cell::Cell::new(0));
        let waits = waiting.clone();
        let entries = native.clone();
        let initialization = InitializationHook::new(move |phase| match phase {
            Phase::Contended => {
                let count = waits.get() + 1;
                waits.set(count);
                let elapsed = if count == 1 {
                    milliseconds.saturating_sub(1)
                } else {
                    milliseconds
                };
                NOW.with(|now| *now.borrow_mut() = Some(start + Duration::from_millis(elapsed)));
            }
            Phase::BeforeSqlite => entries.set(entries.get() + 1),
            Phase::AfterWal => {}
        });
        let refused = owner.connect();
        drop(initialization);
        let timeouts = Hook::timeouts();
        let _ = release.send(());
        let first = worker.join().unwrap();
        assert!(
            other.is_ok(),
            "another root completes while this root is held"
        );
        assert!(
            matches!(refused, Err(Error::Admission { message }) if message == "vector cache initialization contention window expired")
        );
        assert_eq!(waiting.get(), if milliseconds == 0 { 1 } else { 2 });
        assert_eq!(native.get(), 0);
        assert_eq!(timeouts, [] as [i64; 0]);
        first.unwrap();
        owner.connect().unwrap();
    }
}

#[test]
fn queued_initialization_and_wal_share_the_original_timeout() {
    let root = tempfile::tempdir().unwrap();
    let policy = settings::VectorStoreSettings {
        busy_timeout: Duration::from_millis(73),
        ..settings::VectorStoreSettings::owner_defaults()
    };
    let owner = VectorStore::acquire(root.path(), &policy).unwrap();
    let (release, worker) = held_owned_initialization(owner.clone());
    let worker = RefCell::new(Some(worker));
    let start = Instant::now();
    let clock = Hook::new(Some(start), || {
        panic!("already-WAL database must not need a BUSY retry")
    });
    let initialization = InitializationHook::new(move |phase| {
        if phase == Phase::Contended {
            NOW.with(|now| *now.borrow_mut() = Some(start + Duration::from_millis(72)));
            release.send(()).unwrap();
            worker.borrow_mut().take().unwrap().join().unwrap().unwrap();
        }
    });
    let cache = owner.connect().unwrap();
    assert_eq!(
        Hook::timeouts(),
        [1],
        "queue time must not reset the WAL window"
    );
    assert_eq!(
        cache
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
    drop(initialization);
    drop(clock);
}

#[test]
fn initialization_lock_error_precedes_sqlite_and_releases_mode_ownership() {
    let root = tempfile::tempdir().unwrap();
    let lock = root.path().join("vectors.init.lock");
    std::fs::create_dir(&lock).unwrap();
    assert!(VectorCache::open(root.path()).is_err());
    assert!(!root.path().join(VECTOR_DB_FILE).exists());
    std::fs::remove_dir(lock).unwrap();
    VectorStore::acquire(
        root.path(),
        &settings::VectorStoreSettings::owner_defaults(),
    )
    .unwrap();
}

#[test]
fn cold_constructor_retries_real_wal_busy_after_initializer_releases() {
    let root = tempfile::tempdir().unwrap();
    let initializer = initializer(root.path());
    let (busy, observed) = mpsc::channel();
    let mut signals = Vec::new();
    let mut constructors = Vec::new();
    for index in 0..2 {
        let path = root.path().to_path_buf();
        let busy = busy.clone();
        let (resume, proceed) = mpsc::channel();
        signals.push(resume);
        constructors.push(std::thread::spawn(move || {
            let started = Instant::now();
            let first = std::cell::Cell::new(true);
            let observation = Rc::new(move |kind| {
                if first.replace(false) {
                    busy.send((index, kind, started.elapsed())).unwrap();
                    proceed.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            });
            let on_busy = observation.clone();
            let _hook = Hook::new(None, move || on_busy("sqlite-busy"));
            let _initialization = InitializationHook::new(move |phase| {
                if phase == Phase::Contended {
                    observation("initialization-lock");
                }
            });
            let result = VectorCache::open(&path);
            result.map(|cache| {
                let mode: String = cache
                    .conn
                    .pragma_query_value(None, "journal_mode", |row| row.get(0))
                    .unwrap();
                let timeout: i64 = cache
                    .conn
                    .pragma_query_value(None, "busy_timeout", |row| row.get(0))
                    .unwrap();
                (mode, timeout)
            })
        }));
    }
    let observed: Vec<_> = (0..2)
        .map(|_| observed.recv_timeout(Duration::from_secs(10)))
        .collect();
    assert!(
        matches!(
            VectorStore::acquire(
                root.path(),
                &settings::VectorStoreSettings::owner_defaults()
            ),
            Err(Error::Admission { .. })
        ),
        "both actual constructors retain mode leases before WAL/schema"
    );
    assert_eq!(
        initializer
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    initializer.execute_batch("ROLLBACK;").unwrap();
    drop(initializer);
    for signal in signals {
        let _ = signal.send(());
    }
    let results: Vec<_> = constructors
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    eprintln!("real first WAL busy: {observed:?}; results={results:?}");
    assert!(observed.iter().all(std::result::Result::is_ok));
    let mut kinds: Vec<_> = observed
        .iter()
        .map(|event| event.as_ref().unwrap().1)
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["initialization-lock", "sqlite-busy"]);
    let expected_timeout = i64::try_from(
        settings::VectorStoreSettings::owner_defaults()
            .busy_timeout
            .as_millis(),
    )
    .unwrap();
    for result in results {
        assert_eq!(
            result.unwrap(),
            ("wal".into(), expected_timeout),
            "both cold constructors recover and restore owner timeout"
        );
    }
}

#[test]
fn zero_timeout_is_one_immediate_cold_wal_attempt() {
    let root = tempfile::tempdir().unwrap();
    let initializer = initializer(root.path());
    let hook = Hook::new(None, || {});
    let policy = settings::VectorStoreSettings {
        busy_timeout: Duration::ZERO,
        ..settings::VectorStoreSettings::owner_defaults()
    };
    let error = VectorStore::acquire(root.path(), &policy).err().unwrap();
    assert!(busy(&error));
    assert_eq!(Hook::timeouts(), vec![0]);
    drop(hook);
    initializer.execute_batch("ROLLBACK;").unwrap();
    drop(initializer);
    let owner = VectorStore::acquire(root.path(), &policy).unwrap();
    assert_eq!(
        owner
            .connect()
            .unwrap()
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn cold_wal_retries_share_one_absolute_budget_including_sqlite_wait() {
    let root = tempfile::tempdir().unwrap();
    let initializer = initializer(root.path());
    let epoch = Instant::now();
    let hook = Hook::new(Some(epoch), move || {
        NOW.with(|now| {
            let current = now.borrow().unwrap();
            let spent = if current == epoch {
                Duration::from_millis(72)
            } else {
                Duration::from_millis(1)
            };
            *now.borrow_mut() = Some(current + spent);
        });
    });
    let policy = settings::VectorStoreSettings {
        busy_timeout: Duration::from_millis(73),
        ..settings::VectorStoreSettings::owner_defaults()
    };
    let error = VectorStore::acquire(root.path(), &policy).err().unwrap();
    assert!(busy(&error));
    assert_eq!(
        Hook::timeouts(),
        vec![73, 1],
        "SQLite's first72ms leaves1ms, and expiry permits no third attempt"
    );
    drop(hook);
    assert_eq!(
        initializer
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    initializer.execute_batch("ROLLBACK;").unwrap();
    drop(initializer);
    let owner = VectorStore::acquire(root.path(), &policy).unwrap();
    assert_eq!(
        owner
            .connect()
            .unwrap()
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
}

#[test]
fn non_busy_journal_failure_is_not_retried_or_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let bytes = vec![b'x'; 1024];
    std::fs::write(root.path().join(VECTOR_DB_FILE), &bytes).unwrap();
    let hook = Hook::new(Some(Instant::now()), || {
        panic!("non-BUSY must not enter retry observation")
    });
    let error = VectorCache::open(root.path()).err().unwrap();
    assert!(
        matches!(&error, Error::Internal { message, .. } if message == "set vector cache journal")
    );
    assert!(
        matches!(std::error::Error::source(&error).and_then(|source| source.downcast_ref::<rusqlite::Error>()), Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::NotADatabase && code.extended_code == 26)
    );
    assert_eq!(
        Hook::timeouts(),
        vec![
            i64::try_from(
                settings::VectorStoreSettings::owner_defaults()
                    .busy_timeout
                    .as_millis()
            )
            .unwrap()
        ]
    );
    drop(hook);
    assert_eq!(
        std::fs::read(root.path().join(VECTOR_DB_FILE)).unwrap(),
        bytes
    );
}

#[derive(Debug, PartialEq)]
struct Stored {
    vectors: Vec<(String, String, i64, Vec<u8>, i64)>,
    leases: Vec<(i64, String, String, i64)>,
    application: i32,
}
fn stored(conn: &Connection) -> Stored {
    let vectors=conn.prepare("SELECT model_id,chunk_hash,dim,vector,used_at FROM vector_cache ORDER BY model_id,chunk_hash").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    let leases=conn.prepare("SELECT lease_id,model_id,chunk_hash,expires_at FROM vector_cache_leases ORDER BY lease_id,model_id,chunk_hash").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    Stored {
        vectors,
        leases,
        application: conn
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .unwrap(),
    }
}
fn seeded(root: &Path) -> (JobPinAuthority, String) {
    let hash = chunk_hash("preserved vector");
    let mut cache = VectorCache::open(root).unwrap();
    cache.put("model", &hash, &[1., 2.]).unwrap();
    cache
        .pin(
            7,
            "model",
            std::slice::from_ref(&hash),
            Duration::from_hours(1),
        )
        .unwrap();
    drop(cache);
    let mut pin = JobPinAuthority::allocate(
        root,
        VectorCacheSource::Legacy(CacheLimits::default()),
        "model".into(),
        Duration::from_hours(1),
        1,
    )
    .unwrap();
    pin.track(&hash).unwrap();
    pin.renew().unwrap();
    (pin, hash)
}
#[test]
fn failed_owned_connect_preserves_vectors_leases_and_persistent_job_pin_sequence() {
    let root = tempfile::tempdir().unwrap();
    let (pin, hash) = seeded(root.path());
    let policy = settings::VectorStoreSettings {
        busy_timeout: Duration::from_millis(73),
        ..settings::VectorStoreSettings::owner_defaults()
    };
    let owner = VectorStore::acquire(root.path(), &policy).unwrap();
    let held = Connection::open(root.path().join(VECTOR_DB_FILE)).unwrap();
    held.pragma_update(None, "journal_mode", "DELETE").unwrap();
    let before = stored(&held);
    held.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let epoch = Instant::now();
    let hook = Hook::new(Some(epoch), move || {
        NOW.with(|now| *now.borrow_mut() = Some(epoch + Duration::from_millis(73)));
    });
    let error = owner.connect().err().unwrap();
    assert!(busy(&error));
    assert_eq!(Hook::timeouts(), vec![73]);
    drop(hook);
    assert_eq!(
        stored(&held),
        before,
        "refused initialization does not change values, recency, leases, DBJ1 marker or counter"
    );
    assert!(
        matches!(VectorCache::open(root.path()), Err(Error::Admission { .. })),
        "failed owned connection retains the existing strict owner"
    );
    held.execute_batch("ROLLBACK;").unwrap();
    drop(held);
    let mut cache = owner.connect().unwrap();
    assert_eq!(stored(&cache.conn), before);
    assert_eq!(
        cache
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
    assert_eq!(cache.get("model", &hash).unwrap(), Some(vec![1., 2.]));
    drop(owner);
    assert!(
        matches!(VectorCache::open(root.path()), Err(Error::Admission { .. })),
        "live owned connection retains mode ownership"
    );
    drop(cache);
    drop(pin);
    let cache = VectorCache::open(root.path()).unwrap();
    let after = stored(&cache.conn);
    assert_eq!(after.application, before.application);
    assert_eq!(
        after.leases.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![i64::MIN, 7]
    );
    drop(cache);
    let next = JobPinAuthority::allocate(
        root.path(),
        VectorCacheSource::Legacy(CacheLimits::default()),
        "model".into(),
        Duration::from_hours(1),
        0,
    )
    .unwrap();
    let cache = VectorCache::open(root.path()).unwrap();
    let counter: String = cache
        .conn
        .query_row(
            "SELECT chunk_hash FROM vector_cache_leases WHERE lease_id=?1",
            [i64::MIN],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        counter, "-2",
        "constructor refusal/retry never resets or reuses signed job ids"
    );
    drop(cache);
    drop(next);
}
