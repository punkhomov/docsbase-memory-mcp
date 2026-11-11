//! Bounded source-only configuration preview. No filesystem or runtime access.
use std::fmt;
use std::path::Path;

use super::schema::{Resolved, SourceId};
use super::{Config, GlobalFileConfig, ProjectFileConfig};
use crate::error::{Error, Result};
use crate::hybrid::config::{HybridPatch, merge::HybridMergePatch};

/// File-local configuration semantics used only by the planning API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigVersion {
    /// Legacy whole-value hybrid replacement.
    V1,
    /// Field-wise hybrid merge and one global vector-store budget.
    V2,
}
/// Supplied source text, parsed without touching paths or credentials.
#[derive(Clone)]
pub struct SourceLayer {
    pub(super) source: SourceId,
    pub(super) version: ConfigVersion,
    pub(super) base: GlobalFileConfig,
    pub(super) hybrid_merge: Option<HybridMergePatch>,
    pub(super) vector_max_bytes: Option<u64>,
    pub(super) original: String,
    pub(super) document: toml::Table,
}
impl fmt::Debug for SourceLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceLayer")
            .field("source", &self.source)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorBudget {
    max_bytes: u64,
}
impl SourceLayer {
    /// Parses one bounded global/project source. Diagnostics do not echo its text.
    ///
    /// # Errors
    /// Unknown keys/version, invalid shape, forbidden scope or excessive source size.
    pub fn parse(source: SourceId, text: &str) -> Result<Self> {
        if !matches!(source, SourceId::Global(_) | SourceId::Project(_)) {
            return Err(invalid("source must identify a global or project file"));
        }
        if u64::try_from(text.len()).map_err(|_| invalid("configuration source size overflow"))?
            > crate::limits::MAX_STATE_BYTES
        {
            return Err(invalid("configuration source exceeds the state byte limit"));
        }
        let document: toml::Table =
            toml::from_str(text).map_err(|_| invalid("invalid configuration TOML"))?;
        let mut fields = document.clone();
        let version = match fields.remove("config_version") {
            None | Some(toml::Value::Integer(1)) => ConfigVersion::V1,
            Some(toml::Value::Integer(2)) => ConfigVersion::V2,
            _ => {
                return Err(invalid(
                    "unsupported configuration version; expected integer 1 or 2",
                ));
            }
        };
        let mut hybrid_merge = None;
        let mut vector_max_bytes = None;
        if version == ConfigVersion::V2 {
            if let Some(hybrid) = fields.remove("hybrid") {
                hybrid_merge = Some(
                    hybrid
                        .try_into()
                        .map_err(|_| invalid("invalid v2 hybrid fields"))?,
                );
            }
            if let Some(vector) = fields.remove("vector_cache") {
                if matches!(source, SourceId::Project(_)) {
                    return Err(invalid("vector_cache is global-only"));
                }
                let budget: VectorBudget = vector
                    .try_into()
                    .map_err(|_| invalid("invalid global vector_cache fields"))?;
                if budget.max_bytes == 0 {
                    return Err(invalid("vector_cache.max_bytes must be positive"));
                }
                vector_max_bytes = Some(budget.max_bytes);
            }
        }
        let base = if matches!(source, SourceId::Global(_)) {
            toml::Value::Table(fields)
                .try_into::<GlobalFileConfig>()
                .map_err(|_| invalid("invalid global configuration fields"))?
        } else {
            let project = toml::Value::Table(fields)
                .try_into::<ProjectFileConfig>()
                .map_err(|_| invalid("invalid project fields or shared-resource override"))?;
            project.into_global()
        };
        Ok(Self {
            source,
            version,
            base,
            hybrid_merge,
            vector_max_bytes,
            original: text.to_owned(),
            document,
        })
    }
    /// Exact source identity supplied by the caller; no canonicalization or I/O.
    #[must_use]
    pub const fn source(&self) -> &SourceId {
        &self.source
    }
    /// Per-file merge version.
    #[must_use]
    pub const fn version(&self) -> ConfigVersion {
        self.version
    }
    /// Path of this already checked file-source variant.
    pub(super) fn path(&self) -> Result<&Path> {
        match &self.source {
            SourceId::Global(path) | SourceId::Project(path) => Ok(path),
            _ => Err(invalid("source has no file identity")),
        }
    }
    pub(super) fn legacy_vector_override(&self) -> Option<u64> {
        match &self.base.hybrid {
            Some(HybridPatch::Table(table)) => table.cache.as_ref().and_then(|v| v.max_bytes),
            _ => None,
        }
    }
}
/// A source-level conflict; preview is never permission to operate a v2 store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewConflict {
    /// A legacy project still specifies the shared cache capacity locally.
    LegacyProjectVectorBudget {
        /// Conflicting project file.
        source: SourceId,
        /// Explicit legacy value.
        max_bytes: u64,
    },
}
/// Fully resolved values for source review, not an operational v2 snapshot.
#[derive(Debug, Clone)]
pub struct SourcePreview {
    /// Effective values in the compatibility Config layout. For a v2 global,
    /// budget provenance uses `vector_cache.max_bytes` with daemon scope instead
    /// of the legacy project-scoped `hybrid.cache.max_bytes` key.
    pub effective: Resolved<Config>,
    /// Effective legacy budget before strict-global projection.
    pub legacy_vector_max_bytes: u64,
    /// Scope conflicts that eventual migration/admission must resolve.
    pub conflicts: Vec<PreviewConflict>,
    /// False until runtime consent and strict-store adoption are implemented.
    pub runtime_activation_available: bool,
}
pub(super) fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
