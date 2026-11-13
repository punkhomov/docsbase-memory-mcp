//! Pure policy owner for the shared vector store. File-v2 activation is separate.

use std::time::Duration;

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Resolved, Scope,
    SettingsSpec, SourceId, Unit,
};
use crate::error::{Error, Result};

pub(crate) const MAX_BYTES_FIELD: FieldSpec =
    FieldSpec::new("vector_cache.max_bytes", Scope::Daemon, Unit::Bytes);

/// Immutable policy shared by every connection belonging to one vector store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorStoreSettings {
    /// Maximum retained vectors, across models and projects.
    pub max_entries: u64,
    /// Maximum retained vector payload bytes; active leases can exceed this cap.
    pub max_bytes: u64,
    /// SQLite writer wait; zero means immediate failure on contention.
    pub busy_timeout: Duration,
}

impl VectorStoreSettings {
    /// Admit a complete strict policy before acquiring files or opening SQLite.
    ///
    /// # Errors
    /// Nonpositive capacities or a timeout outside SQLite's signed milliseconds.
    pub fn validate(&self) -> Result<()> {
        if self.max_entries == 0 || self.max_bytes == 0 {
            return Err(Error::Admission {
                message: "vector-store entry and byte limits must be positive".into(),
            });
        }
        // rusqlite otherwise panics while converting this value for SQLite.
        if self.busy_timeout.as_millis() > i32::MAX as u128 {
            return Err(Error::Admission {
                message: "vector_cache.busy_timeout exceeds SQLite milliseconds".into(),
            });
        }
        Ok(())
    }

    pub(crate) fn owner_defaults() -> Self {
        VectorStoreSpec::defaults(&DefaultsContext::standalone())
    }

    pub(crate) const fn limits(&self) -> super::CacheLimits {
        super::CacheLimits {
            max_entries: self.max_entries,
            max_bytes: self.max_bytes,
        }
    }
}

/// Typed construction overrides; these are not new TOML keys.
#[derive(Debug, Clone, Default)]
pub struct VectorStorePatch {
    /// Maximum retained vectors.
    pub max_entries: Option<u64>,
    /// Maximum vector payload bytes.
    pub max_bytes: Option<u64>,
    /// Writer wait on every owned connection.
    pub busy_timeout: Option<Duration>,
}

/// Sole owner of strict vector-store defaults, scope and validation.
pub struct VectorStoreSpec;
impl SettingsSpec for VectorStoreSpec {
    type Patch = VectorStorePatch;
    type Settings = VectorStoreSettings;
    type Defaults = VectorStoreSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("vector_cache.max_entries", Scope::Daemon, Unit::Count),
        MAX_BYTES_FIELD,
        FieldSpec::new(
            "vector_cache.busy_timeout",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
    ];

    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        VectorStoreSettings {
            max_entries: 1_000_000,
            // 75k x 2048 f32 is ~586 MiB; retain the target ANN corpus.
            max_bytes: 1024 * 1024 * 1024,
            busy_timeout: Duration::from_secs(5),
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
        if let Some(value) = patch.max_entries {
            settings.max_entries = value;
            origins.set("vector_cache.max_entries", source)?;
        }
        if let Some(value) = patch.max_bytes {
            settings.max_bytes = value;
            origins.set("vector_cache.max_bytes", source)?;
        }
        if let Some(value) = patch.busy_timeout {
            settings.busy_timeout = value;
            origins.set("vector_cache.busy_timeout", source)?;
        }
        // Resolution stages both values and origins; a rejected patch rolls back.
        settings.validate()
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        settings.validate()
    }
    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}

/// Resolve owner defaults at an explicit standalone composition boundary.
///
/// # Errors
/// Invalid owner defaults.
pub fn standalone() -> Result<Resolved<VectorStoreSettings>> {
    crate::config::resolve::resolve_component::<VectorStoreSpec>(&DefaultsContext::standalone(), [])
}
