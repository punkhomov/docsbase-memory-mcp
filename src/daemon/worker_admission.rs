//! Bounded application-owned blocking work, independent of connection admission.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::search::operation::SearchOperation;

use super::settings::{DaemonSettings, DaemonSpec};

/// Fair search/control lanes resolved entirely by the daemon policy owner.
pub struct WorkerAdmission {
    search: Lane,
    control: Lane,
    control_timeout: Duration,
    shutdown: Cancellation,
}

impl WorkerAdmission {
    /// Constructs only from complete validated settings; no consumer defaults.
    ///
    /// # Errors
    /// Invalid owner policy, including unsupported semaphore sizes.
    pub fn with_settings(settings: &DaemonSettings) -> Result<Self> {
        DaemonSpec::validate(settings, &Capabilities::current())?;
        Ok(Self {
            search: Lane::new(settings.search_active, settings.search_pending),
            control: Lane::new(settings.control_active, settings.control_pending),
            control_timeout: Duration::from_millis(settings.control_timeout_ms),
            shutdown: Cancellation::new(),
        })
    }

    /// Waits within this operation's original deadline and cancellation state.
    /// The returned permit must move into the actual blocking closure.
    ///
    /// # Errors
    /// Queue full, original deadline expired, cancellation or daemon shutdown.
    pub async fn search(&self, operation: &SearchOperation) -> Result<OwnedSemaphorePermit> {
        operation.check()?;
        let queued_at = Instant::now();
        let result = self
            .search
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.shutdown,
            )
            .await;
        operation.record_queue(queued_at.elapsed());
        operation.check()?;
        result
    }

    /// Uses the owner's finite control timeout and daemon shutdown state.
    ///
    /// # Errors
    /// Queue full, timeout or daemon shutdown.
    pub async fn control(&self) -> Result<OwnedSemaphorePermit> {
        let deadline = Instant::now()
            .checked_add(self.control_timeout)
            .ok_or_else(|| denied("control deadline exceeds this platform's clock range"))?;
        self.control
            .acquire(deadline, &self.shutdown, &self.shutdown)
            .await
    }

    /// Retires pending and future admission; running closures keep their permits.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

struct Lane {
    active: Arc<Semaphore>,
    pending: Arc<Semaphore>,
}

impl Lane {
    fn new(active: usize, pending: usize) -> Self {
        Self {
            active: Arc::new(Semaphore::new(active)),
            pending: Arc::new(Semaphore::new(pending)),
        }
    }

    async fn acquire(
        &self,
        deadline: Instant,
        cancellation: &Cancellation,
        shutdown: &Cancellation,
    ) -> Result<OwnedSemaphorePermit> {
        check(deadline, cancellation, shutdown)?;
        // Tokio's try-acquire respects already queued active waiters. Taking
        // free active capacity first makes pending=0 a useful immediate lane.
        if let Ok(permit) = Arc::clone(&self.active).try_acquire_owned() {
            check(deadline, cancellation, shutdown)?;
            return Ok(permit);
        }
        let pending = Arc::clone(&self.pending)
            .try_acquire_owned()
            .map_err(|_| denied("blocking worker pending limit reached"))?;
        let permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Err(denied("daemon is shutting down")),
            () = cancellation.cancelled() => return Err(denied("shared work request cancelled")),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                return Err(denied("blocking worker admission deadline expired"));
            }
            permit = Arc::clone(&self.active).acquire_owned() => {
                permit.map_err(|_| denied("blocking worker lane closed"))?
            }
        };
        drop(pending);
        check(deadline, cancellation, shutdown)?;
        Ok(permit)
    }
}

fn check(deadline: Instant, cancellation: &Cancellation, shutdown: &Cancellation) -> Result<()> {
    if shutdown.is_cancelled() {
        return Err(denied("daemon is shutting down"));
    }
    if Instant::now() >= deadline {
        return Err(denied("blocking worker admission deadline expired"));
    }
    cancellation.check()
}

fn denied(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
