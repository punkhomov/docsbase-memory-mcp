//! Configuration loading and precedence: CLI > project > global > defaults
//! (FR-28, FR-29; OQ-4; ADR-6).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::hybrid::config::{HybridCfg, HybridDefaults, HybridPatch, HybridSpec, ProvidersCfg};
use bootstrap::DefaultsContext;
use schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};

pub mod bootstrap;
pub mod paths;
pub mod resolve;
pub mod schema;

/// Effective configuration after merging all sources.
///
/// Not `Eq`: provider records carry `toml::Table`, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Extra ignore patterns, appended to the built-in defaults.
    pub ignores: Vec<String>,
    /// Maximum size of a single indexed file, bytes.
    pub max_file_size: u64,
    /// Maximum number of documents per project.
    pub max_docs_per_project: usize,
    /// Register and index a project automatically on first connection (ADR-6: off).
    pub auto_index: bool,
    /// Hybrid retrieval feature flag (phase 2; off in v1).
    pub hybrid: bool,
    /// Hybrid stages resolved from the file forms (bool shorthand or table).
    pub hybrid_cfg: HybridCfg,
    /// Machine-local provider records from the global config (FR-1).
    pub providers: ProvidersCfg,
}

impl Default for Config {
    fn default() -> Self {
        let defaults = ConfigSpec::defaults(&DefaultsContext::standalone());
        ConfigSpec::initial(&defaults)
    }
}

/// Explicit overrides coming from CLI flags (highest precedence).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigOverrides {
    /// Overrides [`Config::ignores`] entirely when set.
    pub ignores: Option<Vec<String>>,
    /// Overrides [`Config::max_file_size`].
    pub max_file_size: Option<u64>,
    /// Overrides [`Config::max_docs_per_project`].
    pub max_docs_per_project: Option<usize>,
    /// Overrides [`Config::auto_index`].
    pub auto_index: Option<bool>,
    /// Overrides [`Config::hybrid`].
    pub hybrid: Option<bool>,
}

/// Raw **global** file shape; every key optional so sources can be layered.
/// Only the global file may carry `[providers.*]` (FR-1): a project file is
/// parsed by [`ProjectFileConfig`], which has no `providers` field, so a
/// provider section in a project is a parse error, not a post-merge check.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalFileConfig {
    ignores: Option<Vec<String>>,
    max_file_size: Option<u64>,
    max_docs_per_project: Option<usize>,
    auto_index: Option<bool>,
    hybrid: Option<HybridPatch>,
    providers: Option<HashMap<String, toml::Table>>,
}

/// Raw **project** file shape: identical to the global shape minus
/// `providers` (FR-1, structural enforcement).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFileConfig {
    ignores: Option<Vec<String>>,
    max_file_size: Option<u64>,
    max_docs_per_project: Option<usize>,
    auto_index: Option<bool>,
    hybrid: Option<HybridPatch>,
}

/// Converts the raw `[providers.*]` map into the typed [`ProvidersCfg`].
///
/// The provider registry owns both halves of validation, so adding a provider
/// never touches this file (FR-4, ADR-18): an unknown name is an admission
/// listing the registry, and the keys of a known provider are validated by
/// that provider. Parsing never consults build features — a file written by a
/// feature-enabled build must load in every build.
fn providers_from_raw(raw: HashMap<String, toml::Table>) -> Result<ProvidersCfg> {
    let mut out = ProvidersCfg::default();
    for (name, table) in raw {
        let provider = crate::hybrid::provider::provider(&name)
            .map_err(|_| crate::hybrid::provider::unknown_provider(&name))?;
        out.insert(name, provider.parse_record(&table)?);
    }
    Ok(out)
}

impl Config {
    /// Loads defaults, then the global config, then the project config.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the config directory cannot be determined
    /// or a config file exists but cannot be read or parsed, and
    /// [`Error::Admission`] when a limit is zero.
    pub fn load(project_root: Option<&Path>) -> Result<Self> {
        Self::load_from(&paths::config_dir()?, project_root)
    }

