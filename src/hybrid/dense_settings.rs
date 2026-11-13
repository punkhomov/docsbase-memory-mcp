//! Pure dense policy, ungated. Query-cache construction consumes its projection;
//! remaining dense resource adoption belongs to the later CF6 slices.
//! The legacy vector budget is per dense path/project, not a strict global quota.

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::error::{Error, Result};
use std::time::Duration;

/// Default query entry limit, owned here and shared by compatibility adapters.
pub const DEFAULT_QUERY_CACHE_MAX_ENTRIES: usize = 256;
/// Default cached query vector payload bytes (allocator/key overhead excluded).
pub const DEFAULT_QUERY_CACHE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Complete immutable policy for one shared query-cache resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryCacheSettings {
    /// Maximum retained query vectors.
    pub max_entries: usize,
    /// Maximum retained vector payload bytes, excluding external Arc holders.
    pub max_bytes: u64,
}
impl QueryCacheSettings {
    /// Check explicit policy before constructing resources.
    ///
    /// # Errors
    /// Either capacity is zero.
    pub fn validate(&self) -> Result<()> {
        if self.max_entries == 0 || self.max_bytes == 0 {
            return Err(Error::Admission {
                message: "query-cache entry and byte limits must be positive".into(),
            });
        }
        Ok(())
    }
    pub(crate) fn owner_defaults() -> Self {
        DenseSpec::defaults(&DefaultsContext::standalone()).query_cache_settings()
    }
}
/// Resolve defaults at a standalone compatibility/composition boundary.
///
/// # Errors
/// The owner's policy is invalid.
pub fn standalone_query_cache() -> Result<QueryCacheSettings> {
    let dense =
        crate::config::resolve::resolve_component::<DenseSpec>(&DefaultsContext::standalone(), [])?;
    Ok(dense.settings().query_cache_settings())
}

/// Complete dense policy; remaining runtime adoption follows in its owning task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenseSettings {
    /// Maximum vectors per project before exact streaming.
    pub max_vectors: usize,
    /// Resident entries plus every live build peak, daemon-wide.
    pub max_bytes: u64,
    /// Maximum resident projects.
    pub max_projects: usize,
    /// Minimum vectors for ANN consideration.
    pub min_vectors: usize,
    /// Retry backoff after a failed build; zero is permitted.
    pub build_backoff: Duration,
    /// Legacy vector-cache budget used by the dense path; not strict-v2 scope.
    pub vector_cache_max_bytes: u64,
    /// ANN dump budget; zero removes all eligible dumps during cleanup.
    pub max_ann_disk_bytes: u64,
    /// Maximum cached query vectors.
    pub query_cache_max_entries: usize,
    /// Cached query vector bytes.
    pub query_cache_max_bytes: u64,
}
impl DenseSettings {
    /// Project the query resource's settings; checked consumers validate imports.
    #[must_use]
    pub const fn query_cache_settings(&self) -> QueryCacheSettings {
        QueryCacheSettings {
            max_entries: self.query_cache_max_entries,
            max_bytes: self.query_cache_max_bytes,
        }
    }
}
/// Explicit typed overrides; these are not accepted TOML keys yet.
#[derive(Debug, Clone, Default)]
pub struct DensePatch {
    /// Maximum vectors per project before exact streaming.
    pub max_vectors: Option<usize>,
    /// Resident entries plus every live build peak, daemon-wide.
    pub max_bytes: Option<u64>,
    /// Maximum resident projects.
    pub max_projects: Option<usize>,
    /// Minimum vectors for ANN consideration.
    pub min_vectors: Option<usize>,
    /// Retry backoff after a failed build; zero is permitted.
    pub build_backoff: Option<Duration>,
    /// Legacy vector-cache budget used by the dense path; not strict-v2 scope.
    pub vector_cache_max_bytes: Option<u64>,
    /// ANN dump budget; zero removes all eligible dumps during cleanup.
    pub max_ann_disk_bytes: Option<u64>,
    /// Maximum cached query vectors.
    pub query_cache_max_entries: Option<usize>,
    /// Cached query vector bytes.
    pub query_cache_max_bytes: Option<u64>,
}
/// Component owner of defaults, scope and admission.
pub struct DenseSpec;
impl SettingsSpec for DenseSpec {
    type Patch = DensePatch;
    type Settings = DenseSettings;
    type Defaults = DenseSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("dense.max_vectors", Scope::Daemon, Unit::Count),
        FieldSpec::new("dense.max_bytes", Scope::Daemon, Unit::Bytes),
        FieldSpec::new("dense.max_projects", Scope::Daemon, Unit::Count),
        FieldSpec::new("dense.min_vectors", Scope::Daemon, Unit::Count),
        FieldSpec::new("dense.build_backoff", Scope::Daemon, Unit::Milliseconds),
        FieldSpec::new("dense.vector_cache_max_bytes", Scope::Daemon, Unit::Bytes),
        FieldSpec::new("dense.max_ann_disk_bytes", Scope::Daemon, Unit::Bytes),
        FieldSpec::new("dense.query_cache_max_entries", Scope::Daemon, Unit::Count),
        FieldSpec::new("dense.query_cache_max_bytes", Scope::Daemon, Unit::Bytes),
    ];
    fn defaults(_: &DefaultsContext) -> Self::Defaults {
        DenseSettings {
            max_vectors: 1_000_000,
            max_bytes: 1536 * 1024 * 1024,
            max_projects: 8,
            min_vectors: 4096,
            build_backoff: Duration::from_secs(30),
            vector_cache_max_bytes: 1024 * 1024 * 1024,
            max_ann_disk_bytes: 4 * 1024 * 1024 * 1024,
            query_cache_max_entries: DEFAULT_QUERY_CACHE_MAX_ENTRIES,
            query_cache_max_bytes: DEFAULT_QUERY_CACHE_MAX_BYTES,
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
        if let Some(value) = patch.max_vectors {
            settings.max_vectors = value;
            origins.set("dense.max_vectors", source)?;
        }
        if let Some(value) = patch.max_bytes {
            settings.max_bytes = value;
            origins.set("dense.max_bytes", source)?;
        }
        if let Some(value) = patch.max_projects {
            settings.max_projects = value;
            origins.set("dense.max_projects", source)?;
        }
        if let Some(value) = patch.min_vectors {
            settings.min_vectors = value;
            origins.set("dense.min_vectors", source)?;
        }
        if let Some(value) = patch.build_backoff {
            settings.build_backoff = value;
            origins.set("dense.build_backoff", source)?;
        }
        if let Some(value) = patch.vector_cache_max_bytes {
            settings.vector_cache_max_bytes = value;
            origins.set("dense.vector_cache_max_bytes", source)?;
        }
        if let Some(value) = patch.max_ann_disk_bytes {
            settings.max_ann_disk_bytes = value;
            origins.set("dense.max_ann_disk_bytes", source)?;
        }
        if let Some(value) = patch.query_cache_max_entries {
            settings.query_cache_max_entries = value;
            origins.set("dense.query_cache_max_entries", source)?;
        }
        if let Some(value) = patch.query_cache_max_bytes {
            settings.query_cache_max_bytes = value;
            origins.set("dense.query_cache_max_bytes", source)?;
        }
        Ok(())
    }
    fn validate(settings: &Self::Settings, _: &Capabilities) -> Result<()> {
        settings.query_cache_settings().validate()
    }
    fn impact(
        old: Option<&Self::Settings>,
        next: &Self::Settings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
