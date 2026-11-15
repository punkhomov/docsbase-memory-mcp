//! One caller's original search deadline and cooperative cancellation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;

use super::settings::{SearchSettings, SearchSpec};

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;

/// A single request budget retained across queues, phases and transport retries.
/// A running synchronous provider requires an external cancellation driver;
/// timestamp checks alone do not interrupt it or a blocked flight waiter.
#[derive(Clone)]
pub struct SearchOperation {
    started_at: Instant,
    deadline: Instant,
    cancellation: Cancellation,
    queue_ms: Arc<AtomicU64>,
    execution: Arc<Execution>,
    #[cfg(all(test, feature = "hybrid-remote"))]
    clock: Option<TestClock>,
}

#[cfg(all(test, feature = "hybrid-remote"))]
pub(crate) type TestClock = (
    Arc<dyn Fn() -> Instant + Send + Sync>,
    Arc<tokio::sync::Notify>,
);

#[derive(Default)]
struct Execution {
    cpu_ms: AtomicU64,
    network_ms: AtomicU64,
    backoff_ms: AtomicU64,
    provider_attempts: AtomicU64,
    shared_work: AtomicBool,
    #[cfg(feature = "hybrid-remote")]
    remote_cancellation_recorded: AtomicBool,
}

/// Per-operation timings overlap for shared work; they are not a wall-clock partition.
#[derive(serde::Serialize)]
pub(crate) struct ExecutionSnapshot {
    execution_version: u8,
    cpu_ms: u64,
    network_ms: u64,
    backoff_ms: u64,
    provider_attempts: u64,
    shared_work: bool,
}

