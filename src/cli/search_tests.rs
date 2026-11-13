//! Synchronous CLI bridge behavior without host timing assumptions.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::error::Error;
use crate::hybrid::work::Cancellation;
use crate::search::operation::SearchOperation;

fn operation(started: Instant) -> SearchOperation {
    let settings = crate::search::settings::standalone().expect("owner policy");
    SearchOperation::new(settings.settings(), started, Cancellation::new()).expect("operation")
}

#[test]
fn plain_thread_bridge_returns_work_result() {
    assert_eq!(
        super::block_on_io(async { Ok(7) }).expect("plain bridge"),
        7
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multithread_runtime_bridge_does_not_nest_an_async_executor() {
    assert_eq!(
        super::block_on_io(async { Ok(7) }).expect("blocking bridge"),
        7
    );
}

#[tokio::test]
async fn current_thread_runtime_refuses_sync_bridge_before_polling_work() {
    let polled = Arc::new(AtomicBool::new(false));
    let work = Arc::clone(&polled);
    let result = super::block_on_io(async move {
        work.store(true, Ordering::SeqCst);
        Ok(7)
    });
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(!polled.load(Ordering::SeqCst));
}

#[test]
fn expired_cli_operation_cannot_turn_missing_endpoint_into_direct_fallback() {
    let cache = tempfile::tempdir().expect("missing endpoint cache");
    let settings = crate::search::settings::standalone().expect("owner policy");
    let started = Instant::now()
        .checked_sub(Duration::from_millis(
            settings.settings().request_timeout_ms,
        ))
        .expect("expired start");
    let operation = operation(started);
    let result = super::try_daemon_search(
        cache.path(),
        std::path::Path::new("."),
        json!({"query":"widgets"}),
        &operation,
    );
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(operation.cancellation().is_cancelled());
}

#[test]
fn missing_endpoint_leaves_original_operation_live_for_direct_search() {
    let cache = tempfile::tempdir().expect("missing endpoint cache");
    let mut settings = crate::search::settings::standalone()
        .expect("owner policy")
        .into_settings();
    settings.request_timeout_ms = 300_000; // safety policy, not a latency assertion
    let started = Instant::now()
        .checked_sub(Duration::from_millis(12))
        .expect("original ingress");
    let operation =
        SearchOperation::new(&settings, started, Cancellation::new()).expect("operation");
    let result = super::try_daemon_search(
        cache.path(),
        std::path::Path::new("."),
        json!({"query":"widgets"}),
        &operation,
    )
    .expect("direct fallback");
    assert!(result.is_none());
    assert_eq!(operation.started_at(), started);
    assert_eq!(
        operation.deadline().duration_since(started),
        Duration::from_millis(settings.request_timeout_ms)
    );
    assert!(!operation.cancellation().is_cancelled());
}

#[test]
fn direct_cache_constructor_consumes_supplied_query_policy() {
    let root = tempfile::tempdir().unwrap();
    let query = crate::hybrid::dense_settings::QueryCacheSettings {
        max_entries: 1,
        max_bytes: 16,
    };
    let dense = super::direct_dense_cache(root.path(), 1024, &query).unwrap();
    dense.store_query_vector("model", "a", &[1., 2.]);
    dense.store_query_vector("model", "b", &[3., 4.]);
    assert!(dense.query_vector("model", "a").is_none());
    assert!(dense.query_vector("model", "b").is_some());
    assert_eq!(dense.query_cache_stats().entries, 1);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
