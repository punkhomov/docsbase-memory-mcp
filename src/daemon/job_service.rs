//! Registered logical jobs with unique native envelopes and reserved terminal work.
//! The outer root outlives every cancellable driver and all issued tickets.
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::request::{self, Activity};
use super::settings::DaemonSettings;
use super::worker_admission::{Lane, WorkerAdmission};
use crate::error::{Error, Result};
use crate::hybrid::work::{Cancellation, RemoteCall};
use crate::index::async_job::IndexState;
use crate::search::budget::PayloadBudget;
use crate::search::operation::SearchOperation;

#[cfg(test)]
pub(crate) type RetirementObserver = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct JobService {
    inner: Arc<Service>,
}
struct Service {
    logical: Lane,
    registrations: mpsc::Sender<Registration>,
    close: Cancellation,
    watcher_owner:
        std::sync::OnceLock<(Arc<crate::watch::Watchers>, super::request::ActivitySource)>,
    #[cfg(test)]
    retirement_observer: std::sync::Mutex<Option<RetirementObserver>>,
}
pub(crate) struct JobReservation {
    owner: Arc<Service>,
    lane: super::worker_admission::LaneReservation,
    queued_at: std::time::Instant,
    operation: SearchOperation,
}
impl JobReservation {
    pub(crate) fn operation(&self) -> SearchOperation {
        self.operation.clone()
    }
}
struct Registration {
    logical: OwnedSemaphorePermit,
    activity: Arc<Activity>, // ALWAYS tail-free
    workers: Arc<WorkerAdmission>,
    operation: SearchOperation,
    terminal: oneshot::Receiver<Box<IndexState>>,
    physical_done: oneshot::Receiver<()>,
    ack: Option<oneshot::Sender<()>>,
    envelope: Option<Box<IndexState>>,
    physical_retired: bool,
}
struct PhysicalTail(Option<oneshot::Sender<()>>);
impl Drop for PhysicalTail {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            let _ = done.send(());
        }
    }
}

/// Sole native owner. Drop transfers preallocated storage; never destroys native state.
pub(crate) struct JobOwner {
    state: Option<Box<IndexState>>,
    terminal: Option<oneshot::Sender<Box<IndexState>>>,
    physical: Option<Arc<PhysicalTail>>,
}
/// Data drops before the opaque physical fence, including abandoned delivery.
pub(crate) struct JobValue<T> {
    value: T,
    _physical: Option<Arc<dyn Send + Sync>>,
}
impl<T> std::ops::Deref for JobValue<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
struct PhaseOutput<T> {
    value: Result<Arc<JobValue<T>>>,
    owner: JobOwner,
}

impl JobOwner {
    pub(crate) fn retain<T>(&self, value: T) -> Arc<JobValue<T>> {
        Arc::new(JobValue {
            value,
            _physical: self.tail(),
        })
    }
    pub(crate) fn fail(&mut self, message: String) {
        self.state().fail(message);
    }
    pub(crate) fn state(&mut self) -> &mut IndexState {
        // State is only taken by Drop. A live owner is constructed with a box.
        match self.state.as_deref_mut() {
            Some(state) => state,
            None => std::process::abort(),
        }
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn call(&self, operation: SearchOperation) -> RemoteCall {
        match &self.physical {
            Some(tail) => RemoteCall::new(operation).for_background(tail.clone()),
            None => std::process::abort(),
        }
    }
    fn tail(&self) -> Option<Arc<dyn Send + Sync>> {
        self.physical
            .as_ref()
            .map(|tail| tail.clone() as Arc<dyn Send + Sync>)
    }
}
impl Drop for JobOwner {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            match self.terminal.take() {
                Some(ticket) => match ticket.send(state) {
                    Ok(()) => {}
                    Err(_native) => std::process::abort(), // never drop native payload here
                },
                None => std::process::abort(),
            }
        }
        drop(self.physical.take());
    }
}

