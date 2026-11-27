//! Pure, versioned effective settings views. No provisioning or runtime observation.
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use super::bootstrap::{BootstrapContext, BootstrapValue};
use super::schema::{FieldOrigin, Provenance, Resolved, SourceId};
use super::{Config, ConfigSpec};
use crate::error::{Error, Result};
use crate::hybrid::provider::{self, Provider};
use crate::ipc::settings::ClientSpec;

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
/// Setting whose resolved value determines another field's effective value.
#[derive(Debug, Clone, Serialize)]
pub struct FieldDerivation {
    /// Key of the determining setting.
    pub key: &'static str,
    /// Source of that setting's resolved value.
    pub source: SourceId,
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
    /// Setting that determines this value when it overrides the field's own source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<FieldDerivation>,
    /// Provider needed by this field's stage, not a provisioning probe.
    pub capability: Option<ProviderCapability>,
}
/// Observation basis, kept distinct from a live daemon snapshot.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewBasis {
    /// Caller-supplied immutable settings/provenance.
    ResolvedSnapshot,
    /// Fresh source resolution from explicit paths.
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
    /// Where the current platform enforces the blocking IPC timeout.
    pub blocking_ipc_timeout_capability: &'static str,
    /// Launch inputs captured locally by the CLI when this view is requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<BootstrapView>,
}

/// Machine inputs observed by the CLI, with their original source.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapView {
    cache_dir: Option<BootstrapValue<PathBuf>>,
    config_dir: BootstrapValue<PathBuf>,
    data_dir: Option<BootstrapValue<PathBuf>>,
    daemon_exe: BootstrapValue<PathBuf>,
    ort_dylib_path: Option<BootstrapValue<PathBuf>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ort_path_derived_from: Option<&'static str>,
    tokio_workers: BootstrapValue<NonZeroUsize>,
    rayon_threads: BootstrapValue<NonZeroUsize>,
    tokenizers_parallelism: BootstrapValue<bool>,
    ipc_debug: BootstrapValue<bool>,
    watch_debug: BootstrapValue<bool>,
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
        blocking_ipc_timeout_capability: ClientSpec::blocking_ipc_timeout_capability(),
        bootstrap: None,
    })
}

pub(crate) fn runtime_identity(config: &Config) -> Result<String> {
    settings_identity(
        config,
        "project-runtime-v1",
        Some(super::schema::Scope::Project),
    )
}

pub(crate) fn launch_identity(config: &Config) -> Result<String> {
    settings_identity(config, "daemon-launch-policy-v1", None)
}

fn settings_identity(
    config: &Config,
    domain: &str,
    scope: Option<super::schema::Scope>,
) -> Result<String> {
    use super::{bootstrap::DefaultsContext, resolve::Resolution};
    let snapshot =
        Resolution::<ConfigSpec>::from_provided(&DefaultsContext::standalone(), config).finish()?;
    let view = from_resolved(&snapshot)?;
    // Only the blocking IPC timeout participates in live daemon control.
    let fields: Vec<_> = view
        .fields
        .iter()
        .filter(|field| {
            scope.is_none_or(|scope| field.origin.scope == scope)
                && !ClientSpec::is_mcp_only_field(field.key)
        })
        .map(|field| (field.key, &field.value))
        .collect();
    if scope.is_some() {
        return super::change::policy_identity(domain, &fields);
    }
    let vector_policy = config
        .vector_store
        .as_ref()
        .map(|policy| (policy.max_entries, policy.max_bytes, policy.busy_timeout));
    super::change::policy_identity(domain, &(fields, vector_policy))
}

/// Resolves the same versioned sources as the runtime loader and displays their origins.
/// Does not create directories, open databases or construct/probe providers.
/// Errors deliberately omit source text and the legacy parser's error chain.
///
/// # Errors
/// Unreadable/invalid sources or an invalid effective view.
pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<EffectiveView> {
    let snapshot = super::resolve::load_from(config_dir, project_root)
        .map_err(|_| invalid("cannot resolve configuration for effective view"))?;
    let mut view = from_resolved(&snapshot)
        .map_err(|_| invalid("cannot describe resolved configuration for effective view"))?;
    view.basis = ViewBasis::SourceResolution;
    Ok(view)
}

pub(crate) fn load_with_bootstrap(
    bootstrap: &BootstrapContext,
    project_root: Option<&Path>,
) -> Result<EffectiveView> {
    use super::paths;
    let config_dir = paths::config_dir_from(bootstrap)?;
    let snapshot = super::resolve::load_from_with_defaults(
        &config_dir.value,
        project_root,
        &bootstrap.defaults,
    )
    .map_err(|_| invalid("cannot resolve configuration for effective view"))?;
    let mut view = from_resolved(&snapshot)
        .map_err(|_| invalid("cannot describe resolved configuration for effective view"))?;
    view.basis = ViewBasis::SourceResolution;
    view.coverage = "file_settings_and_bootstrap";
    let data_dir = paths::data_dir_from(bootstrap).ok();
    let (ort_dylib_path, ort_path_derived_from) =
        resolved_ort_path(bootstrap, &snapshot, data_dir.as_ref())?;
    view.bootstrap = Some(BootstrapView {
        cache_dir: paths::cache_dir_from(bootstrap).ok(),
        config_dir,
        data_dir,
        daemon_exe: paths::daemon_exe_from(bootstrap.daemon_exe.as_ref()),
        ort_dylib_path,
        ort_path_derived_from,
        tokio_workers: bootstrap.tokio_workers.clone(),
        rayon_threads: bootstrap.rayon_threads.clone(),
        tokenizers_parallelism: bootstrap.tokenizers_parallelism.clone(),
        ipc_debug: bootstrap.ipc_debug.clone(),
        watch_debug: bootstrap.watch_debug.clone(),
    });
    Ok(view)
}

