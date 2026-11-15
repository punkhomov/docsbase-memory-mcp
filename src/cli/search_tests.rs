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
    let _env = crate::cli::test_env::env_guard();
    assert_eq!(
        crate::cli::command::run(crate::config::Config::default(), |_| async { Ok(7) }).unwrap(),
        7
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multithread_runtime_bridge_does_not_nest_an_async_executor() {
    let env_guard = crate::cli::test_env::env_guard();
    for failure in [0, 1, 2] {
        let result = crate::cli::command::run(crate::config::Config::default(), |_| {
            assert_eq!(
                tokio::runtime::Handle::current().runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread,
                "work construction stays outside command block_on"
            );
            async move {
                assert_eq!(
                    tokio::runtime::Handle::current().runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::CurrentThread,
                    "actual command work owns one explicit runtime"
                );
                match failure {
                    0 => Ok(7),
                    1 => Err(Error::internal("actual command error")),
                    _ => panic!("actual command unwind"),
                }
            }
        });
        if failure == 0 {
            assert_eq!(result.unwrap(), 7);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        );
    }
    drop(env_guard);
    assert_eq!(
        tokio::spawn(async { 9 }).await.unwrap(),
        9,
        "ambient executor remains live after actual command runtime destruction"
    );
}

#[tokio::test]
async fn current_thread_runtime_refuses_sync_bridge_before_polling_work() {
    let _env = crate::cli::test_env::env_guard();
    let polled = Arc::new(AtomicBool::new(false));
    let work = polled.clone();
    let result = crate::cli::command::run(crate::config::Config::default(), move |_| async move {
        work.store(true, Ordering::SeqCst);
        Ok(7)
    });
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(!polled.load(Ordering::SeqCst));
}

#[test]
fn expired_cli_operation_cannot_turn_missing_endpoint_into_direct_fallback() {
    let _env = crate::cli::test_env::env_guard();
    let cache = tempfile::tempdir().unwrap();
    let settings = crate::search::settings::standalone().unwrap();
    let started = Instant::now()
        .checked_sub(Duration::from_millis(
            settings.settings().request_timeout_ms,
        ))
        .unwrap();
    let operation = operation(started);
    let observed = operation.clone();
    let result = crate::cli::command::run(
        crate::config::Config::default(),
        move |mut context| async move {
            context.cache = cache.path().to_path_buf();
            let activity = context.activity.track();
            super::daemon_in(
                &context,
                std::path::Path::new("."),
                json!({"query":"widgets"}),
                &operation,
                &crate::store::settings::standalone().unwrap(),
                &activity,
            )
            .await
        },
    );
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(observed.cancellation().is_cancelled());
}

#[test]
fn missing_endpoint_leaves_original_operation_live_for_direct_search() {
    let _env = crate::cli::test_env::env_guard();
    let cache = tempfile::tempdir().unwrap();
    let mut global = crate::config::Config::default();
    global.search.request_timeout_ms = 300_000;
    let started = Instant::now()
        .checked_sub(Duration::from_millis(12))
        .unwrap();
    let operation = SearchOperation::new(&global.search, started, Cancellation::new()).unwrap();
    let observed = operation.clone();
    let result = crate::cli::command::run(global, move |mut context| async move {
        context.cache = cache.path().to_path_buf();
        let activity = context.activity.track();
        super::daemon_in(
            &context,
            std::path::Path::new("."),
            json!({"query":"widgets"}),
            &operation,
            &crate::store::settings::standalone().unwrap(),
            &activity,
        )
        .await
    })
    .unwrap();
    assert!(result.is_none());
    assert_eq!(observed.started_at(), started);
    assert_eq!(
        observed.deadline().duration_since(started),
        Duration::from_millis(300_000)
    );
    assert!(!observed.cancellation().is_cancelled());
}

#[test]
fn direct_cache_constructor_consumes_supplied_query_policy() {
    let root = tempfile::tempdir().unwrap();
    let mut query = crate::hybrid::dense_settings::standalone()
        .unwrap()
        .into_settings();
    query.query_cache_max_entries = 1;
    query.query_cache_max_bytes = 16;
    let dense = super::direct_dense_cache(root.path(), 1024, &query).unwrap();
    dense.store_query_vector("model", "a", &[1., 2.]);
    dense.store_query_vector("model", "b", &[3., 4.]);
    assert!(dense.query_vector("model", "a").is_none());
    assert!(dense.query_vector("model", "b").is_some());
    assert_eq!(dense.query_cache_stats().entries, 1);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn direct_cache_factory_consumes_resident_policy_and_legacy_vector_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = crate::hybrid::dense_settings::standalone()
        .unwrap()
        .into_settings();
    settings.max_vectors = 7;
    settings.max_bytes = 12_345;
    settings.max_projects = 2;
    settings.min_vectors = 3;
    settings.vector_cache_max_bytes = 99;
    settings.ann_lifetimes = crate::hybrid::dense_settings::AnnLifetimes::new(
        std::time::Duration::from_secs(120),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    let dense = super::direct_dense_cache(root.path(), 1024, &settings).unwrap();
    let actual = dense.limits();
    assert_eq!(actual.max_vectors, 7);
    assert_eq!(actual.max_bytes, 12_345);
    assert_eq!(actual.max_projects, 2);
    assert_eq!(actual.min_vectors, 3);
    assert_eq!(actual.vector_cache_max_bytes, 1024);
    assert_eq!(actual.ann_lifetimes, settings.ann_lifetimes);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
