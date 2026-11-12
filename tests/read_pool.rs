//! Prepared operational readers: actual `SQLite` behavior, without provider/network.
use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::resolve_component;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::store::Db;
use docsbase_memory::store::read_pool::ReadPool;
use docsbase_memory::store::settings::{DbPatch, DbSpec, ReadPoolPatch, ReadPoolSpec};
use std::time::{Duration, Instant};

fn deadline() -> Instant {
    Instant::now() + Duration::from_mins(5) // Deadlock watchdog, not a latency SLA.
}

fn pool(cache: &std::path::Path, capacity: usize) -> ReadPool {
    let context = DefaultsContext::standalone();
    let db = resolve_component::<DbSpec>(
        &context,
        [(
            SourceId::Cli,
            DbPatch {
                read_busy_timeout: Some(Duration::from_millis(29)),
                ..DbPatch::default()
            },
        )],
    )
    .expect("db policy");
    let pool = resolve_component::<ReadPoolSpec>(
        &context,
        [(
            SourceId::Cli,
            ReadPoolPatch {
                max_connections: Some(capacity),
            },
        )],
    )
    .expect("pool policy");
    ReadPool::prepare(cache, &db, &pool).expect("prepare")
}

#[test]
fn prepared_readers_are_distinct_readonly_with_explicit_timeout() {
    let cache = tempfile::tempdir().expect("cache");
    let _writer = Db::open(cache.path()).expect("writer");
    let pool = pool(cache.path(), 2);
    let first = pool.acquire(deadline()).expect("first");
    let second = pool.acquire(deadline()).expect("second");
    assert!(!std::ptr::eq(first.connection(), second.connection()));
    for reader in [&first, &second] {
        let timeout: u32 = reader
            .connection()
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("timeout");
        assert_eq!(timeout, 29);
        assert!(
            reader
                .connection()
                .execute_batch("CREATE TABLE denied (id INTEGER)")
                .is_err()
        );
    }
    assert_eq!(pool.settings().settings().max_connections, 2);
    assert_eq!(
        pool.settings()
            .provenance()
            .get("read_pool.max_connections")
            .map(|origin| &origin.source),
        Some(&SourceId::Cli)
    );
}

#[test]
fn expired_caller_deadline_is_rejected_even_with_idle_reader() {
    let cache = tempfile::tempdir().expect("cache");
    let _writer = Db::open(cache.path()).expect("writer");
    let pool = pool(cache.path(), 1);
    assert!(pool.acquire(Instant::now()).is_err());
    assert!(pool.acquire(deadline()).is_ok());
}

#[test]
fn nonblocking_checkout_preserves_capacity_root_and_transaction_cleanup() {
    let cache = tempfile::tempdir().expect("cache");
    let _writer = Db::open(cache.path()).expect("writer");
    let pool = pool(cache.path(), 1);
    assert_eq!(pool.cache_root(), cache.path());
    let reader = pool.try_acquire().expect("idle prepared reader");
    assert!(matches!(
        pool.try_acquire(),
        Err(docsbase_memory::error::Error::Admission { .. })
    ));
    reader
        .connection()
        .execute_batch("BEGIN; SELECT * FROM projects")
        .expect("read transaction");
    drop(reader);
    let replacement = pool.try_acquire().expect("same capacity returned");
    assert!(replacement.connection().is_autocommit());
    assert!(
        pool.try_acquire().is_err(),
        "one lease still consumes one slot"
    );
}

#[test]
fn lease_returns_after_error_and_unwind() {
    let cache = tempfile::tempdir().expect("cache");
    let _writer = Db::open(cache.path()).expect("writer");
    let pool = pool(cache.path(), 1);
    let failed = || -> Result<(), rusqlite::Error> {
        let reader = pool.acquire(deadline()).expect("reader");
        reader.connection().execute_batch("SELECT * FROM absent")
    };
    assert!(failed().is_err());
    assert!(pool.acquire(deadline()).is_ok());
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = pool.acquire(deadline()).expect("reader");
        reader
            .connection()
            .execute_batch("BEGIN")
            .expect("begin before unwind");
        panic!("caller unwind");
    }));
    assert!(unwind.is_err());
    assert!(
        pool.acquire(deadline())
            .expect("reader after unwind")
            .connection()
            .is_autocommit()
    );
}

#[test]
fn returned_reader_does_not_pin_previous_sql_snapshot() {
    let cache = tempfile::tempdir().expect("cache");
    let writer = Db::open(cache.path()).expect("writer");
    writer
        .connection()
        .execute_batch("CREATE TABLE payload (value INTEGER); INSERT INTO payload VALUES (1)")
        .expect("seed");
    let pool = pool(cache.path(), 1);
    let reader = pool.acquire(deadline()).expect("reader");
    reader.connection().execute_batch("BEGIN").expect("begin");
    let value = || {
        reader
            .connection()
            .query_row("SELECT value FROM payload", [], |r| r.get::<_, i64>(0))
            .expect("read")
    };
    assert_eq!(value(), 1);
    writer
        .connection()
        .execute_batch("UPDATE payload SET value = 2")
        .expect("concurrent commit");
    assert_eq!(value(), 1, "read transaction pins the old snapshot");
    drop(reader);
    let reader = pool.acquire(deadline()).expect("reused reader");
    assert!(reader.connection().is_autocommit());
    assert_eq!(
        reader
            .connection()
            .query_row("SELECT value FROM payload", [], |r| r.get::<_, i64>(0))
            .expect("new snapshot"),
        2
    );
}

#[test]
fn preparation_never_creates_or_migrates_missing_database() {
    let temp = tempfile::tempdir().expect("temp");
    let cache = temp.path().join("absent");
    let db = resolve_component::<DbSpec>(&DefaultsContext::standalone(), []).expect("db");
    let pool = resolve_component::<ReadPoolSpec>(&DefaultsContext::standalone(), []).expect("pool");
    assert!(ReadPool::prepare(&cache, &db, &pool).is_err());
    assert!(!cache.exists());
}
