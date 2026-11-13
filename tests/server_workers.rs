//! Application worker admission; deterministic polling never uses elapsed-time gates.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;
use std::time::Instant;

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::resolve_component;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::daemon::settings::{DaemonPatch, DaemonSpec};
use docsbase_memory::daemon::worker_admission::WorkerAdmission;
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::search::operation::SearchOperation;

fn admission(pending: usize) -> WorkerAdmission {
    let settings = resolve_component::<DaemonSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            DaemonPatch {
                search_active: Some(1),
                search_pending: Some(pending),
                control_active: Some(1),
                control_pending: Some(pending),
                ..DaemonPatch::default()
            },
        )],
    )
    .unwrap();
    WorkerAdmission::with_settings(settings.settings()).unwrap()
}

fn operation() -> SearchOperation {
    SearchOperation::new(
        docsbase_memory::search::settings::standalone()
            .unwrap()
            .settings(),
        Instant::now(),
        Cancellation::new(),
    )
    .unwrap()
}

async fn pending(future: impl Future) {
    let mut future = pin!(future);
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test]
async fn zero_pending_admits_free_active_and_refuses_busy() {
    let admission = admission(0);
    let first = admission.search(&operation()).await.unwrap();
    assert!(admission.search(&operation()).await.is_err());
    let control = admission.control().await.unwrap();
    assert!(admission.control().await.is_err());
    drop((first, control));
    assert!(admission.search(&operation()).await.is_ok());
    assert!(admission.control().await.is_ok());
}

#[tokio::test]
async fn bounded_pending_keeps_fifo_and_drop_retires_waiter() {
    let admission = admission(2);
    let first = admission.search(&operation()).await.unwrap();
    let op_a = operation();
    let op_b = operation();
    let mut a = Box::pin(admission.search(&op_a));
    let mut b = Box::pin(admission.search(&op_b));
    pending(&mut a).await;
    pending(&mut b).await;
    assert!(admission.search(&operation()).await.is_err());
    drop(first);
    pending(&mut b).await;
    let admitted = a.await.unwrap();
    pending(&mut b).await;
    drop(b);
    let op_c = operation();
    let mut c = Box::pin(admission.search(&op_c));
    pending(&mut c).await;
    drop(admitted);
    assert!(c.await.is_ok());
}

#[tokio::test]
async fn cancellation_before_and_during_queue_never_admits() {
    let admission = admission(1);
    let cancelled = operation();
    cancelled.cancellation().cancel();
    assert!(admission.search(&cancelled).await.is_err());
    let first = admission.search(&operation()).await.unwrap();
    let operation = operation();
    let deadline = operation.deadline();
    let mut queued = Box::pin(admission.search(&operation));
    pending(&mut queued).await;
    operation.cancellation().cancel();
    drop(first);
    assert!(queued.await.is_err());
    assert_eq!(operation.deadline(), deadline);
    assert!(admission.search(&self::operation()).await.is_ok());
}

#[tokio::test]
async fn shutdown_retires_queued_control_and_search() {
    let admission = admission(1);
    let first = admission.search(&operation()).await.unwrap();
    let control = admission.control().await.unwrap();
    let operation = operation();
    let mut search = Box::pin(admission.search(&operation));
    let mut queued_control = Box::pin(admission.control());
    pending(&mut search).await;
    pending(&mut queued_control).await;
    admission.shutdown();
    assert!(search.await.is_err());
    assert!(queued_control.await.is_err());
    drop((first, control));
    assert!(admission.control().await.is_err());
}

#[tokio::test]
async fn async_cancellation_observes_permanent_state_and_wakes_all_waiters() {
    let cancelled = Cancellation::new();
    cancelled.cancel();
    cancelled.cancelled().await;
    let token = Cancellation::new();
    let mut a = Box::pin(token.cancelled());
    let mut b = Box::pin(token.cancelled());
    pending(&mut a).await;
    pending(&mut b).await;
    drop(a);
    token.cancel();
    b.await;
    token.cancelled().await;
}

#[tokio::test]
async fn expired_original_operation_never_enters_worker() {
    let admission = admission(1);
    let policy = docsbase_memory::search::settings::standalone().unwrap();
    let started =
        Instant::now() - std::time::Duration::from_millis(policy.settings().request_timeout_ms);
    let operation = SearchOperation::new(policy.settings(), started, Cancellation::new()).unwrap();
    let deadline = operation.deadline();
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Ok(permit) = admission.search(&operation).await {
        let observed = std::sync::Arc::clone(&entered);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .await
        .unwrap();
    }
    assert!(!entered.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(operation.deadline(), deadline);
    assert!(operation.cancellation().is_cancelled());
    assert!(admission.search(&self::operation()).await.is_ok());
}

#[tokio::test]
async fn dropped_caller_keeps_physical_closure_permit_until_finish() {
    use std::sync::mpsc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    let admission = admission(0);
    let permit = admission.search(&operation()).await.unwrap();
    let (entered, started) = oneshot::channel();
    let (release, proceed) = mpsc::channel();
    let (finished, complete) = oneshot::channel();
    let caller = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            proceed.recv_timeout(Duration::from_mins(5)).unwrap();
            drop(permit);
            let _ = finished.send(());
        })
        .await
        .unwrap();
    });
    started.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let refused = admission.search(&operation()).await.is_err();
    release.send(()).unwrap();
    complete.await.unwrap();
    assert!(refused);
    assert!(admission.search(&operation()).await.is_ok());
}

#[test]
fn application_control_enters_with_search_and_sync_occupied() {
    use std::sync::mpsc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    // The daemon owner's checked inventory for S=C=1 is B=1+1+1+1.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let admission = admission(0);
        let permit = admission.search(&operation()).await.unwrap();
        let (search_release, search_proceed) = mpsc::channel();
        let (sync_release, sync_proceed) = mpsc::channel();
        let (search_entered, search_started) = oneshot::channel();
        let (sync_entered, sync_started) = oneshot::channel();
        let search = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            search_entered.send(()).unwrap();
            search_proceed.recv_timeout(Duration::from_mins(5)).unwrap();
        });
        let sync = tokio::task::spawn_blocking(move || {
            sync_entered.send(()).unwrap();
            sync_proceed.recv_timeout(Duration::from_mins(5)).unwrap();
        });
        search_started.await.unwrap();
        sync_started.await.unwrap();
        let permit = admission.control().await.unwrap();
        let control = tokio::task::spawn_blocking(move || {
            let _permit = permit;
        });
        // Await actual control completion before releasing either occupied worker.
        control.await.unwrap();
        search_release.send(()).unwrap();
        sync_release.send(()).unwrap();
        search.await.unwrap();
        sync.await.unwrap();
    });
}
