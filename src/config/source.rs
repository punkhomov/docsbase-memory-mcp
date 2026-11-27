//! Bounded configuration documents and pure versioned source parsing.
use std::fmt;
use std::path::Path;

use super::schema::{Resolved, SourceId};
use super::{Config, GlobalFileConfig, ProjectFileConfig};
use crate::error::{Error, Result};
use crate::hybrid::config::{HybridPatch, merge::HybridMergePatch};

/// Captures the same bounded file that an operational config load would read.
pub(crate) fn read_edit_document(path: &Path) -> Result<Option<toml::Value>> {
    let Some(text) = super::read_capped_if_exists(path)? else {
        return Ok(None);
    };
    let value = toml::from_str(&text).map_err(|error| {
        if super::resolve::has_version_header(text.as_bytes()) {
            return invalid("invalid versioned configuration TOML");
        }
        Error::internal_with_source(format!("parse {}", path.display()), error)
    })?;
    Ok(Some(value))
}

/// Serializes a preview or write candidate within the operational read limit.
pub(crate) fn render_edit_document(document: &toml::Value) -> Result<String> {
    let rendered = toml::to_string(document)
        .map_err(|error| Error::internal_with_source("render config", error))?;
    let size = u64::try_from(rendered.len())
        .map_err(|_| invalid("rendered configuration size overflow"))?;
    if size > crate::limits::MAX_STATE_BYTES {
        return Err(invalid(
            "rendered configuration exceeds the trusted byte limit",
        ));
    }
    Ok(rendered)
}

/// File-local configuration merge semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigVersion {
    /// Legacy whole-value hybrid replacement.
    V1,
    /// Field-wise hybrid merge and one global vector-store budget.
    V2,
}
/// Initial source for CLI editors creating a global configuration.
pub(crate) fn new_global_document() -> toml::Value {
    toml::Value::Table(toml::Table::from_iter([(
        "config_version".to_owned(),
        toml::Value::Integer(2),
    )]))
}

/// Supplied source text, parsed without touching paths or credentials.
#[derive(Clone)]
pub struct SourceLayer {
    pub(super) source: SourceId,
    pub(super) version: ConfigVersion,
    pub(super) base: GlobalFileConfig,
    pub(super) hybrid_merge: Option<HybridMergePatch>,
    pub(super) vector_store: Option<Resolved<crate::vector_cache::settings::VectorStoreSettings>>,
    pub(super) original: String,
    pub(super) document: toml::Table,
    pub(super) watch: Option<crate::watch::settings::WatchPatch>,
    pub(super) lexical: Option<crate::index::lexical_settings::LexicalPatch>,
    pub(super) store: Option<crate::store::settings::DbFilePatch>,
    pub(super) read_pool: Option<crate::store::settings::ReadPoolPatch>,
    pub(super) dense: Option<crate::hybrid::dense_settings::DenseFilePatch>,
    pub(super) index_writer: Option<crate::index::writer_settings::WriterPatch>,
    pub(super) provisioning: Option<crate::hybrid::provisioning::ProvisioningPatch>,
    pub(super) rrf: Option<crate::hybrid::rrf::settings::RrfPatch>,
    pub(super) retrieval: Option<crate::search::retrieval::RetrievalPatch>,
}
impl fmt::Debug for SourceLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceLayer")
            .field("source", &self.source)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}
impl SourceLayer {
    /// Parses one bounded global/project source. Diagnostics do not echo its text.
    ///
    /// # Errors
    /// Unknown keys/version, invalid shape, forbidden scope or excessive source size.
    pub fn parse(source: SourceId, text: &str) -> Result<Self> {
        if u64::try_from(text.len()).map_err(|_| invalid("configuration source size overflow"))?
            > crate::limits::MAX_STATE_BYTES
        {
            return Err(invalid("configuration source exceeds the state byte limit"));
        }
        let document =
            toml::from_str(text).map_err(|_| invalid("cannot parse configuration TOML"))?;
        Self::from_document(source, text, document)
    }