pub(crate) struct ExecutionTimer {
    operation: SearchOperation,
    kind: TimeKind,
    started: Instant,
}
enum TimeKind {
    Cpu,
    #[cfg(feature = "hybrid-remote")]
    Network,
    #[cfg(feature = "hybrid-remote")]
    Backoff,
    #[cfg(feature = "hybrid-remote")]
    SharedNetwork,
}
impl Drop for ExecutionTimer {
    fn drop(&mut self) {
        let execution = &self.operation.execution;
        let counter = match self.kind {
            TimeKind::Cpu => &execution.cpu_ms,
            #[cfg(feature = "hybrid-remote")]
            TimeKind::Network => &execution.network_ms,
            #[cfg(feature = "hybrid-remote")]
            TimeKind::Backoff => &execution.backoff_ms,
            #[cfg(feature = "hybrid-remote")]
            TimeKind::SharedNetwork if execution.shared_work.load(Ordering::Relaxed) => {
                &execution.network_ms
            }
            #[cfg(feature = "hybrid-remote")]
            TimeKind::SharedNetwork => return,
        };
        add(
            counter,
            u64::try_from(self.operation.since(self.started).as_millis()).unwrap_or(u64::MAX),
        );
    }
}
fn add(counter: &AtomicU64, value: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

impl SearchOperation {
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn with_test_clock(mut self, clock: TestClock) -> Self {
        self.clock = Some(clock);
        self
    }
    #[cfg(all(test, feature = "hybrid-remote"))]
    pub(crate) fn clock_now(operation: &Self) -> Instant {
        operation
            .clock
            .as_ref()
            .map_or_else(Instant::now, |(clock, _)| clock())
    }
    #[cfg(not(all(test, feature = "hybrid-remote")))]
    pub(crate) fn clock_now(_: &Self) -> Instant {
        Instant::now()
    }
    pub(crate) async fn deadline_elapsed(&self) {
        #[cfg(all(test, feature = "hybrid-remote"))]
        if let Some((_, changed)) = &self.clock {
            loop {
                let notified = changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if Self::clock_now(self) >= self.deadline {
                    return;
                }
                notified.await;
            }
        }
        tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)).await;
    }
    pub(crate) fn since(&self, started: Instant) -> Duration {
        Self::clock_now(self).saturating_duration_since(started)
    }

    pub(crate) fn execution(&self) -> ExecutionSnapshot {
        let state = &self.execution;
        ExecutionSnapshot {
            execution_version: 1,
            cpu_ms: state.cpu_ms.load(Ordering::Relaxed),
            network_ms: state.network_ms.load(Ordering::Relaxed),
            backoff_ms: state.backoff_ms.load(Ordering::Relaxed),
            provider_attempts: state.provider_attempts.load(Ordering::Relaxed),
            shared_work: state.shared_work.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn mark_shared_work(&self) {
        self.execution.shared_work.store(true, Ordering::Relaxed);
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn claim_remote_cancellation(&self) -> bool {
        !self
            .execution
            .remote_cancellation_recorded
            .swap(true, Ordering::Relaxed)
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn record_attempt(&self) {
        add(&self.execution.provider_attempts, 1);
    }
    fn timer(&self, kind: TimeKind) -> ExecutionTimer {
        ExecutionTimer {
            operation: self.clone(),
            kind,
            started: Self::clock_now(self),
        }
    }
    pub(crate) fn cpu_timer(&self) -> ExecutionTimer {
        self.timer(TimeKind::Cpu)
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn network_timer(&self) -> ExecutionTimer {
        self.timer(TimeKind::Network)
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn backoff_timer(&self) -> ExecutionTimer {
        self.timer(TimeKind::Backoff)
    }
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn shared_network_timer(&self) -> ExecutionTimer {
        self.timer(TimeKind::SharedNetwork)
    }

    /// Captures a deadline from complete policy and the caller's original start.
    /// Does not resolve defaults, reset elapsed time or start a timer.
    ///
    /// # Errors
    /// Admission for malformed policy or a deadline outside the platform range.
    pub fn new(
        policy: &SearchSettings,
        started_at: Instant,
        cancellation: Cancellation,
    ) -> Result<Self> {
        SearchSpec::validate(policy, &Capabilities::current())?;
        Ok(Self {
            started_at,
            deadline: checked_deadline(
                started_at,
                Duration::from_millis(policy.request_timeout_ms),
            )?,
            cancellation,
            queue_ms: Arc::new(AtomicU64::new(0)),
            execution: Arc::new(Execution::default()),
            #[cfg(all(test, feature = "hybrid-remote"))]
            clock: None,
        })
    }

    /// Explicit composition-owner budget, without the interactive search clamp.
    ///
    /// # Errors
    /// Zero/unrepresentable duration or cancelled/expired original budget.
    pub(crate) fn with_timeout(
        started_at: Instant,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Admission {
                message: "job timeout must be positive".into(),
            });
        }
        let operation = Self {
            started_at,
            deadline: checked_deadline(started_at, timeout)?,
            cancellation,
            queue_ms: Arc::new(AtomicU64::new(0)),
            execution: Arc::new(Execution::default()),
            #[cfg(all(test, feature = "hybrid-remote"))]
            clock: None,
        };
        operation.check()?;
        Ok(operation)
    }

    /// Clamps an explicit caller deadline to this owner's original budget.
    ///
    /// # Errors
    /// Invalid policy, clock range, cancellation or an already-expired budget.
    pub fn constrained(
        policy: &SearchSettings,
        started_at: Instant,
        caller_deadline: Instant,
        cancellation: Cancellation,
    ) -> Result<Self> {
        Self::constrained_at(
            policy,
            started_at,
            caller_deadline,
            cancellation,
            Instant::now(),
        )
    }

    fn constrained_at(
        policy: &SearchSettings,
        started_at: Instant,
        caller_deadline: Instant,
        cancellation: Cancellation,
        now: Instant,
    ) -> Result<Self> {
        let mut operation = Self::new(policy, started_at, cancellation)?;
        operation.deadline = operation.deadline.min(caller_deadline);
        operation.check_at(now)?;
        Ok(operation)
    }

    /// Admission delay accumulated by all clones of this one operation.
    #[must_use]
    pub fn queue_ms(&self) -> u64 {
        self.queue_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn record_queue(&self, duration: Duration) {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let _ = self
            .queue_ms
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(millis))
            });
    }

    /// Original ingress time, before queueing or a reconnect attempt.
    #[must_use]
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// Absolute deadline to pass unchanged to reader-pool admission.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// This caller's token; a clone of the operation shares the same token.
    #[must_use]
    pub fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    /// Checks the boundary before starting another expensive phase.
    ///
    /// # Errors
    /// Admission when cancelled or at/after the original request deadline.
    pub fn check(&self) -> Result<()> {
        self.check_at(Self::clock_now(self))
    }

    /// Remaining portion of the original budget, never a successful zero.
    ///
    /// # Errors
    /// Admission when cancelled or at/after the original request deadline.
    pub fn remaining(&self) -> Result<Duration> {
        self.remaining_at(Self::clock_now(self))
    }

    /// End-to-end elapsed time, including time before a worker starts.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Self::clock_now(self))
    }

    fn check_at(&self, now: Instant) -> Result<()> {
        if now >= self.deadline {
            self.cancellation.cancel();
            return Err(Error::Admission {
                message: "search request deadline expired".to_owned(),
            });
        }
        self.cancellation.check()
    }

    fn remaining_at(&self, now: Instant) -> Result<Duration> {
        self.check_at(now)?;
        Ok(self.deadline.saturating_duration_since(now))
    }

    fn elapsed_at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }
}

fn checked_deadline(started_at: Instant, timeout: Duration) -> Result<Instant> {
    started_at
        .checked_add(timeout)
        .ok_or_else(|| Error::Admission {
            message: "search request deadline exceeds this platform's clock range".to_owned(),
        })
}
