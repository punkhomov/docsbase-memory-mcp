//! Watcher queue, timing, and directory expansion policy.

use std::time::{Duration, Instant};

use super::{BatchRetryPolicy, transient_delay};

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

const DEFAULT_DEBOUNCE_MS: u64 = 1_500;
// FR-15 caps elapsed time from the first event to the end of debounce.
const MAX_DEBOUNCE_MS: u64 = 2_000;
// Preserve the per-watcher capacity bound from T46.
const MAX_QUEUE_CAPACITY: usize = 4096;
const MAX_TRANSIENT_RETRIES: u32 = u32::MAX.ilog(3) + 1;
const MAX_EXPAND_FILES: usize = 10_000;
const MAX_EXPAND_DEPTH: usize = 32;

/// Default quiet window after the last event.
pub const DEBOUNCE: Duration = Duration::from_millis(DEFAULT_DEBOUNCE_MS);
/// Default and maximum batching window from the first event.
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(MAX_DEBOUNCE_MS);

/// Validated watcher capacities and batch timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchSettings {
    debounce_ms: u64,
    max_debounce_ms: u64,
    pub(super) native_queue_capacity: usize,
    pub(super) batch_queue_capacity: usize,
    transient_retries: u32,
    transient_backoff_ms: u64,
    lease_retry_ms: u64,
    pub(super) max_expand_files: usize,
    pub(super) max_expand_depth: usize,
}

impl WatchSettings {
    /// Time to wait after the most recent event.
    #[must_use]
    pub const fn debounce(self) -> Duration {
        Duration::from_millis(self.debounce_ms)
    }

    /// Longest time to wait from the first event.
    #[must_use]
    pub const fn max_debounce(self) -> Duration {
        Duration::from_millis(self.max_debounce_ms)
    }

    pub(super) const fn retry_policy(self) -> BatchRetryPolicy {
        BatchRetryPolicy {
            max_transient_attempts: self.transient_retries,
            lease_retry: Duration::from_millis(self.lease_retry_ms),
            transient_backoff: Duration::from_millis(self.transient_backoff_ms),
        }
    }

