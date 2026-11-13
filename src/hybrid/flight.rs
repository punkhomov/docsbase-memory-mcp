//! Ephemeral identical-work coalescing; no provider runs under registry locks.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::error::{Error, Result};
use crate::hybrid::work::{Cancellation, Wake};

type SharedResult<T> = std::result::Result<T, SharedFailure>;

pub(crate) type WorkResult<T> = std::result::Result<T, WorkError>;

#[derive(Debug)]
pub(crate) enum WorkError {
    Failed(Error),
    Abandoned,
}

impl From<Error> for WorkError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

/// Interest in shared work, never the initiating caller's personal budget.
pub(crate) struct Interest<'a> {
    active: &'a dyn Fn() -> bool,
}

impl Interest<'_> {
    pub(crate) fn check(&self) -> WorkResult<()> {
        if (self.active)() {
            Ok(())
        } else {
            Err(WorkError::Abandoned)
        }
    }
}

#[derive(Clone)]
enum Completion<T> {
    Finished(SharedResult<T>),
    // A caller can retain a selected flight before registering its wake. Such
    // live callers must retry admission instead of inheriting cancellation.
    Abandoned,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum Boundary {
    Selected,
    BeforeStart,
    Inactive,
}

#[cfg(test)]
type BoundaryObserver = Arc<dyn Fn(Boundary) + Send + Sync>;

pub(crate) struct Flights<K, T> {
    active: Mutex<HashMap<K, Arc<Flight<T>>>>,
    #[cfg(test)]
    observer: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    #[cfg(test)]
    boundary_observer: Mutex<Option<BoundaryObserver>>,
}

impl<K, T> Default for Flights<K, T> {
    fn default() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            #[cfg(test)]
            observer: Mutex::new(None),
            #[cfg(test)]
            boundary_observer: Mutex::new(None),
        }
    }
}

impl<K: Clone + Eq + Hash, T: Clone> Flights<K, T> {
    pub(crate) fn run(
        &self,
        key: &K,
        cancellation: &Cancellation,
        cached: impl FnMut() -> Option<T>,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.run_checked(key, cancellation, cached, |_| work().map_err(Into::into))
    }

    pub(crate) fn run_checked(
        &self,
        key: &K,
        cancellation: &Cancellation,
        mut cached: impl FnMut() -> Option<T>,
        work: impl FnOnce(&Interest<'_>) -> WorkResult<T>,
    ) -> Result<T> {
        cancellation.check()?;
        let listener = cancellation.listen();
        loop {
            cancellation.check()?;
            let (flight, leader) = {
                let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(flight) = active.get(key) {
                    (Arc::clone(flight), false)
                } else {
                    // Recheck on every admission attempt: another leader may
                    // have stored a completed value before this retry.
                    if let Some(value) = cached() {
                        cancellation.check()?;
                        return Ok(value);
                    }
                    let flight = Arc::new(Flight {
                        state: Mutex::new(FlightState {
                            result: None,
                            listeners: Vec::new(),
                        }),
                    });
                    active.insert(key.clone(), Arc::clone(&flight));
                    (flight, true)
                }
            };
            #[cfg(test)]
            self.observe_boundary(Boundary::Selected);
            let _membership = flight.join(&listener.wake);
            let leader = if leader {
                Some(Leader {
                    registry: self,
                    key: key.clone(),
                    flight: Arc::clone(&flight),
                    complete: false,
                })
            } else {
                None
            };
            #[cfg(test)]
            if let Some(observer) = self
                .observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                let _ = observer.send(());
            }
            if let Some(leader) = leader {
                #[cfg(test)]
                self.observe_boundary(Boundary::BeforeStart);
                if flight.has_active() {
                    let active = || flight.has_active();
                    let interest = Interest { active: &active };
                    let result = match work(&interest) {
                        Ok(value) => Ok(value),
                        Err(WorkError::Failed(error)) => Err(SharedFailure::new(error)),
                        Err(WorkError::Abandoned) => {
                            #[cfg(test)]
                            self.observe_boundary(Boundary::Inactive);
                            // A live caller can register after the decision.
                            // Retire with retry semantics, not a shared error.
                            leader.finish(Completion::Abandoned);
                            cancellation.check()?;
                            return Err(Error::Admission {
                                message: "shared work has no interested callers".to_owned(),
                            });
                        }
                    };
                    leader.finish(Completion::Finished(result.clone()));
                    cancellation.check()?;
                    return result.map_err(SharedFailure::into_error);
                }
                #[cfg(test)]
                self.observe_boundary(Boundary::Inactive);
                leader.finish(Completion::Abandoned);
                continue;
            }
            loop {
                cancellation.check()?;
                match flight.result() {
                    Some(Completion::Finished(result)) => {
                        return result.map_err(SharedFailure::into_error);
                    }
                    Some(Completion::Abandoned) => break,
                    None => listener.wait()?,
                }
            }
        }
    }

