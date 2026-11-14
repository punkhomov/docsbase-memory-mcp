//! Pure daemon policy owner. Runtime consumers receive complete immutable settings.
use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::error::{Error, Result};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A complete profile selector; overlays replace this whole value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DaemonProfile {
    /// Ordinary shared multi-project service.
    Shared,
    /// Explicit lexical service for one existing registered absolute root.
    Website {
        /// Existing project root; canonical admission belongs to the launch boundary.
        project_root: PathBuf,
    },
}
impl<'de> serde::Deserialize<'de> for DaemonProfile {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // A serde internally-tagged unit variant ignores extra keys. A fieldless
        // struct variant enforces the complete Shared shape before any overlay.
        #[derive(serde::Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum Shape {
            Shared {},
            Website { project_root: PathBuf },
        }
        let profile = match Shape::deserialize(deserializer)? {
            Shape::Shared {} => Self::Shared,
            Shape::Website { project_root } => Self::Website { project_root },
        };
        profile.validate().map_err(serde::de::Error::custom)?;
        Ok(profile)
    }
}
impl DaemonProfile {
    fn validate(&self) -> Result<()> {
        if let Self::Website { project_root } = self
            && !project_root.is_absolute()
        {
            return Err(invalid("daemon.profile.project_root must be absolute"));
        }
        Ok(())
    }
}
/// All daemon resource policy, immutable until restart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DaemonSettings {
    /// Complete selected profile.
    pub profile: DaemonProfile,
    /// Owner policy: `max_connections`.
    pub max_connections: usize,
    /// Owner policy: `max_events`.
    pub max_events: usize,
    /// Owner policy: `max_sessions`.
    pub max_sessions: usize,
    /// Owner policy: `search_active`.
    pub search_active: usize,
    /// Owner policy: `search_pending`.
    pub search_pending: usize,
    /// Owner policy: `control_active`.
    pub control_active: usize,
    /// Owner policy: `control_pending`.
    pub control_pending: usize,
    /// Maximum physically running DNS helpers; their pending cap derives from remote slots.
    pub helper_active: usize,
    /// Owner policy: `max_blocking_threads`.
    pub max_blocking_threads: usize,
    /// Owner policy: `idle_grace_ms`.
    pub idle_grace_ms: u64,
    /// Owner policy: `control_timeout_ms`.
    pub control_timeout_ms: u64,
    /// Owner policy: `poll_interval_ms`.
    pub poll_interval_ms: u64,
    /// Owner policy: `janitor_interval_ms`.
    pub janitor_interval_ms: u64,
    /// Owner policy: `reply_drain_ms`.
    pub reply_drain_ms: u64,
    /// Owner policy: `drain_poll_ms`.
    pub drain_poll_ms: u64,
    // Applicability is semantic: an explicit Shared grace cannot be ignored by Website.
    idle_grace_explicit: bool,
}
// Runtime identity excludes source/applicability facts retained by Settings and
// Provenance. Hashing and equality must use this same effective projection.
#[derive(PartialEq, Eq, serde::Serialize)]
struct EffectiveDaemonPolicy<'a> {
    profile: &'a DaemonProfile,
    max_connections: usize,
    max_events: usize,
    max_sessions: usize,
    search_active: usize,
    search_pending: usize,
    control_active: usize,
    control_pending: usize,
    helper_active: usize,
    max_blocking_threads: usize,
    idle_grace_ms: u64,
    control_timeout_ms: u64,
    poll_interval_ms: u64,
    janitor_interval_ms: u64,
    reply_drain_ms: u64,
    drain_poll_ms: u64,
}
impl DaemonSettings {
    fn effective_policy(&self) -> EffectiveDaemonPolicy<'_> {
        EffectiveDaemonPolicy {
            profile: &self.profile,
            max_connections: self.max_connections,
            max_events: self.max_events,
            max_sessions: self.max_sessions,
            search_active: self.search_active,
            search_pending: self.search_pending,
            control_active: self.control_active,
            control_pending: self.control_pending,
            helper_active: self.helper_active,
            max_blocking_threads: self.max_blocking_threads,
            idle_grace_ms: self.idle_grace_ms,
            control_timeout_ms: self.control_timeout_ms,
            poll_interval_ms: self.poll_interval_ms,
            janitor_interval_ms: self.janitor_interval_ms,
            reply_drain_ms: self.reply_drain_ms,
            drain_poll_ms: self.drain_poll_ms,
        }
    }
}