impl JobService {
    pub(crate) fn retain_watchers(
        &self,
        watchers: Arc<crate::watch::Watchers>,
        source: super::request::ActivitySource,
    ) -> Result<()> {
        self.inner.close.check()?;
        if let Some((current, activity)) = self.inner.watcher_owner.get() {
            if Arc::ptr_eq(current, &watchers) && Arc::ptr_eq(&activity.counter, &source.counter) {
                return Ok(());
            }
            return Err(Error::internal("root watcher owner already installed"));
        }
        self.inner
            .watcher_owner
            .set((watchers, source))
            .map_err(|_| Error::internal("root watcher owner already installed"))
    }
    #[cfg(test)]
    pub(crate) fn observe_retirement(&self, observer: RetirementObserver) {
        *self.inner.retirement_observer.lock().unwrap() = Some(observer);
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn pending_available(&self) -> usize {
        self.inner.logical.pending_available()
    }

    pub(crate) fn reserve(&self, operation: SearchOperation) -> Result<JobReservation> {
        operation.check()?;
        self.inner.close.check()?;
        Ok(JobReservation {
            owner: self.inner.clone(),
            lane: self.inner.logical.reserve()?,
            queued_at: std::time::Instant::now(),
            operation,
        })
    }
    pub(crate) async fn admit_reserved(
        &self,
        workers: Arc<WorkerAdmission>,
        activity: Arc<Activity>,
        budget: &PayloadBudget,
        reservation: JobReservation,
    ) -> Result<JobOwner> {
        if !Arc::ptr_eq(&self.inner, &reservation.owner) {
            return Err(Error::internal("foreign job reservation"));
        }
        let operation = reservation.operation;
        let logical = reservation
            .lane
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.inner.close,
            )
            .await?;
        operation.record_queue(reservation.queued_at.elapsed());
        self.register(logical, workers, activity, &operation, budget, None)
            .await
    }
    #[cfg(test)]
    pub(crate) async fn admit(
        &self,
        workers: Arc<WorkerAdmission>,
        activity: Arc<Activity>,
        operation: &SearchOperation,
        budget: &PayloadBudget,
    ) -> Result<JobOwner> {
        self.admit_with_account(workers, activity, operation, budget, None)
            .await
    }

