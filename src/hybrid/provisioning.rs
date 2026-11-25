//! Installation policy for pinned local models and their native runtime.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

/// Hard cap for a single model artifact (hybrid-local NFR-3).
pub const MODEL_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Hard cap for one model's artifact set (hybrid-local NFR-3).
pub const MODEL_MAX_TOTAL_BYTES: u64 = 3 * 1024 * 1024 * 1024;
/// Hard cap for the compressed runtime artifact (hybrid-local NFR-3).
pub const RUNTIME_MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Hard cap for the extracted runtime library (hybrid-local NFR-3).
pub const RUNTIME_MAX_LIBRARY_BYTES: u64 = 512 * 1024 * 1024;
/// Minimum free-space allowance after accounting for the installation (FR-13).
pub const MIN_HEADROOM_BYTES: u64 = 256 * 1024 * 1024;

/// Complete installation-wide policy, resolved before provisioning begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProvisioningSettings {
    /// Policy for fetching a pinned local model.
    pub models: ModelProvisioningSettings,
    /// Policy for installing the pinned ONNX Runtime.
    pub runtime: RuntimeProvisioningSettings,
}

/// Size, disk, HTTP and exclusive-lock policy for model fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelProvisioningSettings {
    pub(crate) max_file_bytes: u64,
    pub(crate) max_total_bytes: u64,
    pub(crate) headroom_bytes: u64,
    pub(crate) connect_timeout_ms: u64,
    pub(crate) response_timeout_ms: u64,
    pub(crate) body_timeout_ms: u64,
    pub(crate) lock_attempts: u32,
    pub(crate) lock_retry_delay_ms: u64,
}

