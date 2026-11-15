use super::*;
use crate::daemon::request::ActivitySource;
use std::future::Future;
use std::task::{Context, Poll, Waker};

fn operation() -> SearchOperation {
    SearchOperation::with_timeout(
        std::time::Instant::now(),
        std::time::Duration::from_secs(60),
        Cancellation::new(),
    )
    .unwrap()
}
#[test]
fn abandoned_registration_keeps_logical_and_activity_until_reserved_terminal_after_shutdown() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let workers = Arc::new(WorkerAdmission::with_settings(policy.settings()).unwrap());
    let budget = PayloadBudget::new(
        crate::search::settings::standalone()
            .unwrap()
            .into_settings(),
    )
    .unwrap();
    let (activity, _events) = ActivitySource::channel(8);
    let mut root = JobRoot::new(policy.settings());
    let service = root.service();
    let call = operation();
    {
        let _entered = runtime.enter();
        let mut admit = Box::pin(service.admit(workers.clone(), activity.track(), &call, &budget));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(admit.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(
            root.registrations.len(),
            1,
            "receiver registration precedes acceptance/native capture"
        );
        drop(admit);
    }
    assert!(
        !service.inner.logical.is_idle(),
        "abandoning ack wait cannot release registered L"
    );
    assert_eq!(
        activity.counter.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    workers.shutdown();
    root.run(&runtime, async { Ok(()) }).unwrap();
    assert!(service.inner.logical.is_idle());
    assert_eq!(
        activity.counter.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    runtime.block_on(async {
        assert!(
            service
                .admit(workers, activity.track(), &operation(), &budget)
                .await
                .is_err(),
            "closed root accepts no new job/native state"
        );
    });
}
#[test]
fn multiple_registered_tickets_retire_serially_with_ordinary_admission_closed() {
    for active in [1, 3] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut policy = crate::daemon::settings::standalone()
            .unwrap()
            .into_settings();
        policy.background_active = active;
        let workers = Arc::new(WorkerAdmission::with_settings(&policy).unwrap());
        let budget = PayloadBudget::new(
            crate::search::settings::standalone()
                .unwrap()
                .into_settings(),
        )
        .unwrap();
        let (activity, _events) = ActivitySource::channel(8);
        let mut root = JobRoot::new(&policy);
        let service = root.service();
        let retired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        service.observe_retirement(Arc::new({
            let retired = retired.clone();
            move |stage| {
                if stage == "after_index" {
                    retired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }));
        root.run(&runtime, async {
            let mut owners = Vec::new();
            for _ in 0..active {
                owners.push(
                    service
                        .admit(workers.clone(), activity.track(), &operation(), &budget)
                        .await
                        .unwrap(),
                );
            }
            assert_eq!(
                activity.counter.load(std::sync::atomic::Ordering::SeqCst),
                active
            );
            workers.shutdown();
            drop(owners);
            Ok(())
        })
        .unwrap();
        assert_eq!(retired.load(std::sync::atomic::Ordering::SeqCst), active);
        assert!(service.inner.logical.is_idle());
        assert_eq!(
            activity.counter.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