    // The caller already parsed one byte-bounded capture.
    pub(super) fn from_document(
        source: SourceId,
        text: &str,
        document: toml::Table,
    ) -> Result<Self> {
        if !matches!(source, SourceId::Global(_) | SourceId::Project(_)) {
            return Err(invalid("source must identify a global or project file"));
        }
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
        let mut watch = None;
        let mut lexical = None;
        let mut rrf = None;
        let mut retrieval = None;
        let mut store = None;
        let mut read_pool = None;
        let mut dense = None;
        let mut index_writer = None;
        let mut provisioning = None;
        let mut hybrid_merge = None;
        let mut vector_store = None;
        if version == ConfigVersion::V2 {
            if let Some(value) = fields.remove("watch") {
                watch = Some(
                    value
                        .try_into()
                        .map_err(|_| invalid("invalid v2 watch fields"))?,
                );
            }
            if let Some(hybrid) = fields.remove("hybrid") {
                hybrid_merge = Some(
                    hybrid
                        .try_into()
                        .map_err(|_| invalid("invalid v2 hybrid fields"))?,
                );
            }
            if let Some(value) = fields.remove("lexical") {
                lexical = Some(
                    value
                        .try_into()
                        .map_err(|_| invalid("invalid v2 lexical fields"))?,
                );
            }
            rrf = take_project_table(&mut fields, "rrf")?;
            retrieval = take_project_table(&mut fields, "retrieval")?;
            provisioning = take_global_table(&mut fields, &source, "provisioning")?;
            store = take_global_table(&mut fields, &source, "store")?;
            read_pool = take_global_table(&mut fields, &source, "read_pool")?;
            dense = take_global_table(&mut fields, &source, "dense")?;
            index_writer = take_index_writer(&mut fields, &source)?;
            let vector = fields.remove("vector_cache");
            if matches!(source, SourceId::Global(_)) {
                vector_store = Some(crate::vector_cache::settings::resolve_file(
                    &source, vector,
                )?);
            } else if vector.is_some() {
                return Err(invalid("vector_cache is global-only"));
            }
        }
        if matches!(source, SourceId::Project(_)) && fields.contains_key("providers") {
            return Err(invalid("[providers] is only allowed in the global config"));
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
            vector_store,
            original: text.to_owned(),
            document,
            watch,
            lexical,
            rrf,
            retrieval,
            provisioning,
            store,
            read_pool,
            dense,
            index_writer,
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
fn take_project_table<P: serde::de::DeserializeOwned>(
    fields: &mut toml::Table,
    key: &str,
) -> Result<Option<P>> {
    fields
        .remove(key)
        .map(|value| decode_v2_table(value, key))
        .transpose()
}

fn take_global_table<P: serde::de::DeserializeOwned>(
    fields: &mut toml::Table,
    source: &SourceId,
    key: &str,
) -> Result<Option<P>> {
    fields
        .remove(key)
        .map(|value| global_table(value, source, key))
        .transpose()
}

fn take_index_writer(
    fields: &mut toml::Table,
    source: &SourceId,
) -> Result<Option<crate::index::writer_settings::WriterPatch>> {
    let Some(index) = fields.get_mut("index").and_then(toml::Value::as_table_mut) else {
        return Ok(None);
    };
    let Some(writer) = index.remove("writer") else {
        return Ok(None);
    };
    if index.is_empty() {
        fields.remove("index");
    }
    global_table(writer, source, "index.writer").map(Some)
}

fn global_table<P: serde::de::DeserializeOwned>(
    value: toml::Value,
    source: &SourceId,
    key: &str,
) -> Result<P> {
    if matches!(source, SourceId::Project(_)) {
        return Err(invalid(&format!("{key} is global-only")));
    }
    decode_v2_table(value, key)
}

fn decode_v2_table<P: serde::de::DeserializeOwned>(value: toml::Value, key: &str) -> Result<P> {
    if !value.is_table() {
        return Err(invalid(&format!("v2 {key} settings require a table")));
    }
    value
        .try_into()
        .map_err(|_| invalid(&format!("invalid v2 {key} fields")))
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
    /// Effective values with typed shared vector policy for a v2 global.
    /// V1 retains the project-scoped `hybrid.cache.max_bytes` key.
    pub effective: Resolved<Config>,
    /// Effective legacy budget before strict-global projection.
    pub legacy_vector_max_bytes: u64,
    /// Scope conflicts that eventual migration/admission must resolve.
    pub conflicts: Vec<PreviewConflict>,
    /// False because supplied previews confer no authority to activate a migration.
    pub runtime_activation_available: bool,
}
pub(super) fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
