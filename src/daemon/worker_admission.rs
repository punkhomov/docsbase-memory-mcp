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
    background: Lane,
    terminal: Arc<Semaphore>,
    #[cfg(feature = "hybrid-remote")]
    helper: Lane,
    control_timeout: Duration,
    shutdown: Cancellation,
}

impl WorkerAdmission {
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        let (search_active, search_pending) = self.search.occupancy();
        let (control_active, control_pending) = self.control.occupancy();
        let (background_active, background_pending) = self.background.occupancy();
        #[cfg(feature = "hybrid-remote")]
        let (helper_active, helper_pending) = self.helper.occupancy();
        #[cfg(not(feature = "hybrid-remote"))]
        let (helper_active, helper_pending) = (0, 0);
        serde_json::json!({"cpu_search_active": search_active,"cpu_search_pending":search_pending,"cpu_control_active":control_active,"cpu_control_pending":control_pending,"cpu_background_active":background_active,"cpu_background_pending":background_pending,"cpu_terminal_active":1usize.saturating_sub(self.terminal.available_permits()),"helper_active":helper_active,"helper_pending":helper_pending})
    }

    /// Compatibility composition resolving omitted remote policy through its owner.
    ///
    /// # Errors
    /// Invalid owner policy, including unsupported semaphore sizes.
    pub fn with_settings(settings: &DaemonSettings) -> Result<Self> {
        Self::with_policies(
            settings,
            crate::hybrid::remote_settings::standalone()?.settings(),
        )
    }

    /// Compose complete daemon and remote policy before creating any worker lane.
    ///
    /// # Errors
    /// Invalid owner policy or semaphore capacity.
    pub fn with_policies(
        settings: &DaemonSettings,
        remote: &crate::hybrid::remote_settings::RemoteSettings,
    ) -> Result<Self> {
        DaemonSpec::validate(settings, &Capabilities::current())?;
        crate::hybrid::remote_settings::RemoteSpec::validate(remote, &Capabilities::current())?;
        Ok(Self {
            search: Lane::new(settings.search_active, settings.search_pending),
            control: Lane::new(settings.control_active, settings.control_pending),
            background: Lane::new(settings.background_active, settings.background_pending),
            terminal: Arc::new(Semaphore::new(1)),
            #[cfg(feature = "hybrid-remote")]
            helper: Lane::new(
                settings.helper_active,
                remote
                    .interactive_active
                    .checked_add(remote.background_active)
                    .ok_or_else(|| denied("remote helper pending capacity overflow"))?,
            ),
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
        let queued_at = SearchOperation::clock_now(operation);
        let result = self
            .search
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.shutdown,
            )
            .await;
        operation.record_queue(operation.since(queued_at));
        operation.check()?;
        result
    }

    /// Admit a background CPU phase within its original whole-job budget.
    pub(crate) async fn background(
        &self,
        operation: &SearchOperation,
    ) -> Result<OwnedSemaphorePermit> {
        operation.check()?;
        let queued = SearchOperation::clock_now(operation);
        let result = self
            .background
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.shutdown,
            )
            .await;
        operation.record_queue(operation.since(queued));
        operation.check()?;
        result
    }

    pub(crate) fn reserve_background(
        &self,
        operation: &SearchOperation,
    ) -> Result<LaneReservation> {
        operation.check()?;
        self.shutdown.check()?;
        self.background.reserve()
    }

    pub(crate) async fn background_reserved(
        &self,
        operation: &SearchOperation,
        reservation: LaneReservation,
    ) -> Result<OwnedSemaphorePermit> {
        reservation
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.shutdown,
            )
            .await
    }

    // Only JobRoot's already registered terminal authority can call this lane.
    pub(crate) fn terminal(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.terminal)
            .try_acquire_owned()
            .map_err(|_| denied("terminal worker already occupied"))
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

    pub(crate) async fn cancelled(&self) {
        self.shutdown.cancelled().await;
    }

    /// Retires pending and future admission; running closures keep their permits.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    #[cfg(feature = "hybrid-remote")]
    pub(crate) async fn helper(
        &self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<OwnedSemaphorePermit> {
        self.helper
            .acquire(deadline, cancellation, &self.shutdown)
            .await
    }
}

pub(crate) struct Lane {
    active_capacity: usize,
    pending_capacity: usize,
    active: Arc<Semaphore>,
    pending: Arc<Semaphore>,
}

impl Lane {
    pub(crate) fn new(active: usize, pending: usize) -> Self {
        Self {
            active_capacity: active,
            pending_capacity: pending,
            active: Arc::new(Semaphore::new(active)),
            pending: Arc::new(Semaphore::new(pending)),
        }
    }

    fn occupancy(&self) -> (usize, usize) {
        (
            self.active_capacity
                .saturating_sub(self.active.available_permits()),
            self.pending_capacity
                .saturating_sub(self.pending.available_permits()),
        )
    }

    pub(crate) fn reserve(&self) -> Result<LaneReservation> {
        if let Ok(ready) = self.active.clone().try_acquire_owned() {
            return Ok(LaneReservation {
                active: self.active.clone(),
                ready: Some(ready),
                pending: None,
            });
        }
        let pending = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| denied("blocking worker pending limit reached"))?;
        Ok(LaneReservation {
            active: self.active.clone(),
            ready: None,
            pending: Some(pending),
        })
    }

    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn pending_available(&self) -> usize {
        self.pending.available_permits()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.active.available_permits() == self.active_capacity
    }

    pub(crate) async fn acquire(
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

pub(crate) struct LaneReservation {
    active: Arc<Semaphore>,
    ready: Option<OwnedSemaphorePermit>,
    pending: Option<OwnedSemaphorePermit>,
}
impl LaneReservation {
    pub(crate) async fn acquire(
        mut self,
        deadline: Instant,
        cancellation: &Cancellation,
        shutdown: &Cancellation,
    ) -> Result<OwnedSemaphorePermit> {
        check(deadline, cancellation, shutdown)?;
        if let Some(ready) = self.ready.take() {
            return Ok(ready);
        }
        let permit = tokio::select! { biased;
            () = shutdown.cancelled() => return Err(denied("daemon is shutting down")),
            () = cancellation.cancelled() => return Err(denied("shared work request cancelled")),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(denied("blocking worker admission deadline expired")),
            permit = self.active.clone().acquire_owned() => permit.map_err(|_| denied("blocking worker lane closed"))?,
        };
        drop(self.pending.take());
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
