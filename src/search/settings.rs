//! Pure owner of search request policy; runtime consumers receive complete data.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::error::{Error, Result};

/// Resolves standalone search policy at a compatibility composition boundary.
/// Runtime consumers receive complete settings and never fill absent fields.
///
/// # Errors
/// Returns an admission error if the owner's default policy is invalid.
pub fn standalone() -> Result<crate::config::schema::Resolved<SearchSettings>> {
    crate::config::resolve::resolve_component::<SearchSpec>(&DefaultsContext::standalone(), [])
}

/// Complete daemon-wide request policy; byte limits do not measure process RSS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSettings {
    /// Additional document-cap refill ceiling, never below the ordinary base window.
    pub dedupe_max_candidates: usize,
    /// Total request budget in milliseconds, including queues and retries.
    pub request_timeout_ms: u64,
    /// Maximum charged bytes for one preparation scope or captured request payload.
    pub max_request_payload_bytes: u64,
    /// Maximum charged bytes retained by concurrent scopes/payloads in one daemon.
    pub max_retained_payload_bytes: u64,
}

/// Optional overrides in the global `[search]` table or a typed resolution.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPatch {
    /// Additional document-cap refill ceiling (`1..=MAX_HITS`).
    pub dedupe_max_candidates: Option<usize>,
    /// Total request budget in milliseconds, including queues and retries.
    pub request_timeout_ms: Option<u64>,
    /// Maximum charged bytes for one preparation scope or captured request payload.
    pub max_request_payload_bytes: Option<u64>,
    /// Maximum charged bytes retained by concurrent scopes/payloads in one daemon.
    pub max_retained_payload_bytes: Option<u64>,
}

/// Owner of defaults, validation, field provenance and local policy impact.
pub struct SearchSpec;

impl SettingsSpec for SearchSpec {
    type Patch = SearchPatch;
    type Settings = SearchSettings;
    type Defaults = SearchSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("search.dedupe_max_candidates", Scope::Daemon, Unit::Count),
        FieldSpec::new(
            "search.request_timeout_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new(
            "search.max_request_payload_bytes",
            Scope::Daemon,
            Unit::Bytes,
        ),
        FieldSpec::new(
            "search.max_retained_payload_bytes",
            Scope::Daemon,
            Unit::Bytes,
        ),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        SearchSettings {
            dedupe_max_candidates: 1000,
            request_timeout_ms: 30_000,
            max_request_payload_bytes: 64 * 1024 * 1024,
            max_retained_payload_bytes: 256 * 1024 * 1024,
        }
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
        if let Some(value) = patch.dedupe_max_candidates {
            settings.dedupe_max_candidates = value;
            origins.set("search.dedupe_max_candidates", source)?;
        }
        if let Some(value) = patch.request_timeout_ms {
            settings.request_timeout_ms = value;
            origins.set("search.request_timeout_ms", source)?;
        }
        if let Some(value) = patch.max_request_payload_bytes {
            settings.max_request_payload_bytes = value;
            origins.set("search.max_request_payload_bytes", source)?;
        }
        if let Some(value) = patch.max_retained_payload_bytes {
            settings.max_retained_payload_bytes = value;
            origins.set("search.max_retained_payload_bytes", source)?;
        }
        Ok(())
    }

    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        if !(1..=crate::index::tantivy_index::MAX_HITS).contains(&settings.dedupe_max_candidates) {
            return Err(Error::Admission {
                message: "search.dedupe_max_candidates must be between 1 and 1000".to_owned(),
            });
        }
        if settings.request_timeout_ms == 0 {
            return Err(Error::Admission {
                message: "search.request_timeout_ms must be positive".to_owned(),
            });
        }
        if settings.max_request_payload_bytes == 0
            || settings.max_retained_payload_bytes == 0
            || settings.max_request_payload_bytes > settings.max_retained_payload_bytes
        {
            return Err(Error::Admission {
                message: "search payload limits must be positive and max_request_payload_bytes must not exceed max_retained_payload_bytes".to_owned(),
            });
        }
        Ok(())
    }

    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}

impl SearchSpec {
    pub(crate) fn view_fields(
        settings: &SearchSettings,
        view: &mut crate::config::view::ViewBuilder<'_>,
    ) -> Result<()> {
        for (key, value) in [
            (
                "search.dedupe_max_candidates",
                serde_json::json!(settings.dedupe_max_candidates),
            ),
            (
                "search.request_timeout_ms",
                serde_json::json!(settings.request_timeout_ms),
            ),
            (
                "search.max_request_payload_bytes",
                serde_json::json!(settings.max_request_payload_bytes),
            ),
            (
                "search.max_retained_payload_bytes",
                serde_json::json!(settings.max_retained_payload_bytes),
            ),
        ] {
            view.push(key, value, None)?;
        }
        Ok(())
    }
}

impl SearchSpec {
    /// Assess only search runtime policy; no index/vector identity is affected.
    /// The old policy is caller-supplied, not a live daemon observation.
    ///
    /// # Errors
    /// Invalid old/new settings or policy encoding.
    pub fn assess_change(
        old: Option<&SearchSettings>,
        next: &SearchSettings,
    ) -> Result<crate::config::change::Assessment> {
        use crate::config::change::{policy_identity, runtime_assessment};
        let identity = |value: &SearchSettings| {
            Self::validate(value, &Capabilities::current())?;
            policy_identity(
                "search-policy-v1",
                &(
                    value.dedupe_max_candidates,
                    value.request_timeout_ms,
                    value.max_request_payload_bytes,
                    value.max_retained_payload_bytes,
                ),
            )
        };
        Ok(runtime_assessment(
            "search",
            old.map(identity).transpose()?,
            identity(next)?,
            Self::impact(old, next, &ArtifactFacts::default()),
        ))
    }
}