/// Size, disk and HTTP policy for installing the native runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RuntimeProvisioningSettings {
    pub(crate) max_archive_bytes: u64,
    pub(crate) max_library_bytes: u64,
    pub(crate) headroom_bytes: u64,
    pub(crate) connect_timeout_ms: u64,
    pub(crate) response_timeout_ms: u64,
    pub(crate) body_timeout_ms: u64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvisioningPatch {
    models: Option<ModelPatch>,
    runtime: Option<RuntimePatch>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelPatch {
    max_file_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
    headroom_bytes: Option<u64>,
    connect_timeout_ms: Option<u64>,
    response_timeout_ms: Option<u64>,
    body_timeout_ms: Option<u64>,
    lock_attempts: Option<u32>,
    lock_retry_delay_ms: Option<u64>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePatch {
    max_archive_bytes: Option<u64>,
    max_library_bytes: Option<u64>,
    headroom_bytes: Option<u64>,
    connect_timeout_ms: Option<u64>,
    response_timeout_ms: Option<u64>,
    body_timeout_ms: Option<u64>,
}

pub(crate) struct ProvisioningSpec;

impl ProvisioningSpec {
    pub(crate) const DEFAULTS: ProvisioningSettings = ProvisioningSettings {
        models: ModelProvisioningSettings {
            max_file_bytes: MODEL_MAX_FILE_BYTES,
            max_total_bytes: MODEL_MAX_TOTAL_BYTES,
            headroom_bytes: MIN_HEADROOM_BYTES,
            connect_timeout_ms: 30_000,
            response_timeout_ms: 120_000,
            body_timeout_ms: 300_000,
            lock_attempts: 50,
            lock_retry_delay_ms: 20,
        },
        runtime: RuntimeProvisioningSettings {
            max_archive_bytes: RUNTIME_MAX_ARCHIVE_BYTES,
            max_library_bytes: RUNTIME_MAX_LIBRARY_BYTES,
            headroom_bytes: MIN_HEADROOM_BYTES,
            connect_timeout_ms: 30_000,
            response_timeout_ms: 120_000,
            body_timeout_ms: 600_000,
        },
    };

    pub(crate) fn view_fields(
        settings: &ProvisioningSettings,
        view: &mut ViewBuilder<'_>,
    ) -> Result<()> {
        let values = serde_json::to_value(settings)
            .map_err(|error| Error::internal_with_source("encode provisioning settings", error))?;
        let capability = Some(crate::config::view::capability("fastembed")?);
        for field in Self::FIELDS {
            let mut keys = field.key.split('.').skip(1);
            let section = keys
                .next()
                .ok_or_else(|| invalid("invalid provisioning descriptor"))?;
            let name = keys
                .next()
                .ok_or_else(|| invalid("invalid provisioning descriptor"))?;
            view.push(field.key, values[section][name].clone(), capability.clone())?;
        }
        Ok(())
    }
}

macro_rules! field {
    ($section:ident.$name:ident, $unit:ident) => {
        FieldSpec::new(
            concat!(
                "provisioning.",
                stringify!($section),
                ".",
                stringify!($name)
            ),
            Scope::Machine,
            Unit::$unit,
        )
    };
}

impl SettingsSpec for ProvisioningSpec {
    type Patch = ProvisioningPatch;
    type Settings = ProvisioningSettings;
    type Defaults = ProvisioningSettings;
    const SCOPE: Scope = Scope::Machine;
    const FIELDS: &'static [FieldSpec] = &[
        field!(models.max_file_bytes, Bytes),
        field!(models.max_total_bytes, Bytes),
        field!(models.headroom_bytes, Bytes),
        field!(models.connect_timeout_ms, Milliseconds),
        field!(models.response_timeout_ms, Milliseconds),
        field!(models.body_timeout_ms, Milliseconds),
        field!(models.lock_attempts, Count),
        field!(models.lock_retry_delay_ms, Milliseconds),
        field!(runtime.max_archive_bytes, Bytes),
        field!(runtime.max_library_bytes, Bytes),
        field!(runtime.headroom_bytes, Bytes),
        field!(runtime.connect_timeout_ms, Milliseconds),
        field!(runtime.response_timeout_ms, Milliseconds),
        field!(runtime.body_timeout_ms, Milliseconds),
    ];

    fn defaults(_: &DefaultsContext) -> ProvisioningSettings {
        Self::DEFAULTS
    }

    fn initial(defaults: &ProvisioningSettings) -> ProvisioningSettings {
        *defaults
    }

    fn overlay(
        settings: &mut ProvisioningSettings,
        patch: ProvisioningPatch,
        _: &ProvisioningSettings,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if !source.permits(Self::SCOPE) {
            return Err(invalid("provisioning is global-only"));
        }
        macro_rules! apply {
            ($section:ident, $($name:ident),+ $(,)?) => {
                if let Some(patch) = patch.$section {
                    $(if let Some(value) = patch.$name {
                        settings.$section.$name = value;
                        origins.set(
                            concat!("provisioning.", stringify!($section), ".", stringify!($name)),
                            source,
                        )?;
                    })+
                }
            };
        }
        apply!(
            models,
            max_file_bytes,
            max_total_bytes,
            headroom_bytes,
            connect_timeout_ms,
            response_timeout_ms,
            body_timeout_ms,
            lock_attempts,
            lock_retry_delay_ms,
        );
        apply!(
            runtime,
            max_archive_bytes,
            max_library_bytes,
            headroom_bytes,
            connect_timeout_ms,
            response_timeout_ms,
            body_timeout_ms,
        );
        Self::validate(settings, &Capabilities::current())
    }

    fn validate(settings: &ProvisioningSettings, _: &Capabilities) -> Result<()> {
        let models = &settings.models;
        let runtime = &settings.runtime;
        if !(1..=MODEL_MAX_FILE_BYTES).contains(&models.max_file_bytes)
            || !(models.max_file_bytes..=MODEL_MAX_TOTAL_BYTES).contains(&models.max_total_bytes)
            || !(1..=RUNTIME_MAX_ARCHIVE_BYTES).contains(&runtime.max_archive_bytes)
            || !(1..=RUNTIME_MAX_LIBRARY_BYTES).contains(&runtime.max_library_bytes)
            || models.headroom_bytes < MIN_HEADROOM_BYTES
            || runtime.headroom_bytes < MIN_HEADROOM_BYTES
        {
            return Err(invalid(
                "invalid provisioning byte caps or free-space headroom",
            ));
        }
        let timeouts = [
            models.connect_timeout_ms,
            models.response_timeout_ms,
            models.body_timeout_ms,
            runtime.connect_timeout_ms,
            runtime.response_timeout_ms,
            runtime.body_timeout_ms,
        ];
        let now = Instant::now();
        if timeouts
            .iter()
            .any(|&ms| ms == 0 || now.checked_add(Duration::from_millis(ms)).is_none())
            || models.lock_attempts == 0
            || models
                .lock_retry_delay_ms
                .checked_mul(u64::from(models.lock_attempts - 1))
                .and_then(|ms| now.checked_add(Duration::from_millis(ms)))
                .is_none()
        {
            return Err(invalid(
                "invalid provisioning timeouts or lock retry budget",
            ));
        }
        Ok(())
    }

    fn impact(
        old: Option<&ProvisioningSettings>,
        next: &ProvisioningSettings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}

fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}

#[cfg(feature = "hybrid-local")]
pub(super) fn http_agent(connect_ms: u64, response_ms: u64, body_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(Duration::from_millis(connect_ms)))
        .timeout_recv_response(Some(Duration::from_millis(response_ms)))
        .timeout_recv_body(Some(Duration::from_millis(body_ms)))
        .build()
        .new_agent()
}

#[cfg(feature = "hybrid-local")]
pub(super) fn required_free_bytes(expected: u64, headroom: u64) -> u64 {
    expected
        .saturating_add(expected / 2)
        .saturating_add(headroom)
}

#[cfg(all(test, feature = "hybrid-local"))]
mod tests {
    use super::*;

    #[test]
    fn http_agent_applies_the_three_independent_deadlines() {
        let settings = http_agent(23, 47, 89);
        assert!(settings.config().https_only());
        let timeouts = settings.config().timeouts();
        assert_eq!(timeouts.connect, Some(Duration::from_millis(23)));
        assert_eq!(timeouts.recv_response, Some(Duration::from_millis(47)));
        assert_eq!(timeouts.recv_body, Some(Duration::from_millis(89)));
    }
}
