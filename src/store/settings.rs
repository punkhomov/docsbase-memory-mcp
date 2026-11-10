//! Component-owned storage settings and explicit compatibility resolution.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Resolved, Scope,
    SettingsSpec, SourceId, Unit,
};
use crate::error::{Error, Result};
use std::time::Duration;

/// Resolves the deterministic legacy/bootstrap policy through its sole owner.
/// Runtime consumers receive the result explicitly; they never fill fields.
///
/// # Errors
/// Returns an admission error if the owner defaults are invalid.
pub fn standalone() -> Result<Resolved<DbSettings>> {
    crate::config::resolve::resolve_component::<DbSpec>(&DefaultsContext::standalone(), [])
}

/// Complete metadata DB policy, passed explicitly to connection constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSettings {
    /// Busy timeout for write-capable connections; zero means immediate failure.
    pub write_busy_timeout: Duration,
    /// Busy timeout for operational read-only connections.
    pub read_busy_timeout: Duration,
    /// Number of WAL-mode negotiation attempts.
    pub wal_attempts: u32,
    /// Delay between WAL negotiation attempts; zero is permitted.
    pub wal_retry_delay: Duration,
}
/// Explicit typed overrides; these are not accepted TOML keys yet.
#[derive(Debug, Clone, Default)]
pub struct DbPatch {
    /// Busy timeout for write-capable connections; zero means immediate failure.
    pub write_busy_timeout: Option<Duration>,
    /// Busy timeout for operational read-only connections.
    pub read_busy_timeout: Option<Duration>,
    /// Number of WAL-mode negotiation attempts.
    pub wal_attempts: Option<u32>,
    /// Delay between WAL negotiation attempts; zero is permitted.
    pub wal_retry_delay: Option<Duration>,
}
/// Component owner of defaults, scope and admission.
pub struct DbSpec;
impl SettingsSpec for DbSpec {
    type Patch = DbPatch;
    type Settings = DbSettings;
    type Defaults = DbSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new(
            "store.write_busy_timeout",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
        FieldSpec::new("store.read_busy_timeout", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("store.wal_attempts", Scope::Daemon, Unit::Count),
        FieldSpec::new("store.wal_retry_delay", Scope::Daemon, Unit::Milliseconds),
    ];
    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        DbSettings {
            write_busy_timeout: Duration::from_secs(30),
            read_busy_timeout: Duration::from_secs(5),
            wal_attempts: 3,
            wal_retry_delay: Duration::from_millis(100),
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
        if let Some(value) = patch.write_busy_timeout {
            settings.write_busy_timeout = value;
            origins.set("store.write_busy_timeout", source)?;
        }
        if let Some(value) = patch.read_busy_timeout {
            settings.read_busy_timeout = value;
            origins.set("store.read_busy_timeout", source)?;
        }
        if let Some(value) = patch.wal_attempts {
            settings.wal_attempts = value;
            origins.set("store.wal_attempts", source)?;
        }
        if let Some(value) = patch.wal_retry_delay {
            settings.wal_retry_delay = value;
            origins.set("store.wal_retry_delay", source)?;
        }
        Ok(())
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        // rusqlite converts milliseconds to SQLite's signed integer and
        // panics on overflow. Reject before a future connection is opened.
        for (key, timeout) in [
            ("store.write_busy_timeout", settings.write_busy_timeout),
            ("store.read_busy_timeout", settings.read_busy_timeout),
        ] {
            if timeout.as_millis() > i32::MAX as u128 {
                return Err(Error::Admission {
                    message: format!("{key} exceeds SQLite busy_timeout milliseconds"),
                });
            }
        }
        if settings.wal_attempts == 0 {
            return Err(Error::Admission {
                message: "store.wal_attempts must be > 0".to_owned(),
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

/// Complete readpool policy; runtime adoption follows in the owning task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPoolSettings {
    /// Maximum connections in the future #66 bounded read pool.
    pub max_connections: usize,
}
/// Explicit typed overrides; these are not accepted TOML keys yet.
#[derive(Debug, Clone, Default)]
pub struct ReadPoolPatch {
    /// Maximum connections in the future #66 bounded read pool.
    pub max_connections: Option<usize>,
}
/// Component owner of defaults, scope and admission.
pub struct ReadPoolSpec;
impl SettingsSpec for ReadPoolSpec {
    type Patch = ReadPoolPatch;
    type Settings = ReadPoolSettings;
    type Defaults = ReadPoolSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[FieldSpec::new(
        "read_pool.max_connections",
        Scope::Daemon,
        Unit::Count,
    )];
    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        ReadPoolSettings { max_connections: 4 }
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
        if let Some(value) = patch.max_connections {
            settings.max_connections = value;
            origins.set("read_pool.max_connections", source)?;
        }
        Ok(())
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        if settings.max_connections == 0 {
            return Err(Error::Admission {
                message: "read_pool.max_connections must be > 0".to_owned(),
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
