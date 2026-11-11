//! Web-owned policy; no search/index defaults live here.
use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Resolved, Scope,
    SettingsSpec, SourceId, Unit,
};
use crate::error::{Error, Result};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSettings {
    pub port: u16,
    pub search_connections: usize,
    pub control_connections: usize,
    pub queue_limit: usize,
    pub max_body_bytes: usize,
    pub control_timeout_ms: u64,
    pub startup_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub poll_interval_ms: u64,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebPatch {
    pub port: Option<u16>,
    pub search_connections: Option<usize>,
    pub control_connections: Option<usize>,
    pub queue_limit: Option<usize>,
    pub max_body_bytes: Option<usize>,
    pub control_timeout_ms: Option<u64>,
    pub startup_timeout_ms: Option<u64>,
    pub shutdown_timeout_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
}
pub struct WebSpec;
impl SettingsSpec for WebSpec {
    type Settings = WebSettings;
    type Patch = WebPatch;
    type Defaults = WebSettings;
    const SCOPE: Scope = Scope::Machine;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("web.port", Scope::Machine, Unit::Count),
        FieldSpec::new("web.search_connections", Scope::Machine, Unit::Count),
        FieldSpec::new("web.control_connections", Scope::Machine, Unit::Count),
        FieldSpec::new("web.queue_limit", Scope::Machine, Unit::Count),
        FieldSpec::new("web.max_body_bytes", Scope::Machine, Unit::Bytes),
        FieldSpec::new("web.control_timeout_ms", Scope::Machine, Unit::Milliseconds),
        FieldSpec::new("web.startup_timeout_ms", Scope::Machine, Unit::Milliseconds),
        FieldSpec::new(
            "web.shutdown_timeout_ms",
            Scope::Machine,
            Unit::Milliseconds,
        ),
        FieldSpec::new("web.poll_interval_ms", Scope::Machine, Unit::Milliseconds),
    ];
    fn defaults(_: &DefaultsContext) -> WebSettings {
        WebSettings {
            port: 0,
            search_connections: 4,
            control_connections: 2,
            queue_limit: 16,
            max_body_bytes: 64 * 1024,
            control_timeout_ms: 5000,
            startup_timeout_ms: 15000,
            shutdown_timeout_ms: 3000,
            poll_interval_ms: 5000,
        }
    }
    fn initial(defaults: &WebSettings) -> WebSettings {
        defaults.clone()
    }
    fn overlay(
        s: &mut WebSettings,
        p: WebPatch,
        _: &WebSettings,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        macro_rules! apply { ($($field:ident),*) => { $(if let Some(value) = p.$field { s.$field = value; origins.set(concat!("web.", stringify!($field)), source)?; })* }; }
        apply!(
            port,
            search_connections,
            control_connections,
            queue_limit,
            max_body_bytes,
            control_timeout_ms,
            startup_timeout_ms,
            shutdown_timeout_ms,
            poll_interval_ms
        );
        Ok(())
    }
    fn validate(s: &WebSettings, _: &Capabilities) -> Result<()> {
        if s.search_connections == 0
            || s.control_connections == 0
            || s.search_connections.saturating_add(s.control_connections) > 16
            || s.queue_limit > 1024
            || s.max_body_bytes == 0
            || s.max_body_bytes > crate::ipc::protocol::MAX_FRAME_BYTES
            || s.control_timeout_ms == 0
            || s.startup_timeout_ms == 0
            || s.shutdown_timeout_ms == 0
            || s.poll_interval_ms == 0
            // Browser setTimeout accepts a signed 32-bit millisecond delay.
            || s.poll_interval_ms > i32::MAX as u64
        {
            return Err(Error::Admission { message: "invalid web policy: nonzero capacities/timeouts required; pool <=16, queue <=1024, body <= IPC frame".into() });
        }
        Ok(())
    }
    fn impact(old: Option<&WebSettings>, next: &WebSettings, _: &ArtifactFacts) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
/// Resolve global Web policy and CLI override once at startup.
/// # Errors
/// File/parse errors or invalid policy.
pub fn load(port: Option<u16>) -> Result<Resolved<WebSettings>> {
    let path = crate::config::paths::config_dir()?.join("web.toml");
    let mut layers = Vec::new();
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::internal_with_source("read web.toml", e))?;
        let overrides = toml::from_str(&text).map_err(|_| Error::Admission {
            message: "invalid web.toml".into(),
        })?;
        layers.push((SourceId::Global(path), overrides));
    }
    layers.push((
        SourceId::Cli,
        WebPatch {
            port,
            ..WebPatch::default()
        },
    ));
    crate::config::resolve::resolve_component::<WebSpec>(&DefaultsContext::standalone(), layers)
}
