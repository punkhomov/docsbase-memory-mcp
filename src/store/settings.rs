//! Component-owned storage settings and explicit compatibility resolution.

use crate::config::Config;
use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Resolved, Scope,
    SettingsSpec, SourceId, Unit,
};
use crate::error::{Error, Result};
use std::time::{Duration, Instant};

/// Resolves the deterministic legacy/bootstrap policy through its sole owner.
/// Runtime consumers receive the result explicitly; they never fill fields.
///
/// # Errors
/// Returns an admission error if the owner defaults are invalid.
pub fn standalone() -> Result<Resolved<DbSettings>> {
    crate::config::resolve::resolve_component::<DbSpec>(&DefaultsContext::standalone(), [])
}

/// Resolves the standalone reader policy at an explicit composition boundary.
///
/// # Errors
/// Returns an admission error if owner defaults are invalid.
pub fn standalone_read_pool() -> Result<Resolved<ReadPoolSettings>> {
    crate::config::resolve::resolve_component::<ReadPoolSpec>(&DefaultsContext::standalone(), [])
}

pub(crate) fn db_from_config(config: &Resolved<Config>) -> Result<Resolved<DbSettings>> {
    Ok(Resolved::new(
        config.settings().store.clone(),
        component_origins(config, DbSpec::FIELDS)?,
    ))
}

pub(crate) fn pool_from_config(config: &Resolved<Config>) -> Result<Resolved<ReadPoolSettings>> {
    Ok(Resolved::new(
        config.settings().read_pool.clone(),
        component_origins(config, ReadPoolSpec::FIELDS)?,
    ))
}

fn component_origins(config: &Resolved<Config>, fields: &[FieldSpec]) -> Result<Provenance> {
    let mut origins = Provenance::initial(fields, &SourceId::Defaults);
    for field in fields {
        let origin = config
            .provenance()
            .get(field.key)
            .ok_or_else(|| Error::internal(format!("missing effective field {}", field.key)))?;
        origins.set(field.key, &origin.source)?;
    }
    Ok(origins)
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
/// Explicit typed overrides for the metadata DB.
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
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DbFilePatch {
    write_busy_timeout: Option<u64>,
    read_busy_timeout: Option<u64>,
    wal_attempts: Option<u32>,
    wal_retry_delay: Option<u64>,
}

impl From<DbFilePatch> for DbPatch {
    fn from(patch: DbFilePatch) -> Self {
        Self {
            write_busy_timeout: patch.write_busy_timeout.map(Duration::from_millis),
            read_busy_timeout: patch.read_busy_timeout.map(Duration::from_millis),
            wal_attempts: patch.wal_attempts,
            wal_retry_delay: patch.wal_retry_delay.map(Duration::from_millis),
        }
    }
}

impl DbSettings {
    pub(crate) fn view_fields(
        &self,
        view: &mut crate::config::view::ViewBuilder<'_>,
    ) -> Result<()> {
        for (key, value) in [
            ("store.write_busy_timeout", self.write_busy_timeout),
            ("store.read_busy_timeout", self.read_busy_timeout),
            ("store.wal_retry_delay", self.wal_retry_delay),
        ] {
            let millis = u64::try_from(value.as_millis())
                .map_err(|_| Error::internal("cannot display DB setting milliseconds"))?;
            view.push(key, millis.into(), None)?;
        }
        view.push("store.wal_attempts", self.wal_attempts.into(), None)
    }
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
        if settings.wal_retry_delay.as_millis() > u128::from(u64::MAX)
            || settings
                .wal_retry_delay
                .checked_mul(settings.wal_attempts - 1)
                .and_then(|delay| Instant::now().checked_add(delay))
                .is_none()
        {
            return Err(Error::Admission {
                message: "store WAL retry duration exceeds the platform clock".to_owned(),
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

/// Complete prepared metadata reader policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPoolSettings {
    /// Maximum prepared connections in the #66 bounded read pool.
    pub max_connections: usize,
}
/// Explicit overrides for the prepared metadata reader capacity.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadPoolPatch {
    /// Maximum prepared connections in the #66 bounded read pool.
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
        if std::alloc::Layout::array::<std::sync::Mutex<super::Db>>(settings.max_connections)
            .is_err()
        {
            return Err(Error::Admission {
                message: "read_pool.max_connections exceeds the allocation range".to_owned(),
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

impl DbSpec {
    /// Assess connection policy only, without opening/migrating any store.
    /// Old policy is caller-supplied; this is not a live daemon observation.
    ///
    /// # Errors
    /// Invalid old/new settings or policy encoding.
    pub fn assess_change(
        old: Option<&DbSettings>,
        next: &DbSettings,
    ) -> Result<crate::config::change::Assessment> {
        use crate::config::change::{policy_identity, runtime_assessment};
        let identity = |value: &DbSettings| {
            Self::validate(value, &Capabilities::current())?;
            // Duration serde uses integer seconds+nanos, preserving sub-ms input.
            policy_identity(
                "metadata-db-policy-v1",
                &(
                    value.write_busy_timeout,
                    value.read_busy_timeout,
                    value.wal_attempts,
                    value.wal_retry_delay,
                ),
            )
        };
        Ok(runtime_assessment(
            "metadata_db",
            old.map(identity).transpose()?,
            identity(next)?,
            Self::impact(old, next, &ArtifactFacts::default()),
        ))
    }

    /// Assess header schema observations, never registry rows or query readiness.
    ///
    /// # Errors
    /// Multiple contradictory metadata schema observations.
    pub fn assess_schema(facts: &ArtifactFacts) -> Result<crate::config::change::Assessment> {
        super::plan::assess_schema(facts)
    }
}
