//! Pure owner of remote execution policy. Constructors consume complete settings.

use std::time::{Duration, Instant};

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Resolved, Scope,
    SettingsSpec, SourceId, Unit,
};
use crate::error::{Error, Result};

/// Complete global remote policy; enabling this section does not enable a provider.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteSettings {
    /// Maximum interactive logical stages, including their retry waits.
    pub interactive_active: usize,
    /// Maximum interactive admission waiters.
    pub interactive_pending: usize,
    /// HTTP attempt and complete response-body timeout.
    pub attempt_timeout_ms: u64,
    /// Total attempts, including the first send.
    pub retry_attempts: u32,
    /// Initial retry delay.
    pub retry_initial_ms: u64,
    /// Maximum exponential retry delay.
    pub retry_max_ms: u64,
    /// Exponential retry multiplier.
    pub retry_multiplier: u32,
    /// Maximum accepted provider Retry-After delay.
    pub max_retry_after_ms: u64,
    /// Exclusive jitter upper bound; zero disables jitter.
    pub jitter_max_ms: u64,
    /// Maximum texts in one embedding HTTP request.
    pub embed_batch_texts: usize,
    /// Target embedding input bytes, with the existing oversized-single-text exception.
    pub embed_batch_bytes: usize,
    /// Maximum actual response body bytes, independent of Content-Length.
    pub max_response_bytes: u64,
}

/// Optional global overrides. Unknown fields are rejected before construction.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePatch {
    /// Override `interactive_active`.
    pub interactive_active: Option<usize>,
    /// Override `interactive_pending`.
    pub interactive_pending: Option<usize>,
    /// Override `attempt_timeout_ms`.
    pub attempt_timeout_ms: Option<u64>,
    /// Override `retry_attempts`.
    pub retry_attempts: Option<u32>,
    /// Override `retry_initial_ms`.
    pub retry_initial_ms: Option<u64>,
    /// Override `retry_max_ms`.
    pub retry_max_ms: Option<u64>,
    /// Override `retry_multiplier`.
    pub retry_multiplier: Option<u32>,
    /// Override `max_retry_after_ms`.
    pub max_retry_after_ms: Option<u64>,
    /// Override `jitter_max_ms`.
    pub jitter_max_ms: Option<u64>,
    /// Override `embed_batch_texts`.
    pub embed_batch_texts: Option<usize>,
    /// Override `embed_batch_bytes`.
    pub embed_batch_bytes: Option<usize>,
    /// Override `max_response_bytes`.
    pub max_response_bytes: Option<u64>,
}

/// Defaults, validation, provenance and runtime-only change assessment.
pub struct RemoteSpec;

impl RemoteSpec {
    /// One owner catalog, also used by explicit compatibility composition.
    pub const DEFAULTS: RemoteSettings = RemoteSettings {
        interactive_active: 4,
        interactive_pending: 32,
        attempt_timeout_ms: 30_000,
        retry_attempts: 4,
        retry_initial_ms: 250,
        retry_max_ms: 4_000,
        retry_multiplier: 2,
        max_retry_after_ms: 30_000,
        jitter_max_ms: 50,
        embed_batch_texts: 128,
        embed_batch_bytes: 512 * 1024,
        max_response_bytes: 32 * 1024 * 1024,
    };

    /// Decode this owner's captured TOML value without runtime or filesystem access.
    ///
    /// # Errors
    /// Malformed section, unknown field or incompatible scalar type.
    pub(crate) fn decode_patch(value: toml::Value) -> Result<RemotePatch> {
        value
            .try_into()
            .map_err(|_| invalid("invalid remote policy fields or types"))
    }

    pub(crate) fn view_fields(
        settings: &RemoteSettings,
        view: &mut crate::config::view::ViewBuilder<'_>,
    ) -> Result<()> {
        let values = serde_json::to_value(settings)
            .map_err(|error| Error::internal_with_source("encode remote policy", error))?;
        let capability = Some(crate::config::view::capability("openrouter")?);
        for field in Self::FIELDS {
            let key = field.key;
            let local = key
                .strip_prefix("remote.")
                .ok_or_else(|| invalid("invalid remote field descriptor"))?;
            view.push(key, values[local].clone(), capability.clone())?;
        }
        Ok(())
    }

