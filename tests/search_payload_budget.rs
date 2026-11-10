//! Search-owned policy resolution and byte admission before payload allocation.

use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::time::Duration;

use docsbase_memory::config::Config;
use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::{global_from, resolve_component};
use docsbase_memory::config::schema::{Scope, SourceId, Unit};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::search::budget::PayloadBudget;
use docsbase_memory::search::settings::{SearchPatch, SearchSettings, SearchSpec};

fn settings(request: u64, retained: u64) -> SearchSettings {
    SearchSettings {
        max_request_payload_bytes: request,
        max_retained_payload_bytes: retained,
    }
}

fn budget(request: u64, retained: u64) -> PayloadBudget {
    PayloadBudget::new(settings(request, retained)).expect("valid budget")
}

#[test]
fn missing_search_fields_are_resolved_by_the_config_owner() {
    let config_dir = tempfile::tempdir().expect("config");
    let resolved = global_from(config_dir.path()).expect("config");
    assert_eq!(
        resolved.settings().search,
        settings(64 * 1024 * 1024, 256 * 1024 * 1024)
    );
    for field in [
        "search.max_request_payload_bytes",
        "search.max_retained_payload_bytes",
    ] {
        let origin = resolved.provenance().get(field).expect("declared origin");
        assert_eq!(origin.source, SourceId::Defaults);
        assert_eq!(origin.scope, Scope::Daemon);
        assert_eq!(origin.unit, Unit::Bytes);
    }
}

#[test]
fn partial_global_search_policy_preserves_the_other_owner_default() {
    let config_dir = tempfile::tempdir().expect("config");
    let path = config_dir.path().join("config.toml");
    std::fs::write(&path, "[search]\nmax_request_payload_bytes = 4096\n").expect("config");
    let resolved = global_from(config_dir.path()).expect("config");
    assert_eq!(
        resolved.settings().search,
        settings(4096, 256 * 1024 * 1024)
    );
    assert_eq!(
        resolved
            .provenance()
            .get("search.max_request_payload_bytes")
            .expect("origin")
            .source,
        SourceId::Global(path)
    );
    assert_eq!(
        resolved
            .provenance()
            .get("search.max_retained_payload_bytes")
            .expect("origin")
            .source,
        SourceId::Defaults
    );
}

#[test]
fn file_policy_reaches_the_budget_without_a_consumer_fallback() {
    let config_dir = tempfile::tempdir().expect("config");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "[search]\nmax_request_payload_bytes = 8\nmax_retained_payload_bytes = 12\n",
    )
    .expect("config");
    let config = Config::global_from(config_dir.path()).expect("config");
    assert_eq!(config.search, settings(8, 12));
    let budget = PayloadBudget::new(config.search).expect("configured budget");
    let _first = budget.reserve(8).expect("request cap");
    let _second = budget.reserve(4).expect("remaining aggregate");
    assert!(matches!(budget.reserve(1), Err(Error::Admission { .. })));
    assert!(matches!(budget.reserve(9), Err(Error::Admission { .. })));
}

#[test]
fn project_files_cannot_change_daemon_search_policy() {
    let config_dir = tempfile::tempdir().expect("config");
    let project = tempfile::tempdir().expect("project");
    std::fs::write(
        project.path().join(".docsbase.toml"),
        "[search]\nmax_request_payload_bytes = 8\n",
    )
    .expect("project config");
    assert!(Config::load_from(config_dir.path(), Some(project.path())).is_err());
    assert!(
        resolve_component::<SearchSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Project(project.path().into()),
                SearchPatch {
                    max_request_payload_bytes: Some(8),
                    ..SearchPatch::default()
                }
            )],
        )
        .is_err()
    );
}

#[test]
fn unknown_search_keys_and_invalid_values_are_rejected() {
    let config_dir = tempfile::tempdir().expect("config");
    for table in [
        "max_payload_bytes = 8",
        "max_request_payload_bytes = -1",
        "max_request_payload_bytes = 0",
        "max_retained_payload_bytes = 0",
        "max_request_payload_bytes = 12\nmax_retained_payload_bytes = 8",
    ] {
        std::fs::write(
            config_dir.path().join("config.toml"),
            format!("[search]\n{table}\n"),
        )
        .expect("config");
        assert!(Config::global_from(config_dir.path()).is_err(), "{table}");
    }
}

