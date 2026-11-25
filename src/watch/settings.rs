//! Project-owned timing for the watcher event batch.

use std::time::Duration;

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

/// Default quiet window after the last event.
pub const DEBOUNCE: Duration = Duration::from_millis(DEFAULT_DEBOUNCE_MS);
/// Default and maximum batching window from the first event.
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(MAX_DEBOUNCE_MS);

/// The validated quiet and maximum windows for one watch batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchSettings {
    debounce_ms: u64,
    max_debounce_ms: u64,
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
}

pub(crate) struct WatchSpec;

impl WatchSpec {
    pub(crate) fn view_fields(settings: WatchSettings, view: &mut ViewBuilder<'_>) -> Result<()> {
        view.push("watch.debounce_ms", settings.debounce_ms.into(), None)?;
        view.push(
            "watch.max_debounce_ms",
            settings.max_debounce_ms.into(),
            None,
        )
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
    ];

    fn defaults(_: &DefaultsContext) -> WatchSettings {
        WatchSettings {
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            max_debounce_ms: MAX_DEBOUNCE_MS,
        }
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
        let candidate = WatchSettings {
            debounce_ms: patch.debounce_ms.unwrap_or(settings.debounce_ms),
            max_debounce_ms: patch.max_debounce_ms.unwrap_or(settings.max_debounce_ms),
        };
        Self::validate(&candidate, &Capabilities::current())?;
        let mut candidate_origins = origins.clone();
        if patch.debounce_ms.is_some() {
            candidate_origins.set("watch.debounce_ms", source)?;
        }
        if patch.max_debounce_ms.is_some() {
            candidate_origins.set("watch.max_debounce_ms", source)?;
        }
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
