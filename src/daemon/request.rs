//! Original-deadline waiting and ownership of actual work through reply delivery.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::task::{JoinError, JoinHandle};

use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::search::operation::SearchOperation;

#[cfg(test)]
#[path = "worker_spawn_tests.rs"]
mod worker_spawn_tests;

/// Transfer actual execution ownership, including unwind, into the closure.
/// `None` permits are reserved for the legacy Shared background sync path.
pub(crate) fn spawn<T: Send + 'static>(
    permit: Option<OwnedSemaphorePermit>,
    activity: Option<Arc<Activity>>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> JoinHandle<Result<T>> {
    tokio::task::spawn_blocking(move || {
        // Release capacity before Activity publishes physical completion.
        let _activity = activity;
        let _permit = permit;
        work()
    })
}

/// One charge shared by the async reply and the actual blocking closure.
#[derive(Clone)]
pub(crate) struct ActivitySource {
    pub(crate) counter: Arc<AtomicUsize>,
    pub(crate) events: mpsc::Sender<()>,
    pub(crate) bridge: Option<Arc<super::remote::SyncBridge>>,
}

impl ActivitySource {
    pub(crate) fn channel(capacity: usize) -> (Self, mpsc::Receiver<()>) {
        let (events, receiver) = mpsc::channel(capacity);
        (
            Self {
                counter: Arc::new(AtomicUsize::new(0)),
                events,
                bridge: tokio::runtime::Handle::try_current()
                    .ok()
                    .map(|handle| Arc::new(super::remote::SyncBridge::new(handle))),
            },
            receiver,
        )
    }

    pub(crate) fn with_runtime(
        capacity: usize,
        handle: tokio::runtime::Handle,
    ) -> (Self, mpsc::Receiver<()>) {
        let (mut source, receiver) = Self::channel(capacity);
        source.bridge = Some(Arc::new(super::remote::SyncBridge::new(handle)));
        (source, receiver)
    }

    pub(crate) fn retire_bridge(&self) {
        if let Some(bridge) = &self.bridge {
            bridge.retire();
        }
    }

    pub(crate) fn track(&self) -> Arc<Activity> {
        Activity::new(&self.counter, &self.events, None)
    }

    pub(crate) async fn drain(&self, poll: std::time::Duration) {
        while self.counter.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(poll).await;
        }
    }
}

/// One charge shared by the async reply and the actual blocking closure.
pub(crate) struct Activity {
    counter: Arc<AtomicUsize>,
    events: mpsc::Sender<()>,
    connection: Option<Arc<OwnedSemaphorePermit>>,
}

impl Activity {
    #[must_use]
    pub(crate) fn new(
        counter: &Arc<AtomicUsize>,
        events: &mpsc::Sender<()>,
        connection: Option<&Arc<OwnedSemaphorePermit>>,
    ) -> Arc<Self> {
        counter.fetch_add(1, Ordering::SeqCst);
        Arc::new(Self {
            counter: Arc::clone(counter),
            events: events.clone(),
            connection: connection.cloned(),
        })
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // Announce completion after releasing this owner's capacity reference.
        drop(self.connection.take());
        self.counter.fetch_sub(1, Ordering::SeqCst);
        let _ = self.events.try_send(());
    }
}

pub(super) async fn run<T>(
    worker: JoinHandle<Result<T>>,
    operation: Option<&SearchOperation>,
) -> Result<T> {
    match operation {
        Some(operation) => {
            let deadline = tokio::time::Instant::from_std(operation.deadline());
            with_expiry(worker, operation, tokio::time::sleep_until(deadline)).await
        }
        None => joined(worker.await),
    }
}

/// A private expiry signal permits gate-driven tests without virtual std time.
async fn with_expiry<T>(
    worker: JoinHandle<Result<T>>,
    operation: &SearchOperation,
    expiry: impl Future<Output = ()>,
) -> Result<T> {
    let mut cancellation = CancelOnDrop(Some(operation.cancellation().clone()));
    tokio::select! {
        biased;
        result = worker => {
            cancellation.0 = None;
            operation.check()?;
            joined(result)
        }
        () = operation.cancellation().cancelled() => {
            Err(Error::Admission { message: "search request cancelled".to_owned() })
        }
        () = expiry => {
            operation.cancellation().cancel();
            Err(Error::Admission {
                message: "search request deadline expired".to_owned(),
            })
        }
    }
}

