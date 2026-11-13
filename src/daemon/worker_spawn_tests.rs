//! Exercise the same closure owner used by request, sync and janitor dispatch.

use super::*;
use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::SourceId;
use crate::daemon::settings::{DaemonPatch, DaemonSpec};
use crate::daemon::worker_admission::WorkerAdmission;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::oneshot;

fn admission() -> WorkerAdmission {
    let policy = resolve_component::<DaemonSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            DaemonPatch {
                control_active: Some(1),
                control_pending: Some(0),
                ..DaemonPatch::default()
            },
        )],
    )
    .unwrap();
    WorkerAdmission::with_settings(policy.settings()).unwrap()
}

#[tokio::test]
async fn detached_worker_retains_permit_and_activity_through_actual_finish() {
    let admission = admission();
    let permit = admission.control().await.unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let (events, mut observed) = mpsc::channel(1);
    let activity = Activity::new(&count, &events, None);
    let (entered, started) = oneshot::channel();
    let (release, proceed) = std_mpsc::channel();
    let worker = spawn(Some(permit), Some(Arc::clone(&activity)), move || {
        entered.send(()).unwrap();
        proceed.recv_timeout(Duration::from_mins(5)).unwrap();
        Ok(())
    });
    started.await.unwrap();
    drop(worker);
    drop(activity);
    let charged = count.load(Ordering::SeqCst);
    let refused = admission.control().await.is_err();
    release.send(()).unwrap();
    observed.recv().await.unwrap();
    assert_eq!(charged, 1);
    assert!(refused);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(admission.control().await.is_ok());
}

#[tokio::test]
async fn worker_unwind_releases_permit_and_activity() {
    let admission = admission();
    let count = Arc::new(AtomicUsize::new(0));
    let (events, _) = mpsc::channel(1);
    let activity = Activity::new(&count, &events, None);
    let worker = spawn(
        Some(admission.control().await.unwrap()),
        Some(activity),
        || {
            panic!("injected closure unwind");
            #[allow(unreachable_code)]
            Ok::<(), Error>(())
        },
    );
    assert!(worker.await.unwrap_err().is_panic());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(admission.control().await.is_ok());
}
