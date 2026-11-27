//! Installation-wide timeout policy for local IPC and MCP callers.

use std::time::Duration;

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

const IPC_IO: FieldSpec = FieldSpec::new(
    "client.ipc_io_timeout_ms",
    Scope::Machine,
    Unit::Milliseconds,
);
const MCP_QUICK: FieldSpec = FieldSpec::new(
    "client.mcp_quick_timeout_ms",
    Scope::Machine,
    Unit::Milliseconds,
);
const MCP_LONG: FieldSpec = FieldSpec::new(
    "client.mcp_long_timeout_ms",
    Scope::Machine,
    Unit::Milliseconds,
);

// Reserve the Windows NMPWAIT_WAIT_FOREVER sentinel and keep the same source range on every OS.
const MAX_TIMEOUT: Duration = Duration::from_millis((u32::MAX - 1) as u64);

/// Complete timeout policy resolved before opening a client connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientSettings {
    ipc_io: Duration,
    mcp_quick: Duration,
    mcp_long: Duration,
}

impl ClientSettings {
    /// Per blocking read/write and Windows busy-pipe connection admission.
    #[must_use]
    pub const fn ipc_io_timeout(&self) -> Duration {
        self.ipc_io
    }

    /// Per MCP connection setup, quick exchange or post-index session rebind.
    #[must_use]
    pub const fn mcp_quick_timeout(&self) -> Duration {
        self.mcp_quick
    }

    /// Per MCP indexing exchange.
    #[must_use]
    pub const fn mcp_long_timeout(&self) -> Duration {
        self.mcp_long
    }

    fn values(self) -> [(FieldSpec, Duration); 3] {
        [
            (IPC_IO, self.ipc_io),
            (MCP_QUICK, self.mcp_quick),
            (MCP_LONG, self.mcp_long),
        ]
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "TOML timeout keys include their unit"
)]
pub(crate) struct ClientPatch {
    ipc_io_timeout_ms: Option<u64>,
    mcp_quick_timeout_ms: Option<u64>,
    mcp_long_timeout_ms: Option<u64>,
}

pub(crate) struct ClientSpec;

impl ClientSpec {
    pub(crate) const DEFAULTS: ClientSettings = ClientSettings {
        ipc_io: Duration::from_secs(2),
        mcp_quick: Duration::from_secs(30),
        mcp_long: Duration::from_mins(10),
    };

    pub(crate) const fn blocking_ipc_timeout_capability() -> &'static str {
        if cfg!(windows) {
            "windows_named_pipe_io_and_busy_connect"
        } else {
            "unix_socket_io"
        }
    }

    pub(crate) fn is_mcp_only_field(key: &str) -> bool {
        key == MCP_QUICK.key || key == MCP_LONG.key
    }

    pub(crate) fn view_fields(settings: ClientSettings, view: &mut ViewBuilder<'_>) -> Result<()> {
        for (field, timeout) in settings.values() {
            view.push(field.key, serde_json::json!(timeout.as_millis()), None)?;
        }
        Ok(())
    }
}

impl SettingsSpec for ClientSpec {
    type Patch = ClientPatch;
    type Settings = ClientSettings;
    type Defaults = ClientSettings;

    const SCOPE: Scope = Scope::Machine;
    const FIELDS: &'static [FieldSpec] = &[IPC_IO, MCP_QUICK, MCP_LONG];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        Self::DEFAULTS
    }

    fn initial(defaults: &Self::Defaults) -> Self::Settings {
        *defaults
    }

    fn overlay(
        settings: &mut Self::Settings,
        patch: Self::Patch,
        _: &Self::Defaults,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        let next = ClientSettings {
            ipc_io: patch
                .ipc_io_timeout_ms
                .map_or(settings.ipc_io, Duration::from_millis),
            mcp_quick: patch
                .mcp_quick_timeout_ms
                .map_or(settings.mcp_quick, Duration::from_millis),
            mcp_long: patch
                .mcp_long_timeout_ms
                .map_or(settings.mcp_long, Duration::from_millis),
        };
        Self::validate(&next, &Capabilities::current())?;
        for (field, value) in [
            (IPC_IO, patch.ipc_io_timeout_ms),
            (MCP_QUICK, patch.mcp_quick_timeout_ms),
            (MCP_LONG, patch.mcp_long_timeout_ms),
        ] {
            if value.is_some() {
                origins.set(field.key, source)?;
            }
        }
        *settings = next;
        Ok(())
    }

    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        for (field, timeout) in settings.values() {
            if timeout.is_zero() || timeout > MAX_TIMEOUT {
                return Err(Error::Admission {
                    message: format!(
                        "{} must be between 1 and {} milliseconds",
                        field.key,
                        MAX_TIMEOUT.as_millis()
                    ),
                });
            }
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