    /// Loads the global config only (read once at daemon start, OQ-6).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the config directory cannot be
    /// determined or the file exists but cannot be read or parsed, and
    /// [`Error::Admission`] when a limit is zero.
    pub fn global() -> Result<Self> {
        Self::global_from(&paths::config_dir()?)
    }

    /// Same as [`Config::global`] but with an explicit config directory.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the file exists but cannot be read or
    /// parsed, and [`Error::Admission`] when a limit is zero.
    pub fn global_from(config_dir: &Path) -> Result<Self> {
        resolve::global_from(config_dir).map(schema::Resolved::into_settings)
    }

    /// Layers the project config on top of a global snapshot (read when the
    /// project is opened, OQ-6).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the project file exists but cannot be
    /// read or parsed, and [`Error::Admission`] when a limit is zero.
    pub fn for_project(global: &Config, root: &Path) -> Result<Self> {
        resolve::legacy_for_project(global, root).map(schema::Resolved::into_settings)
    }

    /// Same as [`Config::load`] but with an explicit config directory (testable).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when a config file exists but cannot be read
    /// or parsed, and [`Error::Admission`] when a limit is zero.
    pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<Self> {
        resolve::load_from(config_dir, project_root).map(schema::Resolved::into_settings)
    }

    /// Applies CLI overrides on top of the merged configuration.
    #[must_use]
    pub fn with_overrides(mut self, overrides: &ConfigOverrides) -> Self {
        if let Some(v) = &overrides.ignores {
            self.ignores.clone_from(v);
        }
        if let Some(v) = overrides.max_file_size {
            self.max_file_size = v;
        }
        if let Some(v) = overrides.max_docs_per_project {
            self.max_docs_per_project = v;
        }
        if let Some(v) = overrides.auto_index {
            self.auto_index = v;
        }
        if let Some(v) = overrides.hybrid {
            // Boolean override replaces the whole hybrid value, mirroring the
            // `hybrid = true` file shorthand (FR-13): enabled means defaults.
            self.hybrid = v;
            self.hybrid_cfg = if v {
                HybridCfg::enabled_with_defaults()
            } else {
                HybridCfg::default()
            };
        }
        self
    }

    /// Rejects values that would silently disable indexing.
    ///
    /// Callers that apply [`Config::with_overrides`] must validate again.
    ///
    /// # Errors
    /// Returns [`Error::Admission`] for zero limits.
    pub fn validate(&self) -> Result<()> {
        if self.max_file_size == 0 {
            return Err(Error::Admission {
                message: "max_file_size must be > 0".to_owned(),
            });
        }
        if self.max_docs_per_project == 0 {
            return Err(Error::Admission {
                message: "max_docs_per_project must be > 0".to_owned(),
            });
        }
        let frame_payload = (crate::ipc::protocol::MAX_FRAME_BYTES as u64)
            .saturating_sub(crate::ipc::protocol::FRAME_OVERHEAD);
        if self.max_file_size > frame_payload {
            return Err(Error::Admission {
                message: format!(
                    "max_file_size must not exceed {frame_payload} bytes so a get_doc response fits the {} byte protocol frame",
                    crate::ipc::protocol::MAX_FRAME_BYTES
                ),
            });
        }
        crate::hybrid::config::validate_settings(&self.hybrid_cfg)?;
        Ok(())
    }
}

/// Reads the global config file (contains `[providers.*]`).
fn read_global(path: &Path) -> Result<GlobalFileConfig> {
    let text = read_capped(path)?;
    toml::from_str(&text)
        .map_err(|err| Error::internal_with_source(format!("parse {}: {err}", path.display()), err))
}