    pub(super) fn next_event_timeout(self, elapsed: Duration) -> Option<Duration> {
        let remaining = self.max_debounce().saturating_sub(elapsed);
        (!remaining.is_zero()).then(|| self.debounce().min(remaining))
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WatchPatch {
    debounce_ms: Option<u64>,
    max_debounce_ms: Option<u64>,
    native_queue_capacity: Option<usize>,
    batch_queue_capacity: Option<usize>,
    transient_retries: Option<u32>,
    transient_backoff_ms: Option<u64>,
    lease_retry_ms: Option<u64>,
    max_expand_files: Option<usize>,
    max_expand_depth: Option<usize>,
}

pub(crate) struct WatchSpec;

impl WatchSpec {
    pub(super) const DEFAULTS: WatchSettings = WatchSettings {
        debounce_ms: DEFAULT_DEBOUNCE_MS,
        max_debounce_ms: MAX_DEBOUNCE_MS,
        native_queue_capacity: MAX_QUEUE_CAPACITY,
        batch_queue_capacity: MAX_QUEUE_CAPACITY,
        transient_retries: 3,
        transient_backoff_ms: 100,
        lease_retry_ms: 250,
        max_expand_files: MAX_EXPAND_FILES,
        max_expand_depth: MAX_EXPAND_DEPTH,
    };

    pub(crate) fn view_fields(settings: WatchSettings, view: &mut ViewBuilder<'_>) -> Result<()> {
        for (key, value) in [
            ("watch.debounce_ms", settings.debounce_ms.into()),
            ("watch.max_debounce_ms", settings.max_debounce_ms.into()),
            (
                "watch.native_queue_capacity",
                settings.native_queue_capacity.into(),
            ),
            (
                "watch.batch_queue_capacity",
                settings.batch_queue_capacity.into(),
            ),
            ("watch.transient_retries", settings.transient_retries.into()),
            (
                "watch.transient_backoff_ms",
                settings.transient_backoff_ms.into(),
            ),
            ("watch.lease_retry_ms", settings.lease_retry_ms.into()),
            ("watch.max_expand_files", settings.max_expand_files.into()),
            ("watch.max_expand_depth", settings.max_expand_depth.into()),
        ] {
            view.push(key, value, None)?;
        }
        Ok(())
    }
}

impl SettingsSpec for WatchSpec {
    type Patch = WatchPatch;
    type Settings = WatchSettings;
    type Defaults = WatchSettings;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("watch.debounce_ms", Scope::Project, Unit::Milliseconds),
        FieldSpec::new("watch.max_debounce_ms", Scope::Project, Unit::Milliseconds),
        FieldSpec::new("watch.native_queue_capacity", Scope::Daemon, Unit::Count),
        FieldSpec::new("watch.batch_queue_capacity", Scope::Daemon, Unit::Count),
        FieldSpec::new("watch.transient_retries", Scope::Project, Unit::Count),
        FieldSpec::new(
            "watch.transient_backoff_ms",
            Scope::Project,
            Unit::Milliseconds,
        ),
        FieldSpec::new("watch.lease_retry_ms", Scope::Project, Unit::Milliseconds),
        FieldSpec::new("watch.max_expand_files", Scope::Project, Unit::Count),
        FieldSpec::new("watch.max_expand_depth", Scope::Project, Unit::Count),
    ];

    fn defaults(_: &DefaultsContext) -> WatchSettings {
        Self::DEFAULTS
    }

    fn initial(defaults: &WatchSettings) -> WatchSettings {
        *defaults
    }

    fn overlay(
        settings: &mut WatchSettings,
        patch: WatchPatch,
        _: &WatchSettings,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if !source.permits(Scope::Daemon)
            && (patch.native_queue_capacity.is_some() || patch.batch_queue_capacity.is_some())
        {
            return Err(Error::Admission {
                message: "watch queue capacities are global-only".to_owned(),
            });
        }
        let mut candidate = *settings;
        let mut candidate_origins = origins.clone();
        macro_rules! apply {
            ($($field:ident),+ $(,)?) => {
                $(if let Some(value) = patch.$field {
                    candidate.$field = value;
                    candidate_origins.set(concat!("watch.", stringify!($field)), source)?;
                })+
            };
        }
        apply!(
            debounce_ms,
            max_debounce_ms,
            native_queue_capacity,
            batch_queue_capacity,
            transient_retries,
            transient_backoff_ms,
            lease_retry_ms,
            max_expand_files,
            max_expand_depth,
        );
        Self::validate(&candidate, &Capabilities::current())?;
        *settings = candidate;
        *origins = candidate_origins;
        Ok(())
    }

    fn validate(settings: &WatchSettings, _: &Capabilities) -> Result<()> {
        if settings.debounce_ms == 0
            || settings.debounce_ms > settings.max_debounce_ms
            || settings.max_debounce_ms > MAX_DEBOUNCE_MS
        {
            return Err(Error::Admission {
                message: format!(
                    "watch requires 1 <= watch.debounce_ms <= watch.max_debounce_ms <= {MAX_DEBOUNCE_MS}"
                ),
            });
        }
        if !(1..=MAX_QUEUE_CAPACITY).contains(&settings.native_queue_capacity)
            || !(1..=MAX_QUEUE_CAPACITY).contains(&settings.batch_queue_capacity)
        {
            return Err(Error::Admission {
                message: format!(
                    "watch queue capacities must be between 1 and {MAX_QUEUE_CAPACITY}"
                ),
            });
        }
        if !(1..=MAX_EXPAND_FILES).contains(&settings.max_expand_files)
            || !(1..=MAX_EXPAND_DEPTH).contains(&settings.max_expand_depth)
        {
            return Err(Error::Admission {
                message: format!(
                    "watch requires 1 <= max_expand_files <= {MAX_EXPAND_FILES} and 1 <= max_expand_depth <= {MAX_EXPAND_DEPTH}"
                ),
            });
        }
        let policy = settings.retry_policy();
        let now = Instant::now();
        if settings.transient_retries > MAX_TRANSIENT_RETRIES
            || policy.lease_retry.is_zero()
            || now.checked_add(policy.lease_retry).is_none()
            || now
                .checked_add(transient_delay(&policy, settings.transient_retries))
                .is_none()
        {
            return Err(Error::Admission {
                message: format!(
                    "watch retry durations must fit the platform clock with positive lease_retry_ms and at most {MAX_TRANSIENT_RETRIES} transient retries"
                ),
            });
        }
        Ok(())
    }

    fn impact(
        old: Option<&WatchSettings>,
        next: &WatchSettings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