    #[cfg(test)]
    fn observe_boundary(&self, boundary: Boundary) {
        let observer = self
            .boundary_observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(observer) = observer {
            observer(boundary);
        }
    }

    fn complete(&self, key: &K, flight: &Arc<Flight<T>>, result: Completion<T>) {
        let listeners = {
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            let mut state = flight.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.result.is_none() {
                state.result = Some(result);
            }
            let listeners = std::mem::take(&mut state.listeners);
            if active
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, flight))
            {
                active.remove(key);
            }
            listeners
        };
        for wake in listeners.into_iter().filter_map(|wake| wake.upgrade()) {
            wake.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn registry_available(&self) -> bool {
        self.active.try_lock().is_ok()
    }

    #[cfg(test)]
    pub(crate) fn observe_joins(&self, observer: std::sync::mpsc::Sender<()>) {
        *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = Some(observer);
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

struct Flight<T> {
    state: Mutex<FlightState<T>>,
}
struct FlightState<T> {
    result: Option<Completion<T>>,
    listeners: Vec<Weak<Wake>>,
}

impl<T: Clone> Flight<T> {
    fn join(self: &Arc<Self>, wake: &Arc<Wake>) -> Membership<T> {
        let weak = Arc::downgrade(wake);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .push(weak.clone());
        Membership {
            flight: Arc::clone(self),
            wake: weak,
        }
    }

    fn has_active(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .iter()
            .filter_map(Weak::upgrade)
            .any(|wake| wake.active())
    }

    fn result(&self) -> Option<Completion<T>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .result
            .clone()
    }
}

struct Membership<T> {
    flight: Arc<Flight<T>>,
    wake: Weak<Wake>,
}
impl<T> Drop for Membership<T> {
    fn drop(&mut self) {
        self.flight
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .retain(|wake| !Weak::ptr_eq(wake, &self.wake));
    }
}

struct Leader<'a, K: Clone + Eq + Hash, T: Clone> {
    registry: &'a Flights<K, T>,
    key: K,
    flight: Arc<Flight<T>>,
    complete: bool,
}

impl<K: Clone + Eq + Hash, T: Clone> Leader<'_, K, T> {
    fn finish(mut self, result: Completion<T>) {
        self.registry.complete(&self.key, &self.flight, result);
        self.complete = true;
    }
}

impl<K: Clone + Eq + Hash, T: Clone> Drop for Leader<'_, K, T> {
    fn drop(&mut self) {
        if !self.complete {
            self.registry.complete(
                &self.key,
                &self.flight,
                Completion::Finished(Err(SharedFailure::new(Error::internal(
                    "shared work leader unwound before completion",
                )))),
            );
        }
    }
}

#[derive(Clone)]
struct SharedFailure(Arc<Error>);
impl SharedFailure {
    fn new(error: Error) -> Self {
        Self(Arc::new(error))
    }

    fn into_error(self) -> Error {
        match self.0.as_ref() {
            Error::Admission { message } => Error::Admission {
                message: message.clone(),
            },
            Error::Protocol { message } => Error::Protocol {
                message: message.clone(),
            },
            Error::Project {
                message,
                instruction,
            } => Error::Project {
                message: message.clone(),
                instruction: instruction.clone(),
            },
            Error::Index { path, message } => Error::Index {
                path: path.clone(),
                message: message.clone(),
            },
            Error::Query { message } => Error::Query {
                message: message.clone(),
            },
            Error::Transport { message, .. } => {
                Error::transport_with_source(message.clone(), SharedSource(Arc::clone(&self.0)))
            }
            Error::Internal { message, .. } => {
                Error::internal_with_source(message.clone(), SharedSource(Arc::clone(&self.0)))
            }
        }
    }
}