    /// Assess runtime policy only; model/vector/index identities are unaffected.
    ///
    /// # Errors
    /// Invalid policy or policy identity encoding failure.
    pub fn assess_change(
        old: Option<&RemoteSettings>,
        next: &RemoteSettings,
    ) -> Result<crate::config::change::Assessment> {
        use crate::config::change::{policy_identity, runtime_assessment};
        let identity = |settings: &RemoteSettings| {
            Self::validate(settings, &Capabilities::current())?;
            policy_identity("remote-policy-v1", settings)
        };
        Ok(runtime_assessment(
            "remote",
            old.map(identity).transpose()?,
            identity(next)?,
            Self::impact(old, next, &ArtifactFacts::default()),
        ))
    }
}

impl SettingsSpec for RemoteSpec {
    type Patch = RemotePatch;
    type Settings = RemoteSettings;
    type Defaults = RemoteSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("remote.interactive_active", Scope::Daemon, Unit::Count),
        FieldSpec::new("remote.interactive_pending", Scope::Daemon, Unit::Count),
        FieldSpec::new(
            "remote.attempt_timeout_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new("remote.retry_attempts", Scope::Daemon, Unit::Count),
        FieldSpec::new("remote.retry_initial_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("remote.retry_max_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("remote.retry_multiplier", Scope::Daemon, Unit::Count),
        FieldSpec::new(
            "remote.max_retry_after_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new("remote.jitter_max_ms", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("remote.embed_batch_texts", Scope::Daemon, Unit::Count),
        FieldSpec::new("remote.embed_batch_bytes", Scope::Daemon, Unit::Bytes),
        FieldSpec::new("remote.max_response_bytes", Scope::Daemon, Unit::Bytes),
    ];

    fn defaults(_: &DefaultsContext) -> RemoteSettings {
        Self::DEFAULTS
    }
    fn initial(defaults: &RemoteSettings) -> RemoteSettings {
        defaults.clone()
    }
    fn overlay(
        settings: &mut RemoteSettings,
        patch: RemotePatch,
        _: &RemoteSettings,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if !source.permits(Self::SCOPE) {
            return Err(invalid("remote policy is global-only"));
        }
        macro_rules! apply {
            ($($field:ident),* $(,)?) => {$(
                if let Some(value) = patch.$field {
                    settings.$field = value;
                    origins.set(concat!("remote.", stringify!($field)), source)?;
                }
            )*};
        }
        apply!(
            interactive_active,
            interactive_pending,
            attempt_timeout_ms,
            retry_attempts,
            retry_initial_ms,
            retry_max_ms,
            retry_multiplier,
            max_retry_after_ms,
            jitter_max_ms,
            embed_batch_texts,
            embed_batch_bytes,
            max_response_bytes
        );
        Ok(())
    }
    fn validate(settings: &RemoteSettings, _: &Capabilities) -> Result<()> {
        let max = tokio::sync::Semaphore::MAX_PERMITS;
        if settings.interactive_active == 0
            || settings.interactive_active > max
            || settings.interactive_pending > max
            || settings.retry_attempts == 0
            || settings.retry_multiplier == 0
            || settings.embed_batch_texts == 0
            || settings.embed_batch_bytes == 0
            || settings.max_response_bytes == 0
            || settings.max_response_bytes > isize::MAX as u64
            || settings
                .embed_batch_texts
                .checked_mul(std::mem::size_of::<String>())
                .is_none_or(|bytes| bytes > isize::MAX as usize)
            || settings.retry_initial_ms > settings.retry_max_ms
        {
            return Err(invalid("invalid remote count, allocation or retry bounds"));
        }
        for (milliseconds, positive) in [
            (settings.attempt_timeout_ms, true),
            (settings.retry_initial_ms, false),
            (settings.retry_max_ms, false),
            (settings.max_retry_after_ms, false),
            (settings.jitter_max_ms, false),
        ] {
            if (positive && milliseconds == 0)
                || Instant::now()
                    .checked_add(Duration::from_millis(milliseconds))
                    .is_none()
            {
                return Err(invalid(
                    "remote durations must be representable; attempt timeout must be positive",
                ));
            }
        }
        Ok(())
    }
    fn impact(
        old: Option<&RemoteSettings>,
        next: &RemoteSettings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}

/// Resolve omitted policy only at a standalone compatibility composition boundary.
///
/// # Errors
/// Invalid owner catalog.
pub fn standalone() -> Result<Resolved<RemoteSettings>> {
    crate::config::resolve::resolve_component::<RemoteSpec>(&DefaultsContext::standalone(), [])
}

fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