    pub(crate) async fn admit_with_account(
        &self,
        workers: Arc<WorkerAdmission>,
        activity: Arc<Activity>,
        operation: &SearchOperation,
        budget: &PayloadBudget,
        account: Option<Arc<crate::search::budget::PayloadPermit>>,
    ) -> Result<JobOwner> {
        let queued = std::time::Instant::now();
        let logical = self
            .inner
            .logical
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.inner.close,
            )
            .await?;
        operation.record_queue(queued.elapsed());
        self.register(logical, workers, activity, operation, budget, account)
            .await
    }

    async fn register(
        &self,
        logical: OwnedSemaphorePermit,
        workers: Arc<WorkerAdmission>,
        activity: Arc<Activity>,
        operation: &SearchOperation,
        budget: &PayloadBudget,
        account: Option<Arc<crate::search::budget::PayloadPermit>>,
    ) -> Result<JobOwner> {
        operation.check()?;
        self.inner.close.check()?;
        let registration_bytes = u64::try_from(std::mem::size_of::<IndexState>() + 4096)
            .map_err(|_| Error::internal("job registration size overflow"))?;
        let account = match account {
            Some(account) => {
                account.grow_shared(
                    account
                        .bytes()
                        .checked_add(registration_bytes)
                        .ok_or_else(|| Error::Admission {
                            message: "job registration capture overflow".into(),
                        })?,
                )?;
                account
            }
            None => Arc::new(budget.reserve(registration_bytes)?),
        };
        let state = Box::new(IndexState::empty(account));
        #[cfg(test)]
        let state = {
            let mut state = state;
            state.set_retirement_observer(
                self.inner
                    .retirement_observer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            );
            state
        };
        let (terminal, receiver) = oneshot::channel();
        let (done, physical_done) = oneshot::channel();
        let physical = Arc::new(PhysicalTail(Some(done)));
        let (ack, acknowledged) = oneshot::channel();
        // No await from logical acquisition through registration; receiver owns L.
        let registration = Registration {
            logical,
            activity,
            workers,
            operation: operation.clone(),
            terminal: receiver,
            physical_done,
            ack: Some(ack),
            envelope: None,
            physical_retired: false,
        };
        self.inner
            .registrations
            .try_send(registration)
            .map_err(|_| Error::Admission {
                message: "background registration closed/full".into(),
            })?;
        let owner = JobOwner {
            state: Some(state),
            terminal: Some(terminal),
            physical: Some(physical),
        };
        let call = RemoteCall::new(operation.clone());
        call.wait(&self.inner.close, async {
            acknowledged
                .await
                .map_err(|e| Error::internal_with_source("background registration ack", e))
        })
        .await?;
        operation.check()?;
        Ok(owner)
    }

    /// A single native phase shares background admission with detached jobs.
    /// Its permit and activity travel with the worker through cancellation.
    pub(crate) async fn run_native<T: Send + 'static>(
        &self,
        workers: &WorkerAdmission,
        activity: Arc<Activity>,
        operation: &SearchOperation,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let queued = std::time::Instant::now();
        let logical = self
            .inner
            .logical
            .acquire(
                operation.deadline(),
                operation.cancellation(),
                &self.inner.close,
            )
            .await?;
        operation.record_queue(queued.elapsed());
        let permit = workers.background(operation).await?;
        let call = operation.clone();
        let close = self.inner.close.clone();
        let worker = request::spawn(Some(permit), Some(activity), move || {
            let _logical = logical;
            close.check()?;
            call.check()?;
            work()
        });
        request::run(worker, Some(operation)).await
    }

    pub(crate) async fn phase<T: Send + Sync + 'static>(
        &self,
        workers: &WorkerAdmission,
        activity: Arc<Activity>,
        operation: &SearchOperation,
        mut owner: JobOwner,
        work: impl FnOnce(&mut IndexState) -> Result<T> + Send + 'static,
    ) -> Result<(JobOwner, Arc<JobValue<T>>)> {
        let permit = workers.background(operation).await?;
        operation.check()?;
        let tail = owner.tail();
        let call = operation.clone();
        let worker =
            request::spawn_with_tail(Some(permit), Some(activity), None, tail, move || {
                call.check()?;
                let result = work(owner.state());
                if let Err(error) = &result {
                    owner.fail(error.to_string());
                }
                Ok(PhaseOutput {
                    value: result.map(|value| owner.retain(value)),
                    owner,
                })
            });
        let PhaseOutput { value, owner } = request::run(worker, Some(operation)).await?;
        Ok((owner, value?))
    }
}

