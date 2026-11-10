use std::time::{Duration, Instant};

use crate::error::Error;
use crate::hybrid::work::Cancellation;
use crate::search::settings::SearchSettings;

use super::{SearchOperation, checked_deadline};

fn policy() -> SearchSettings {
    SearchSettings {
        request_timeout_ms: 37,
        max_request_payload_bytes: 1,
        max_retained_payload_bytes: 2,
    }
}

fn owned<T: Send + Sync + 'static>() {}

#[test]
fn constrained_budget_keeps_original_start_and_the_smaller_deadline() {
    let start = Instant::now();
    let now = start + Duration::from_millis(5);
    for (peer, expected) in [(20, 20), (100, 37)] {
        let operation = SearchOperation::constrained_at(
            &policy(),
            start,
            start + Duration::from_millis(peer),
            Cancellation::new(),
            now,
        )
        .expect("live constrained operation");
        assert_eq!(operation.started_at(), start);
        assert_eq!(
            operation.deadline(),
            start + Duration::from_millis(expected)
        );
        assert_eq!(
            operation
                .remaining_at(now)
                .expect("original remaining budget"),
            Duration::from_millis(expected - 5)
        );
    }
}

#[test]
fn constrained_budget_refuses_expiry_at_either_original_boundary() {
    let start = Instant::now();
    for (peer, elapsed) in [(20, 20), (100, 37)] {
        let token = Cancellation::new();
        let result = SearchOperation::constrained_at(
            &policy(),
            start,
            start + Duration::from_millis(peer),
            token.clone(),
            start + Duration::from_millis(elapsed),
        );
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert!(token.is_cancelled());
    }
}

#[test]
fn queue_observations_follow_the_original_operation_across_clones() {
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    operation.record_queue(Duration::from_millis(7));
    let worker = operation.clone();
    worker.record_queue(Duration::from_millis(11));
    assert_eq!(operation.queue_ms(), 18);
    assert_eq!(worker.started_at(), start);
    assert_eq!(
        operation.elapsed_at(start + Duration::from_millis(25)),
        Duration::from_millis(25)
    );
    let independent = SearchOperation::new(&policy(), start, Cancellation::new()).expect("other");
    assert_eq!(independent.queue_ms(), 0);
}

#[test]
fn original_start_and_exact_budget_are_retained() {
    owned::<SearchOperation>();
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    assert_eq!(operation.started_at(), start);
    assert_eq!(operation.deadline(), start + Duration::from_millis(37));
    assert_eq!(
        operation.remaining_at(start).expect("active"),
        Duration::from_millis(37)
    );
    assert_eq!(operation.elapsed_at(start), Duration::ZERO);
}

#[test]
fn queue_and_phase_time_consume_one_budget() {
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    let after_queue = start + Duration::from_millis(15);
    assert_eq!(operation.elapsed_at(after_queue), Duration::from_millis(15));
    assert_eq!(
        operation.remaining_at(after_queue).expect("remaining"),
        Duration::from_millis(22)
    );
    let next_phase = start + Duration::from_millis(36);
    assert_eq!(
        operation.remaining_at(next_phase).expect("remaining"),
        Duration::from_millis(1)
    );
}

#[test]
fn exact_deadline_cancels_only_this_operation() {
    let start = Instant::now();
    let token = Cancellation::new();
    let operation = SearchOperation::new(&policy(), start, token.clone()).expect("operation");
    let independent = SearchOperation::new(&policy(), start, Cancellation::new()).expect("other");
    operation
        .check_at(
            operation
                .deadline()
                .checked_sub(Duration::from_nanos(1))
                .expect("one nanosecond before deadline"),
        )
        .expect("before deadline");
    assert!(!token.is_cancelled());
    let error = operation
        .check_at(operation.deadline())
        .expect_err("deadline reached");
    assert!(matches!(error, Error::Admission { .. }));
    assert!(error.to_string().contains("deadline"));
    assert!(token.is_cancelled());
    assert!(operation.cancellation().is_cancelled());
    independent.check_at(start).expect("independent token");
}

#[test]
fn late_check_refuses_work_and_never_returns_zero_as_success() {
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    assert!(matches!(
        operation.remaining_at(operation.deadline()),
        Err(Error::Admission { .. })
    ));
    assert!(
        operation
            .check_at(operation.deadline() + Duration::from_millis(1))
            .is_err()
    );
    assert!(operation.cancellation().is_cancelled());
}

#[test]
fn clones_keep_the_original_deadline_and_share_cancellation() {
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    let retry = operation.clone();
    assert_eq!(retry.started_at(), start);
    assert_eq!(retry.deadline(), operation.deadline());
    assert_eq!(
        retry
            .remaining_at(start + Duration::from_millis(30))
            .expect("remaining"),
        Duration::from_millis(7)
    );
    assert!(retry.check_at(retry.deadline()).is_err());
    assert!(operation.cancellation().is_cancelled());
    assert!(
        operation.check_at(start).is_err(),
        "cancellation is permanent"
    );
}

#[test]
fn caller_cancellation_is_visible_without_waiting_for_the_deadline() {
    let start = Instant::now();
    let token = Cancellation::new();
    let operation = SearchOperation::new(&policy(), start, token.clone()).expect("operation");
    token.cancel();
    assert!(matches!(
        operation.check_at(start),
        Err(Error::Admission { .. })
    ));
    assert!(operation.remaining_at(start).is_err());
}

#[test]
fn invalid_imported_policy_is_rejected_without_cancelling_the_caller() {
    for settings in [
        SearchSettings {
            request_timeout_ms: 0,
            ..policy()
        },
        SearchSettings {
            max_request_payload_bytes: 0,
            ..policy()
        },
        SearchSettings {
            max_retained_payload_bytes: 0,
            ..policy()
        },
        SearchSettings {
            max_request_payload_bytes: 3,
            ..policy()
        },
    ] {
        let token = Cancellation::new();
        assert!(matches!(
            SearchOperation::new(&settings, Instant::now(), token.clone()),
            Err(Error::Admission { .. })
        ));
        assert!(!token.is_cancelled());
    }
}

#[test]
fn platform_deadline_overflow_is_an_explicit_error() {
    let error =
        checked_deadline(Instant::now(), Duration::MAX).expect_err("unrepresentable Instant");
    assert!(matches!(error, Error::Admission { .. }));
    assert!(error.to_string().contains("deadline"));
}

#[test]
fn elapsed_diagnostics_saturate_for_an_earlier_observation() {
    let start = Instant::now();
    let operation = SearchOperation::new(&policy(), start, Cancellation::new()).expect("operation");
    assert_eq!(
        operation.elapsed_at(
            start
                .checked_sub(Duration::from_millis(1))
                .expect("earlier observation"),
        ),
        Duration::ZERO
    );
}
