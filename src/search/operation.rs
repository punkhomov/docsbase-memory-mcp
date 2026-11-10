//! One caller's original search deadline and cooperative cancellation.

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
        })
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
