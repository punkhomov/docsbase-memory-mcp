//! Retry/coalescing policy of the watcher consumer loop (flake #28):
//! transient failures retry with backoff, lease conflicts retry unbounded
//! outside the transient budget, permanent failures and vanished roots stop
//! immediately, and retries coalesce paths that arrive during backoff.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use docsbase_memory::error::Error;
use docsbase_memory::index::job::JobStats;
use docsbase_memory::watch::{
    Batch, BatchEvent, BatchOutcome, BatchRetryPolicy, run_pending_batch,
};

fn batch(paths: &[&str]) -> Batch {
    let mut batch = Batch::new();
    for path in paths {
        batch.push(PathBuf::from(path));
    }
    batch
}

fn policy() -> BatchRetryPolicy {
    BatchRetryPolicy {
        max_transient_attempts: 3,
        lease_retry: Duration::from_millis(1),
        transient_backoff: Duration::from_millis(10),
    }
}

fn lease_error() -> Error {
    Error::Project {
        message: "another writer holds the lease".to_owned(),
        instruction: None,
    }
}

#[test]
fn transient_failures_retry_with_backoff_then_drop() {
    let mut pending = batch(&["a.md"]);
    let mut calls = 0_u32;
    let mut delays = Vec::new();
    let mut events = Vec::new();
    let outcome = run_pending_batch(
        &mut pending,
        &policy(),
        |_| {
            calls += 1;
            Err(Error::internal("boom"))
        },
        |_| {},
        |delay| delays.push(delay),
        || false,
        &mut |event| events.push(event),
    );
    assert_eq!(calls, 4, "one initial attempt plus three retries");
    assert_eq!(
        delays,
        vec![
            Duration::from_millis(10),
            Duration::from_millis(30),
            Duration::from_millis(90)
        ],
        "backoff grows x3 per attempt"
    );
    assert!(
        matches!(outcome, BatchOutcome::Dropped { attempts: 3, .. }),
        "{outcome:?}"
    );
    let retries = events
        .iter()
        .filter(|event| matches!(event, BatchEvent::TransientRetry { .. }))
        .count();
    assert_eq!(retries, 3, "{events:?}");
    assert!(
        matches!(
            events.last(),
            Some(BatchEvent::DroppedTransient { attempts: 3, .. })
        ),
        "{events:?}"
    );
}

#[test]
fn retry_coalesces_paths_arriving_during_backoff() {
    let mut pending = batch(&["a.md"]);
    let mut calls: Vec<Vec<PathBuf>> = Vec::new();
    let slept = Cell::new(false);
    let arrived = Cell::new(false);
    let outcome = run_pending_batch(
        &mut pending,
        &policy(),
        |paths| {
            calls.push(paths.to_vec());
            if calls.len() == 1 {
                Err(Error::internal("boom"))
            } else {
                Ok(JobStats::default())
            }
        },
        |pending| {
            if slept.get() && !arrived.get() {
                arrived.set(true);
                pending.push(PathBuf::from("b.md"));
            }
        },
        |_| slept.set(true),
        || false,
        &mut |_| {},
    );
    assert!(matches!(outcome, BatchOutcome::Done(_)), "{outcome:?}");
    assert_eq!(calls.len(), 2, "no extra run for the coalesced path");
    assert_eq!(calls[0], vec![PathBuf::from("a.md")]);
    assert_eq!(
        calls[1],
        vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
        "the retry must see both paths"
    );
}

#[test]
fn lease_conflicts_do_not_consume_the_transient_budget() {
    let mut pending = batch(&["a.md"]);
    let mut calls = 0_u32;
    let mut lease_events = 0_u32;
    let outcome = run_pending_batch(
        &mut pending,
        &policy(),
        |_| {
            calls += 1;
            match calls {
                1..=5 => Err(lease_error()),
                6 => Err(Error::internal("boom")),
                _ => Ok(JobStats::default()),
            }
        },
        |_| {},
        |_| {},
        || false,
        &mut |event| {
            if matches!(event, BatchEvent::LeaseRetry { .. }) {
                lease_events += 1;
            }
        },
    );
    assert!(matches!(outcome, BatchOutcome::Done(_)), "{outcome:?}");
    assert_eq!(calls, 7, "5 lease waits + 1 transient retry + success");
    assert_eq!(lease_events, 5, "lease retries are reported, not counted");
}

#[test]
fn permanent_failures_drop_immediately() {
    let mut pending = batch(&["a.md"]);
    let mut calls = 0_u32;
    let mut events = Vec::new();
    let outcome = run_pending_batch(
        &mut pending,
        &policy(),
        |_| {
            calls += 1;
            Err(Error::Project {
                message: "needs a full rebuild".to_owned(),
                instruction: None,
            })
        },
        |_| {},
        |_| {},
        || false,
        &mut |event| events.push(event),
    );
    assert_eq!(calls, 1, "no retries for permanent failures");
    assert!(
        matches!(outcome, BatchOutcome::Dropped { attempts: 0, .. }),
        "{outcome:?}"
    );
    assert!(
        matches!(events.as_slice(), [BatchEvent::DroppedPermanent { .. }]),
        "{events:?}"
    );
}

#[test]
fn root_missing_stops_before_running() {
    let mut pending = batch(&["a.md"]);
    let mut calls = 0_u32;
    let outcome = run_pending_batch(
        &mut pending,
        &policy(),
        |_| {
            calls += 1;
            Ok(JobStats::default())
        },
        |_| {},
        |_| {},
        || true,
        &mut |_| {},
    );
    assert_eq!(calls, 0, "a vanished root must not run the batch");
    assert!(matches!(outcome, BatchOutcome::RootMissing), "{outcome:?}");
}