fn joined<T>(result: std::result::Result<Result<T>, JoinError>) -> Result<T> {
    result.map_err(|err| Error::internal_with_source("tool task join", err))?
}

pub(crate) struct CancelOnDrop(pub(crate) Option<Cancellation>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc as std_mpsc};
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tokio::sync::{Semaphore, mpsc, oneshot};

    use crate::error::Error;
    use crate::hybrid::work::Cancellation;
    use crate::search::operation::SearchOperation;

    use super::*;

    const WATCHDOG: Duration = Duration::from_mins(5);

    fn operation() -> SearchOperation {
        SearchOperation::new(
            crate::search::settings::standalone()
                .expect("owner policy")
                .settings(),
            Instant::now(),
            Cancellation::new(),
        )
        .expect("operation")
    }

    #[tokio::test]
    async fn expiry_keeps_actual_worker_and_connection_charged() {
        let count = Arc::new(AtomicUsize::new(0));
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots).try_acquire_owned().expect("permit");
        let (events, mut observed) = mpsc::channel(1);
        let reply = Activity::new(&count, &events, Some(&Arc::new(permit)));
        let actual = Arc::clone(&reply);
        let (entered, started) = oneshot::channel();
        let (release, proceed) = std_mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _activity = actual;
            let _ = entered.send(());
            proceed
                .recv_timeout(WATCHDOG)
                .expect("actual worker release watchdog");
            Ok(json!({"finished":true}))
        });
        tokio::time::timeout(WATCHDOG, started)
            .await
            .expect("actual worker entry watchdog")
            .expect("actual work started");
        let operation = operation();
        let error = tokio::time::timeout(
            WATCHDOG,
            with_expiry(worker, &operation, std::future::ready(())),
        )
        .await
        .expect("injected expiry watchdog")
        .expect_err("injected expiry");
        assert!(matches!(error, Error::Admission { .. }));
        assert!(operation.cancellation().is_cancelled());
        drop(reply);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(Arc::clone(&slots).try_acquire_owned().is_err());
        assert!(observed.try_recv().is_err());
        let _ = release.send(());
        tokio::time::timeout(WATCHDOG, observed.recv())
            .await
            .expect("actual worker settlement watchdog")
            .expect("actual worker settled");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
        assert!(observed.try_recv().is_err());
    }

    #[tokio::test]
    async fn completed_work_keeps_reply_and_connection_charged() {
        for outcome in 0..3 {
            let count = Arc::new(AtomicUsize::new(0));
            let slots = Arc::new(Semaphore::new(1));
            let permit = Arc::clone(&slots).try_acquire_owned().expect("permit");
            let (events, mut observed) = mpsc::channel(1);
            let reply = Activity::new(&count, &events, Some(&Arc::new(permit)));
            let actual = Arc::clone(&reply);
            let worker = tokio::task::spawn_blocking(move || {
                let _activity = actual;
                match outcome {
                    0 => Ok(json!({"finished":true})),
                    1 => Err(Error::Query {
                        message: "injected query failure".to_owned(),
                    }),
                    _ => panic!("injected worker unwind"),
                }
            });
            let operation = operation();
            let result = tokio::time::timeout(WATCHDOG, run(worker, Some(&operation)))
                .await
                .expect("worker completion watchdog");
            assert!(match outcome {
                0 => result.is_ok(),
                1 => matches!(result, Err(Error::Query { .. })),
                _ => matches!(result, Err(Error::Internal { .. })),
            });
            assert!(!operation.cancellation().is_cancelled());
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(Arc::clone(&slots).try_acquire_owned().is_err());
            assert!(observed.try_recv().is_err());
            drop(reply);
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
            observed.try_recv().expect("one last-drop event");
            assert!(observed.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn completed_worker_cannot_reset_expired_original_budget() {
        let policy = crate::search::settings::standalone().expect("owner policy");
        let start = Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                policy.settings().request_timeout_ms,
            ))
            .expect("original start");
        let operation = SearchOperation::new(policy.settings(), start, Cancellation::new())
            .expect("expired original operation");
        let worker = tokio::task::spawn_blocking(|| Ok(json!({"finished":true})));
        let result = tokio::time::timeout(
            WATCHDOG,
            with_expiry(worker, &operation, std::future::pending()),
        )
        .await
        .expect("expired completion watchdog");
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert!(operation.cancellation().is_cancelled());
        assert_eq!(operation.started_at(), start);
    }
}