/// Reads the project config file (FR-1): a `[providers]` section here is a
/// parse error mapped to [`Error::Admission`] — machine capabilities are not
/// repository state and must move to the global config.
fn read_project(path: &Path) -> Result<ProjectFileConfig> {
    let text = read_capped(path)?;
    match toml::from_str::<ProjectFileConfig>(&text) {
        Ok(project) => Ok(project),
        Err(parse_err) => {
            // Detect `[providers]` regardless of spelling (whitespace,
            // inline tables) via a generic parse: if the document contains a
            // top-level `providers` key, the failure is the structural
            // FR-1 violation, not a generic syntax error.
            let has_providers = toml::from_str::<toml::Value>(&text)
                .ok()
                .is_some_and(|value| value.get("providers").is_some());
            if has_providers {
                Err(Error::Admission {
                    message: format!(
                        "{}: [providers] is only allowed in the global config (config.toml); move the section there — {parse_err}",
                        path.display()
                    ),
                })
            } else {
                Err(Error::internal_with_source(
                    format!("parse {}: {parse_err}", path.display()),
                    parse_err,
                ))
            }
        }
    }
}

/// Reads and caps a config file (repo-controlled, T48).
fn read_capped(path: &Path) -> Result<String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > crate::limits::MAX_STATE_BYTES => {
            return Err(Error::internal(format!(
                "config {} is {} bytes, over the {} byte limit",
                path.display(),
                metadata.len(),
                crate::limits::MAX_STATE_BYTES
            )));
        }
        _ => {}
    }
    std::fs::read_to_string(path)
        .map_err(|err| Error::internal_with_source(format!("read {}: {err}", path.display()), err))
}

// Compatibility composition owns only the existing top-level v1 fields.
// Component-specific defaults and rules remain in their owner specs.
struct ConfigDefaults {
    initial: Config,
    hybrid: HybridDefaults,
}
struct ConfigSpec;
impl SettingsSpec for ConfigSpec {
    type Patch = GlobalFileConfig;
    type Settings = Config;
    type Defaults = ConfigDefaults;
    const SCOPE: Scope = Scope::Project;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("ignores", Scope::Project, Unit::List),
        FieldSpec::new("max_file_size", Scope::Project, Unit::Bytes),
        FieldSpec::new("max_docs_per_project", Scope::Project, Unit::Count),
        FieldSpec::new("auto_index", Scope::Project, Unit::Boolean),
        FieldSpec::new("providers", Scope::Machine, Unit::List),
    ];
    fn fields() -> Vec<FieldSpec> {
        let mut fields = Self::FIELDS.to_vec();
        fields.extend_from_slice(HybridSpec::FIELDS);
        fields
    }
    fn defaults(context: &DefaultsContext) -> Self::Defaults {
        let hybrid = HybridSpec::defaults(context);
        let initial = Config {
            ignores: Vec::new(),
            max_file_size: 1_048_576,
            max_docs_per_project: 20_000,
            auto_index: false,
            hybrid: false,
            hybrid_cfg: HybridSpec::initial(&hybrid),
            providers: ProvidersCfg::default(),
        };
        ConfigDefaults { initial, hybrid }
    }
    fn initial(defaults: &Self::Defaults) -> Config {
        defaults.initial.clone()
    }
    fn overlay(
        settings: &mut Config,
        patch: GlobalFileConfig,
        defaults: &Self::Defaults,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        if let Some(v) = patch.ignores {
            settings.ignores = v;
            origins.set("ignores", source)?;
        }
        if let Some(v) = patch.max_file_size {
            settings.max_file_size = v;
            origins.set("max_file_size", source)?;
        }
        if let Some(v) = patch.max_docs_per_project {
            settings.max_docs_per_project = v;
            origins.set("max_docs_per_project", source)?;
        }
        if let Some(v) = patch.auto_index {
            settings.auto_index = v;
            origins.set("auto_index", source)?;
        }
        if let Some(v) = patch.hybrid {
            HybridSpec::overlay(
                &mut settings.hybrid_cfg,
                v,
                &defaults.hybrid,
                source,
                origins,
            )?;
            settings.hybrid = settings.hybrid_cfg.enabled;
        }
        if let Some(raw) = patch.providers {
            settings.providers = providers_from_raw(raw)?;
            origins.set("providers", source)?;
        }
        Ok(())
    }
    fn validate(settings: &Config, _: &Capabilities) -> Result<()> {
        settings.validate()
    }
    fn impact(old: Option<&Config>, next: &Config, _: &ArtifactFacts) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