/// Optional global or typed CLI daemon overrides.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonPatch {
    /// Complete selector, never a field-wise mode/root merge.
    pub profile: Option<DaemonProfile>,
    /// Explicit `max_connections` override.
    pub max_connections: Option<usize>,
    /// Explicit `max_events` override.
    pub max_events: Option<usize>,
    /// Explicit `max_sessions` override.
    pub max_sessions: Option<usize>,
    /// Explicit `search_active` override.
    pub search_active: Option<usize>,
    /// Explicit `search_pending` override.
    pub search_pending: Option<usize>,
    /// Explicit `control_active` override.
    pub control_active: Option<usize>,
    /// Explicit `control_pending` override.
    pub control_pending: Option<usize>,
    /// Explicit DNS helper capacity.
    pub helper_active: Option<usize>,
    /// Explicit `max_blocking_threads` override.
    pub max_blocking_threads: Option<usize>,
    /// Explicit `idle_grace_ms` override.
    pub idle_grace_ms: Option<u64>,
    /// Explicit `control_timeout_ms` override.
    pub control_timeout_ms: Option<u64>,
    /// Explicit `poll_interval_ms` override.
    pub poll_interval_ms: Option<u64>,
    /// Explicit `janitor_interval_ms` override.
    pub janitor_interval_ms: Option<u64>,
    /// Explicit `reply_drain_ms` override.
    pub reply_drain_ms: Option<u64>,
    /// Explicit `drain_poll_ms` override.
    pub drain_poll_ms: Option<u64>,
}
/// Defaults, validation, provenance, views and restart effects for daemon policy.
pub struct DaemonSpec;
impl DaemonSpec {
    /// Single catalog also used by documented compatibility adapters.
    pub const DEFAULTS: DaemonSettings = DaemonSettings {
        profile: DaemonProfile::Shared,
        max_connections: 64,
        max_events: 1024,
        max_sessions: 256,
        search_active: 4,
        search_pending: 32,
        control_active: 2,
        control_pending: 16,
        helper_active: 1,
        max_blocking_threads: 512,
        idle_grace_ms: 5000,
        control_timeout_ms: 2000,
        poll_interval_ms: 50,
        janitor_interval_ms: 500,
        reply_drain_ms: 5000,
        drain_poll_ms: 20,
        idle_grace_explicit: false,
    };
    /// Pure runtime assessment. Applying a changed policy needs daemon restart.
    ///
    /// # Errors
    /// Invalid complete settings or policy encoding.
    pub fn assess_change(
        old: Option<&DaemonSettings>,
        next: &DaemonSettings,
    ) -> Result<crate::config::change::Assessment> {
        use crate::config::change::{
            Blocker, BlockerCode, ResourceId, policy_identity, runtime_assessment,
        };
        let identity = |settings: &DaemonSettings| {
            Self::validate(settings, &Capabilities::current())?;
            policy_identity("daemon-policy-v1", &settings.effective_policy())
        };
        let mut assessment = runtime_assessment(
            "daemon",
            old.map(identity).transpose()?,
            identity(next)?,
            Self::impact(old, next, &ArtifactFacts::default()),
        );
        if !assessment.actions.is_empty() {
            assessment.blockers.push(Blocker {
                resource: Some(ResourceId::Runtime("daemon".to_owned())),
                code: BlockerCode::ExecutorUnavailable,
            });
        }
        Ok(assessment)
    }
    pub(crate) fn view_fields(
        settings: &DaemonSettings,
        view: &mut crate::config::view::ViewBuilder<'_>,
    ) -> Result<()> {
        view.push(
            "daemon.helper_active",
            serde_json::json!(settings.helper_active),
            None,
        )?;
        view.push(
            "daemon.profile",
            serde_json::to_value(&settings.profile)
                .map_err(|e| Error::internal_with_source("encode daemon profile", e))?,
            None,
        )?;
        view.push(
            "daemon.max_connections",
            serde_json::json!(settings.max_connections),
            None,
        )?;
        view.push(
            "daemon.max_events",
            serde_json::json!(settings.max_events),
            None,
        )?;
        view.push(
            "daemon.max_sessions",
            serde_json::json!(settings.max_sessions),
            None,
        )?;
        view.push(
            "daemon.search_active",
            serde_json::json!(settings.search_active),
            None,
        )?;
        view.push(
            "daemon.search_pending",
            serde_json::json!(settings.search_pending),
            None,
        )?;
        view.push(
            "daemon.control_active",
            serde_json::json!(settings.control_active),
            None,
        )?;
        view.push(
            "daemon.control_pending",
            serde_json::json!(settings.control_pending),
            None,
        )?;
        view.push(
            "daemon.max_blocking_threads",
            serde_json::json!(settings.max_blocking_threads),
            None,
        )?;
        view.push(
            "daemon.idle_grace_ms",
            serde_json::json!(settings.idle_grace_ms),
            None,
        )?;
        view.push(
            "daemon.control_timeout_ms",
            serde_json::json!(settings.control_timeout_ms),
            None,
        )?;
        view.push(
            "daemon.poll_interval_ms",
            serde_json::json!(settings.poll_interval_ms),
            None,
        )?;
        view.push(
            "daemon.janitor_interval_ms",
            serde_json::json!(settings.janitor_interval_ms),
            None,
        )?;
        view.push(
            "daemon.reply_drain_ms",
            serde_json::json!(settings.reply_drain_ms),
            None,
        )?;
        view.push(
            "daemon.drain_poll_ms",
            serde_json::json!(settings.drain_poll_ms),
            None,
        )?;
        Ok(())
    }
}
impl SettingsSpec for DaemonSpec {
    type Patch = DaemonPatch;
    type Settings = DaemonSettings;
    type Defaults = DaemonSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("daemon.profile", Scope::Daemon, Unit::Name),
        FieldSpec::new("daemon.max_connections", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.max_events", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.max_sessions", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.search_active", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.search_pending", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.control_active", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.control_pending", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.helper_active", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.max_blocking_threads", Scope::Daemon, Unit::Count),
        FieldSpec::new("daemon.idle_grace_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new(
            "daemon.control_timeout_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new("daemon.poll_interval_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new(
            "daemon.janitor_interval_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new("daemon.reply_drain_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("daemon.drain_poll_ms", Scope::Daemon, Unit::Milliseconds),
    ];
    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        Self::DEFAULTS.clone()
    }
    fn initial(defaults: &Self::Defaults) -> Self::Settings {
        defaults.clone()
    }
    fn overlay(
        settings: &mut Self::Settings,
        patch: Self::Patch,
        _: &Self::Defaults,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if let Some(value) = patch.helper_active {
            settings.helper_active = value;
            origins.set("daemon.helper_active", source)?;
        }
        if let Some(profile) = patch.profile {
            profile.validate()?;
            settings.profile = profile;
            origins.set("daemon.profile", source)?;
        }
        if patch.idle_grace_ms.is_some() {
            settings.idle_grace_explicit = true;
        }
        if let Some(value) = patch.max_connections {
            settings.max_connections = value;
            origins.set("daemon.max_connections", source)?;
        }
        if let Some(value) = patch.max_events {
            settings.max_events = value;
            origins.set("daemon.max_events", source)?;
        }
        if let Some(value) = patch.max_sessions {
            settings.max_sessions = value;
            origins.set("daemon.max_sessions", source)?;
        }
        if let Some(value) = patch.search_active {
            settings.search_active = value;
            origins.set("daemon.search_active", source)?;
        }
        if let Some(value) = patch.search_pending {
            settings.search_pending = value;
            origins.set("daemon.search_pending", source)?;
        }
        if let Some(value) = patch.control_active {
            settings.control_active = value;
            origins.set("daemon.control_active", source)?;
        }
        if let Some(value) = patch.control_pending {
            settings.control_pending = value;
            origins.set("daemon.control_pending", source)?;
        }
        if let Some(value) = patch.max_blocking_threads {
            settings.max_blocking_threads = value;
            origins.set("daemon.max_blocking_threads", source)?;
        }
        if let Some(value) = patch.idle_grace_ms {
            settings.idle_grace_ms = value;
            origins.set("daemon.idle_grace_ms", source)?;
        }
        if let Some(value) = patch.control_timeout_ms {
            settings.control_timeout_ms = value;
            origins.set("daemon.control_timeout_ms", source)?;
        }
        if let Some(value) = patch.poll_interval_ms {
            settings.poll_interval_ms = value;
            origins.set("daemon.poll_interval_ms", source)?;
        }
        if let Some(value) = patch.janitor_interval_ms {
            settings.janitor_interval_ms = value;
            origins.set("daemon.janitor_interval_ms", source)?;
        }
        if let Some(value) = patch.reply_drain_ms {
            settings.reply_drain_ms = value;
            origins.set("daemon.reply_drain_ms", source)?;
        }
        if let Some(value) = patch.drain_poll_ms {
            settings.drain_poll_ms = value;
            origins.set("daemon.drain_poll_ms", source)?;
        }
        validate_applicability(settings)
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        settings.profile.validate()?;
        validate_applicability(settings)?;
        let max = tokio::sync::Semaphore::MAX_PERMITS;
        for (key, value, zero_allowed) in [
            ("daemon.max_connections", settings.max_connections, false),
            ("daemon.max_events", settings.max_events, false),
            ("daemon.max_sessions", settings.max_sessions, false),
            ("daemon.search_active", settings.search_active, false),
            ("daemon.search_pending", settings.search_pending, true),
            ("daemon.control_active", settings.control_active, false),
            ("daemon.control_pending", settings.control_pending, true),
            ("daemon.helper_active", settings.helper_active, false),
            (
                "daemon.max_blocking_threads",
                settings.max_blocking_threads,
                false,
            ),
        ] {
            if value > max || (!zero_allowed && value == 0) {
                return Err(invalid(&format!(
                    "{key} must {}fit Tokio's MAX_PERMITS ({max})",
                    if zero_allowed { "" } else { "be positive and " }
                )));
            }
        }
        // No preallocation from an unchecked count, even on smaller pointer-width targets.
        if settings
            .max_sessions
            .checked_mul(std::mem::size_of::<(
                u64,
                crate::daemon::session::SessionInfo,
                tokio::sync::oneshot::Sender<()>,
            )>())
            .is_none_or(|bytes| bytes > isize::MAX as usize)
        {
            return Err(invalid("daemon.max_sessions allocation size overflow"));
        }
        for (key, ms, zero_allowed) in [
            ("daemon.idle_grace_ms", settings.idle_grace_ms, true),
            (
                "daemon.control_timeout_ms",
                settings.control_timeout_ms,
                false,
            ),
            ("daemon.poll_interval_ms", settings.poll_interval_ms, false),
            (
                "daemon.janitor_interval_ms",
                settings.janitor_interval_ms,
                false,
            ),
            ("daemon.reply_drain_ms", settings.reply_drain_ms, false),
            ("daemon.drain_poll_ms", settings.drain_poll_ms, false),
        ] {
            if (!zero_allowed && ms == 0)
                || Instant::now()
                    .checked_add(Duration::from_millis(ms))
                    .is_none()
            {
                return Err(invalid(&format!(
                    "{key} must be {}representable by Instant",
                    if zero_allowed { "" } else { "positive and " }
                )));
            }
        }
        let required = settings
            .search_active
            .checked_add(settings.control_active)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_add(settings.helper_active))
            .ok_or_else(|| invalid("daemon lexical worker capacity overflow"))?;
        if required > settings.max_blocking_threads {
            return Err(invalid(
                "daemon.max_blocking_threads must fit search_active + control_active + fixed sync1 + helper_active",
            ));
        }
        Ok(())
    }
    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(
            old.map(DaemonSettings::effective_policy).as_ref(),
            &next.effective_policy(),
        )
    }
}
fn validate_applicability(settings: &DaemonSettings) -> Result<()> {
    if matches!(settings.profile, DaemonProfile::Website { .. })
        && (settings.idle_grace_explicit
            || settings.idle_grace_ms != DaemonSpec::DEFAULTS.idle_grace_ms)
    {
        return Err(invalid("daemon.idle_grace_ms is inapplicable to Website"));
    }
    Ok(())
}
fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
/// Resolves the owner's standalone policy at a compatibility boundary.
///
/// # Errors
/// Invalid owner defaults.
pub fn standalone() -> Result<crate::config::schema::Resolved<DaemonSettings>> {
    crate::config::resolve::resolve_component::<DaemonSpec>(&DefaultsContext::standalone(), [])
}