type OrtPath = (Option<BootstrapValue<PathBuf>>, Option<&'static str>);

fn resolved_ort_path(
    bootstrap: &BootstrapContext,
    snapshot: &Resolved<Config>,
    data_dir: Option<&BootstrapValue<PathBuf>>,
) -> Result<OrtPath> {
    use super::schema::SourceId;
    #[cfg(not(feature = "hybrid-local"))]
    let _ = data_dir;
    if let Some(path) = &bootstrap.ort_dylib_path {
        return Ok((Some(path.clone()), None));
    }
    let Some(provider) = crate::hybrid::config::fastembed_record(&snapshot.settings().providers)?
        .filter(|provider| provider.enabled)
    else {
        return Ok((None, None));
    };
    if let Some(value) = provider.runtime_path {
        let source = snapshot
            .provenance()
            .get("providers")
            .map_or(SourceId::ProvidedSnapshot, |origin| origin.source.clone());
        return Ok((Some(BootstrapValue { value, source }), None));
    }
    #[cfg(feature = "hybrid-local")]
    if let Some(data_dir) = data_dir
        && let Some(value) = crate::hybrid::runtime::install_path_candidate(&data_dir.value)
    {
        return Ok((
            Some(BootstrapValue {
                value,
                source: data_dir.source.clone(),
            }),
            Some("data_dir"),
        ));
    }
    Ok((None, None))
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
                derived_from: None,
                capability,
            },
        );
        Ok(())
    }
    pub(crate) fn set_derived_from(&mut self, key: &str, dependency: &'static str) -> Result<()> {
        let origin = self
            .origins
            .get(dependency)
            .ok_or_else(|| invalid("effective value depends on an undeclared field"))?;
        let field = self
            .fields
            .get_mut(key)
            .ok_or_else(|| invalid("cannot attribute an absent effective value"))?;
        field.derived_from = Some(FieldDerivation {
            key: dependency,
            source: origin.source.clone(),
        });
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
    fn fastembed_defaults_follow_the_captured_cpu_count_without_a_provider_record() {
        use std::num::NonZeroUsize;

        let original = DefaultsContext {
            available_parallelism: NonZeroUsize::new(2).unwrap(),
            ..DefaultsContext::standalone()
        };
        let later = DefaultsContext {
            available_parallelism: NonZeroUsize::new(16).unwrap(),
            ..original
        };
        let captured = Resolution::<ConfigSpec>::new(&original).finish().unwrap();
        let cloned = Resolution::<ConfigSpec>::from_resolved(&later, &captured)
            .finish()
            .unwrap();
        let fresh = Resolution::<ConfigSpec>::new(&later).finish().unwrap();

        for (snapshot, expected_threads) in [(&captured, 2), (&cloned, 2), (&fresh, 4)] {
            assert!(snapshot.settings().providers.get("fastembed").is_none());
            let view = from_resolved(snapshot).unwrap();
            let value = |key| {
                view.fields
                    .iter()
                    .find(|field| field.key == key)
                    .unwrap()
                    .value
                    .clone()
            };
            assert_eq!(value("providers"), json!([]));
            assert_eq!(value("providers.fastembed.batch_size"), 64);
            assert_eq!(value("providers.fastembed.intra_threads"), expected_threads);
        }
    }

    #[test]
    fn client_timeout_changes_follow_runtime_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Config::global_from(directory.path()).unwrap();
        let launch = launch_identity(&baseline).unwrap();
        let project = runtime_identity(&baseline).unwrap();

        for (field, affects_daemon) in [
            ("ipc_io_timeout_ms", true),
            ("mcp_quick_timeout_ms", false),
            ("mcp_long_timeout_ms", false),
        ] {
            std::fs::write(
                directory.path().join("config.toml"),
                format!("config_version=2\n[client]\n{field}=9000\n"),
            )
            .unwrap();
            let changed = Config::global_from(directory.path()).unwrap();
            assert_eq!(launch_identity(&changed).unwrap() != launch, affects_daemon);
            assert_eq!(runtime_identity(&changed).unwrap(), project);
            assert!(!changed.changes_dense_state(&baseline));
        }
    }

    #[test]
    fn dense_invalidation_follows_document_and_vector_inputs() {
        let original = Config {
            hybrid_cfg: crate::hybrid::config::HybridCfg::enabled_with_defaults(),
            ..Config::default()
        };
        let mut changed = original.clone();
        changed.hybrid_cfg.reranker.as_mut().unwrap().window += 1;
        changed.search.request_timeout_ms += 1;
        assert!(!changed.changes_dense_state(&original));

        changed.hybrid_cfg.embedding.as_mut().unwrap().dimensions += 1;
        assert!(changed.changes_dense_state(&original));

        let mut changed = original.clone();
        changed.max_file_size += 1;
        assert!(changed.changes_dense_state(&original));
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