#[derive(Debug)]
struct SharedSource(Arc<Error>);
impl fmt::Display for SharedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for SharedSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    fn receive<T>(receiver: &Receiver<T>) -> T {
        receiver
            .recv_timeout(Duration::from_mins(5))
            .expect("flight boundary watchdog")
    }

    fn gate() -> (impl Fn() + Send + Sync, Receiver<()>, Sender<()>) {
        let (entered, entry) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let proceed = Mutex::new(proceed);
        let pause = move || {
            entered.send(()).expect("boundary reached");
            receive(&proceed.lock().expect("gate"));
        };
        (pause, entry, release)
    }

    #[derive(Clone, Copy)]
    enum LiveAdmission {
        AfterInactiveDecision,
        SelectedBeforeDecision,
        RegistersAfterRetirement,
        UsesCompletedCacheAfterRetirement,
    }

    fn cancellation_observer(
        admission: LiveAdmission,
        during_work: bool,
        before_start: Arc<impl Fn() + Send + Sync + 'static>,
        inactive: impl Fn() + Send + Sync + 'static,
        selected: impl Fn() + Send + Sync + 'static,
    ) -> BoundaryObserver {
        let selections = AtomicUsize::new(0);
        let starts = AtomicUsize::new(0);
        Arc::new(move |boundary| match boundary {
            Boundary::Selected => {
                if selections.fetch_add(1, Ordering::SeqCst) == 1
                    && !matches!(admission, LiveAdmission::AfterInactiveDecision)
                {
                    selected();
                }
            }
            Boundary::BeforeStart => {
                if starts.fetch_add(1, Ordering::SeqCst) == 0 && !during_work {
                    before_start();
                }
            }
            Boundary::Inactive => inactive(),
        })
    }

    fn pre_provider_cancellation_race(admission: LiveAdmission, during_work: bool) {
        let flights = Flights::<u8, u8>::default();
        let (cancelled, live) = (Cancellation::new(), Cancellation::new());
        let calls = AtomicUsize::new(0);
        let cached = Mutex::new(None);
        let cache_checks = AtomicUsize::new(0);
        let (joined, joins) = mpsc::channel();
        flights.observe_joins(joined);
        let (cancelled_result, live_result) = std::thread::scope(|scope| {
            // Controllers drop before scoped joins on assertion unwind, so no
            // worker can remain stuck on a fixture gate.
            let (before_start, start_entry, start_release) = gate();
            let before_start = Arc::new(before_start);
            let (inactive, inactive_entry, inactive_release) = gate();
            let (selected, selected_entry, selected_release) = gate();
            *flights.boundary_observer.lock().expect("observer") = Some(cancellation_observer(
                admission,
                during_work,
                Arc::clone(&before_start),
                inactive,
                selected,
            ));
            let (flight_registry, leader_token) = (&flights, &cancelled);
            let leader = scope.spawn(move || {
                flight_registry.run_checked(
                    &1,
                    leader_token,
                    || None,
                    |interest| {
                        if during_work {
                            before_start();
                            interest.check()?;
                        }
                        panic!("cancelled provider")
                    },
                )
            });
            receive(&start_entry);
            receive(&joins);
            let run_live = || {
                flights.run(
                    &1,
                    &live,
                    || {
                        cache_checks.fetch_add(1, Ordering::SeqCst);
                        *cached.lock().expect("completed cache")
                    },
                    || {
                        assert!(flights.active.try_lock().is_ok());
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(42)
                    },
                )
            };
            let follower = if matches!(admission, LiveAdmission::AfterInactiveDecision) {
                cancelled.cancel();
                start_release.send(()).expect("check cancelled leader");
                receive(&inactive_entry);
                scope.spawn(run_live)
            } else {
                let follower = scope.spawn(run_live);
                receive(&selected_entry);
                cancelled.cancel();
                start_release.send(()).expect("check cancelled leader");
                receive(&inactive_entry);
                follower
            };
            let cancelled_result = if matches!(
                admission,
                LiveAdmission::RegistersAfterRetirement
                    | LiveAdmission::UsesCompletedCacheAfterRetirement
            ) {
                inactive_release.send(()).expect("retire old flight");
                let result = leader.join().expect("cancelled leader");
                if matches!(admission, LiveAdmission::UsesCompletedCacheAfterRetirement) {
                    *cached.lock().expect("completed cache") = Some(42);
                }
                selected_release.send(()).expect("register on retained Arc");
                result
            } else {
                if matches!(admission, LiveAdmission::SelectedBeforeDecision) {
                    selected_release.send(()).expect("register live caller");
                }
                receive(&joins);
                inactive_release.send(()).expect("retire old flight");
                leader.join().expect("cancelled leader")
            };
            (cancelled_result, follower.join().expect("live caller"))
        });
        assert!(matches!(cancelled_result, Err(Error::Admission { .. })));
        assert_eq!(
            live_result.expect("live caller must retry abandoned work"),
            42
        );
        assert_eq!(cache_checks.load(Ordering::SeqCst), 1);
        let expected_calls = usize::from(!matches!(
            admission,
            LiveAdmission::UsesCompletedCacheAfterRetirement
        ));
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        assert_eq!(flights.active(), 0);
        assert_eq!(cancelled.subscriber_count(), 0);
        assert_eq!(live.subscriber_count(), 0);
    }

    #[test]
    fn live_admission_after_inactive_decision_retries_abandoned_work() {
        pre_provider_cancellation_race(LiveAdmission::AfterInactiveDecision, false);
    }

    #[test]
    fn live_selection_before_inactive_decision_retries_abandoned_work() {
        pre_provider_cancellation_race(LiveAdmission::SelectedBeforeDecision, false);
    }

    #[test]
    fn live_registration_after_retirement_retries_abandoned_work() {
        pre_provider_cancellation_race(LiveAdmission::RegistersAfterRetirement, false);
    }

    #[test]
    fn live_retry_rechecks_completed_cache_before_provider() {
        pre_provider_cancellation_race(LiveAdmission::UsesCompletedCacheAfterRetirement, false);
    }

    #[test]
    fn live_join_after_phase_abandonment_retries_work() {
        pre_provider_cancellation_race(LiveAdmission::AfterInactiveDecision, true);
    }

    #[test]
    fn live_registration_after_phase_retirement_retries_work() {
        pre_provider_cancellation_race(LiveAdmission::RegistersAfterRetirement, true);
    }

    #[test]
    fn provider_admission_failure_is_not_retried() {
        let flights = Flights::<u8, u8>::default();
        let cancellation = Cancellation::new();
        let calls = AtomicUsize::new(0);
        let result = flights.run(
            &1,
            &cancellation,
            || None,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(Error::Admission {
                    message: "provider admission rejected".to_owned(),
                })
            },
        );
        assert_eq!(
            result.expect_err("provider rejected").inner_message(),
            "provider admission rejected"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(flights.active(), 0);
        assert_eq!(cancellation.subscriber_count(), 0);
    }

    #[test]
    fn all_participants_cancelled_before_provider_retires_flight() {
        let flights = Flights::<u8, u8>::default();
        let cancelled = Cancellation::new();
        let control = cancelled.clone();
        *flights.boundary_observer.lock().expect("observer") = Some(Arc::new(move |boundary| {
            if matches!(boundary, Boundary::BeforeStart) {
                control.cancel();
            }
        }));
        let result = flights.run(&1, &cancelled, || None, || panic!("cancelled provider"));
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert_eq!(flights.active(), 0);
        assert_eq!(cancelled.subscriber_count(), 0);
    }

    #[test]
    fn delayed_miss_uses_completed_value_without_a_new_provider_call() {
        let flights = Flights::<u8, Arc<[f32]>>::default();
        let cancellation = Cancellation::new();
        // The caller observed a cache miss before another flight completed.
        let stored: Arc<[f32]> = Arc::from([1.0, 2.0, 3.0]);
        let result = flights
            .run(
                &1,
                &cancellation,
                || Some(Arc::clone(&stored)),
                || panic!("a delayed miss must not start another paid call"),
            )
            .expect("stored value");
        assert!(Arc::ptr_eq(&stored, &result));
        assert_eq!(flights.active(), 0);
        assert_eq!(cancellation.subscriber_count(), 0);
    }
}