pub(crate) struct JobRoot {
    service: JobService,
    registrations: mpsc::Receiver<Registration>,
    slots: Vec<Option<Registration>>,
    terminal: Option<JoinHandle<Result<()>>>,
    poll_interval: std::time::Duration,
}
impl JobRoot {
    pub(crate) fn new(settings: &DaemonSettings) -> Self {
        let (send, registrations) = mpsc::channel(settings.background_active);
        Self {
            service: JobService {
                inner: Arc::new(Service {
                    logical: Lane::new(settings.background_active, settings.background_pending),
                    registrations: send,
                    close: Cancellation::new(),
                    watcher_owner: std::sync::OnceLock::new(),
                    #[cfg(test)]
                    retirement_observer: std::sync::Mutex::new(None),
                }),
            },
            registrations,
            slots: (0..settings.background_active).map(|_| None).collect(),
            terminal: None,
            poll_interval: std::time::Duration::from_millis(settings.drain_poll_ms),
        }
    }
    pub(crate) fn service(&self) -> JobService {
        self.service.clone()
    }
    fn close(&mut self) {
        if let Some((watchers, _)) = self.service.inner.watcher_owner.get() {
            watchers.close();
        }
        self.service.inner.close.cancel();
        for slot in self.slots.iter().flatten() {
            slot.operation.cancellation().cancel();
        }
    }
    pub(crate) async fn drive(&mut self) -> Result<()> {
        let interval = self.poll_interval;
        loop {
            tokio::select! { result = poll_fn(|cx| self.poll(cx)) => return result, () = tokio::time::sleep(interval) => {} }
        }
    }
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        while let Poll::Ready(Some(mut registration)) = self.registrations.poll_recv(cx) {
            if self.service.inner.close.is_cancelled() {
                registration.operation.cancellation().cancel();
            }
            let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) else {
                std::process::abort()
            };
            let ack = registration.ack.take();
            *slot = Some(registration);
            if let Some(ack) = ack {
                let _ = ack.send(());
            }
        }
        if let Some(join) = &mut self.terminal
            && let Poll::Ready(result) = Pin::new(join).poll(cx)
        {
            self.terminal = None;
            #[cfg(test)]
            if let Some(observer) = self
                .service
                .inner
                .retirement_observer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                observer("terminal_finished");
            }
            if let Err(error) = result {
                eprintln!("warning: terminal job worker failed: {error}");
            } else if let Ok(Err(error)) = result {
                eprintln!("warning: terminal job publication failed: {error}");
            }
        }
        for slot in &mut self.slots {
            let Some(registration) = slot.as_mut() else {
                continue;
            };
            if registration.envelope.is_none() {
                match Pin::new(&mut registration.terminal).poll(cx) {
                    Poll::Ready(Ok(envelope)) => registration.envelope = Some(envelope),
                    Poll::Ready(Err(_)) => {
                        std::process::abort();
                    }
                    Poll::Pending => {}
                }
            }
            if !registration.physical_retired
                && Pin::new(&mut registration.physical_done)
                    .poll(cx)
                    .is_ready()
            {
                registration.physical_retired = true;
            }
            if self.terminal.is_none()
                && registration.envelope.is_some()
                && registration.physical_retired
            {
                let Ok(permit) = registration.workers.terminal() else {
                    std::process::abort()
                };
                let Some(mut registration) = slot.take() else {
                    std::process::abort()
                };
                let Some(state) = registration.envelope.take() else {
                    std::process::abort()
                };
                let activity = registration.activity.clone();
                self.terminal = Some(request::spawn(
                    Some(permit),
                    Some(registration.activity),
                    move || {
                        let _logical = registration.logical;
                        state.retire(activity);
                        Ok(())
                    },
                ));
                cx.waker().wake_by_ref();
            }
        }
        if self.service.inner.close.is_cancelled()
            && self.registrations.is_empty()
            && self.slots.iter().all(Option::is_none)
            && self.terminal.is_none()
            && self.service.inner.logical.is_idle()
        {
            return Poll::Ready(Ok(()));
        }
        Poll::Pending
    }

    /// Outer synchronous boundary: receivers survive every dropped/panicking work driver.
    pub(crate) fn run<T>(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        work: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
            tokio::pin!(work);
            tokio::select! { result = &mut work => result, result = self.drive() => { result?; Err(Error::internal("job root ended before work")) } }
        })
        }));
        self.close();
        let drained = runtime.block_on(self.drive());
        if let Some((watchers, source)) = self.service.inner.watcher_owner.get() {
            runtime.block_on(source.drain(self.poll_interval));
            // Existing debounce threads own/drop native watchers. Join their
            // already-retired handles on this outer synchronous root thread.
            watchers.join_retired();
        }
        match result {
            Ok(result) => {
                drained?;
                result
            }
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}
impl Drop for JobRoot {
    fn drop(&mut self) {
        if self.terminal.is_some()
            || self.slots.iter().any(Option::is_some)
            || !self.registrations.is_empty()
            || !self.service.inner.logical.is_idle()
        {
            std::process::abort();
        }
    }
}

#[cfg(test)]
#[path = "job_service_tests.rs"]
mod tests;
