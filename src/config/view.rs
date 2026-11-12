//! Pure, versioned effective settings views. No provisioning or runtime observation.
use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use super::schema::{FieldOrigin, Provenance, Resolved};
use super::{Config, ConfigSpec};
use crate::error::{Error, Result};
use crate::hybrid::provider::{self, Provider};

/// Compiled support from the existing provider registry, not model readiness.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderCapability {
    /// Existing registry identity.
    pub provider: &'static str,
    /// Feature needed to execute this provider, if any.
    pub required_feature: Option<&'static str>,
    /// Whether this binary contains that implementation.
    pub compiled: bool,
}
impl ProviderCapability {
    fn from_provider(provider: &dyn Provider) -> Self {
        Self {
            provider: provider.name(),
            required_feature: provider.feature(),
            compiled: provider.enabled(),
        }
    }
}
/// One actual resolved value with owner-supplied units and provenance.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveField {
    /// Stable owner key.
    pub key: &'static str,
    /// Scalar/list/record, or null for a semantically absent stage field.
    pub value: Value,
    /// Original source, scope and unit, never inferred from current files.
    #[serde(flatten)]
    pub origin: FieldOrigin,
    /// Provider needed by this field's stage, not a provisioning probe.
    pub capability: Option<ProviderCapability>,
}
/// Observation basis, kept distinct from a live daemon snapshot.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewBasis {
    /// Caller-supplied immutable settings/provenance.
    ResolvedSnapshot,
    /// Fresh v1 source resolution from explicit paths.
    SourceResolution,
}
/// Read-only presentation contract, independent of the legacy CLI JSON.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveView {
    /// Presentation schema version, not a configuration or storage version.
    pub view_version: u32,
    /// Where the displayed settings came from.
    pub basis: ViewBasis,
    /// Current coverage: file settings, not every future component owner.
    pub coverage: &'static str,
    /// False: this view does not contact a running daemon.
    pub live_runtime_observed: bool,
    /// False: installed runtimes, models and credentials were not probed.
    pub provisioning_checked: bool,
    /// All fields in stable key order.
    pub fields: Vec<EffectiveField>,
    /// Existing registry capabilities, including feature-disabled providers.
    pub provider_capabilities: Vec<ProviderCapability>,
}

/// Displays already resolved values without consulting files or environment.
///
/// # Errors
/// Invalid owner projection or incomplete/ambiguous field provenance.
pub fn from_resolved(snapshot: &Resolved<Config>) -> Result<EffectiveView> {
    let mut builder = ViewBuilder::new(snapshot.provenance());
    ConfigSpec::view_fields(snapshot.settings(), &mut builder)?;
    let mut provider_capabilities: Vec<_> = provider::providers()
        .iter()
        .map(|entry| ProviderCapability::from_provider(*entry))
        .collect();
    provider_capabilities.sort_by_key(|entry| entry.provider);
    Ok(EffectiveView {
        view_version: 1,
        basis: ViewBasis::ResolvedSnapshot,
        coverage: "file_settings",
        live_runtime_observed: false,
        provisioning_checked: false,
        fields: builder.finish()?,
        provider_capabilities,
    })
}

/// Resolves the same v1 sources as the runtime loader and displays their origins.
/// Does not create directories, open databases or construct/probe providers.
/// Errors deliberately omit source text and the legacy parser's error chain.
///
/// # Errors
/// Unreadable/invalid v1 sources or an invalid effective view.
pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<EffectiveView> {
    let snapshot = super::resolve::load_from(config_dir, project_root)
        .map_err(|_| invalid("cannot resolve v1 configuration for effective view"))?;
    let mut view = from_resolved(&snapshot)
        .map_err(|_| invalid("cannot describe resolved configuration for effective view"))?;
    view.basis = ViewBasis::SourceResolution;
    Ok(view)
}

pub(crate) fn capability(name: &str) -> Result<ProviderCapability> {
    provider::provider(name)
        .map(ProviderCapability::from_provider)
        .map_err(|_| invalid("unknown provider in effective view"))
}

pub(crate) fn provider_records(config: &crate::hybrid::config::ProvidersCfg) -> Result<Value> {
    let rows = config
        .iter()
        .map(|(name, record)| {
            let owner = provider::provider(name)
                .map_err(|_| invalid("unknown provider in effective view"))?;
            Ok(json!({
                "provider": owner.name(),
                "configuration": owner.configuration_view(record)?,
                "capability": ProviderCapability::from_provider(owner),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

pub(crate) struct ViewBuilder<'a> {
    origins: &'a Provenance,
    fields: BTreeMap<&'static str, EffectiveField>,
}
impl<'a> ViewBuilder<'a> {
    fn new(origins: &'a Provenance) -> Self {
        Self {
            origins,
            fields: BTreeMap::new(),
        }
    }
    pub(crate) fn has_field(&self, key: &str) -> bool {
        self.origins.get(key).is_some()
    }
    pub(crate) fn push(
        &mut self,
        key: &'static str,
        value: Value,
        capability: Option<ProviderCapability>,
    ) -> Result<()> {
        let origin = self
            .origins
            .get(key)
            .ok_or_else(|| invalid("effective view contains an undeclared field"))?;
        if self.fields.contains_key(key) {
            return Err(invalid("effective view contains a duplicate field"));
        }
        self.fields.insert(
            key,
            EffectiveField {
                key,
                value,
                origin: origin.clone(),
                capability,
            },
        );
        Ok(())
    }
    fn finish(self) -> Result<Vec<EffectiveField>> {
        if self.fields.len() != self.origins.iter().count() {
            return Err(invalid("effective view is missing declared fields"));
        }
        Ok(self.fields.into_values().collect())
    }
}
fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{bootstrap::DefaultsContext, resolve::Resolution, schema::SourceId};

    #[test]
    fn provided_snapshot_origins_are_preserved_without_guessing_paths() {
        let snapshot = Resolution::<ConfigSpec>::from_provided(
            &DefaultsContext::standalone(),
            &Config::default(),
        )
        .finish()
        .unwrap();
        let view = from_resolved(&snapshot).unwrap();
        assert!(
            view.fields
                .iter()
                .all(|field| field.origin.source == SourceId::ProvidedSnapshot)
        );
    }
    #[test]
    fn checked_view_rejects_missing_duplicate_and_undeclared_fields() {
        let snapshot = Resolution::<ConfigSpec>::new(&DefaultsContext::standalone())
            .finish()
            .unwrap();
        let mut builder = ViewBuilder::new(snapshot.provenance());
        assert!(builder.push("not-a-field", json!(1), None).is_err());
        builder.push("max_file_size", json!(1), None).unwrap();
        assert!(builder.push("max_file_size", json!(2), None).is_err());
        assert!(builder.finish().is_err());
    }
}
