//! One caller's original search deadline and cooperative cancellation.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
}

impl SearchOperation {
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
        })
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
        self.check_at(Instant::now())
    }

    /// Remaining portion of the original budget, never a successful zero.
    ///
    /// # Errors
    /// Admission when cancelled or at/after the original request deadline.
    pub fn remaining(&self) -> Result<Duration> {
        self.remaining_at(Instant::now())
    }

    /// End-to-end elapsed time, including time before a worker starts.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now())
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