#[test]
fn imported_invalid_policy_is_rejected_before_budget_creation() {
    for settings in [settings(0, 12), settings(8, 0), settings(12, 8)] {
        let config = Config {
            search: settings.clone(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
        assert!(matches!(
            PayloadBudget::new(settings),
            Err(Error::Admission { .. })
        ));
    }
}

#[test]
fn legacy_config_json_keeps_its_existing_shape() {
    let default = Config::default();
    let config = Config {
        search: settings(8, 12),
        ..default.clone()
    };
    assert_eq!(
        docsbase_memory::cli::config_payload(&config),
        docsbase_memory::cli::config_payload(&default)
    );
}

#[test]
fn request_and_aggregate_boundaries_do_not_leak_refused_charge() {
    let budget = budget(8, 12);
    let first = budget.reserve(8).expect("first");
    assert!(budget.reserve(9).is_err());
    let second = budget.reserve(4).expect("aggregate boundary");
    assert!(budget.reserve(1).is_err());
    let _empty = budget.reserve(0).expect("empty payload");
    drop(first);
    let _replacement = budget
        .reserve(8)
        .expect("refusals did not consume capacity");
    drop(second);
    let _remaining = budget.reserve(4).expect("released capacity");
}

#[test]
fn budget_clones_share_retained_capacity() {
    let original = budget(8, 12);
    let shared = original.clone();
    let first = original.reserve(8).expect("first");
    assert!(shared.reserve(8).is_err());
    let second = shared.reserve(4).expect("shared remainder");
    drop(first);
    let _replacement = shared.reserve(8).expect("shared release");
    assert!(original.reserve(1).is_err());
    drop(second);
}

#[test]
fn error_and_unwind_release_payload_charge() {
    fn fail(budget: &PayloadBudget) -> Result<()> {
        let _permit = budget.reserve(8)?;
        Err(Error::internal("capture failed"))
    }
    let budget = budget(8, 8);
    assert!(fail(&budget).is_err());
    drop(budget.reserve(8).expect("error released"));
    let caught = std::panic::catch_unwind(|| {
        let _permit = budget.reserve(8).expect("permit");
        panic!("capture unwind");
    });
    assert!(caught.is_err());
    drop(budget.reserve(8).expect("unwind released"));
}

#[test]
fn payload_charge_can_be_released_on_another_thread() {
    let budget = budget(8, 8);
    let permit = budget.reserve(8).expect("permit");
    assert!(budget.reserve(1).is_err());
    std::thread::spawn(move || drop(permit))
        .join()
        .expect("release thread");
    drop(budget.reserve(8).expect("cross-thread release"));
}

#[test]
fn concurrent_reservations_cannot_exceed_the_shared_aggregate() {
    let budget = budget(8, 16);
    std::thread::scope(|scope| {
        let start = Arc::new(Barrier::new(4));
        let (done, completed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Arc::new(Mutex::new(released));
        let mut workers = Vec::new();
        for _ in 0..3 {
            let budget = budget.clone();
            let start = Arc::clone(&start);
            let done = done.clone();
            let released = Arc::clone(&released);
            workers.push(scope.spawn(move || {
                start.wait();
                let permit = budget.reserve(8);
                done.send(permit.is_ok()).expect("attempt completed");
                let _ = released.lock().expect("release gate").recv();
                drop(permit);
            }));
        }
        start.wait();
        let accepted = (0..3)
            .filter(|_| {
                completed
                    .recv_timeout(Duration::from_secs(300))
                    .expect("reservation returned")
            })
            .count();
        assert_eq!(accepted, 2, "only two full payloads fit");
        assert!(budget.reserve(1).is_err());
        for _ in 0..3 {
            release.send(()).expect("release");
        }
        for worker in workers {
            worker.join().expect("worker");
        }
    });
    let _first = budget.reserve(8).expect("all workers released");
    let _second = budget.reserve(8).expect("full capacity returned");
}

#[test]
fn retained_charge_uses_checked_arithmetic_without_allocating_payload() {
    let budget = budget(u64::MAX, u64::MAX);
    let whole = budget.reserve(u64::MAX).expect("logical charge only");
    assert!(budget.reserve(1).is_err(), "overflow is refusal");
    drop(whole);
    drop(
        budget
            .reserve(u64::MAX)
            .expect("overflow did not leak charge"),
    );
}
