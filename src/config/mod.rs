//! Configuration loading and precedence: CLI > project > global > defaults
//! (FR-28, FR-29; OQ-4; ADR-6).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::hybrid::config::{
    CacheCfg, DEFAULT_RERANK_WINDOW, HybridCfg, MAX_RERANK_WINDOW, MIN_RERANK_WINDOW,
    PROVIDER_OPENROUTER, ProvidersCfg, Ranking, resolve_embedding, resolve_reranker,
};

pub mod paths;

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
        Self {
            ignores: Vec::new(),
            max_file_size: 1_048_576,
            max_docs_per_project: 20_000,
            auto_index: false,
            hybrid: false,
            hybrid_cfg: HybridCfg::default(),
            providers: ProvidersCfg::default(),
        }
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
    hybrid: Option<HybridToggle>,
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
    hybrid: Option<HybridToggle>,
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

/// File-level `hybrid` accepts both the legacy boolean shorthand and the
/// full table form (FR-13); the table replaces the value wholesale.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum HybridToggle {
    Bool(bool),
    Table(HybridTable),
}

impl HybridToggle {
    fn into_cfg(self) -> Result<HybridCfg> {
        match self {
            Self::Bool(true) => Ok(HybridCfg::enabled_with_defaults()),
            Self::Bool(false) => Ok(HybridCfg::default()),
            Self::Table(table) => {
                let embedding = match table.embedding {
                    Some(embedding) => Some(resolve_embedding(
                        &embedding.provider,
                        embedding.model.as_deref(),
                        embedding.dimensions,
                        embedding.normalize,
                    )?),
                    None => None,
                };
                let reranker = match table.reranker {
                    Some(reranker) => Some(resolve_reranker(
                        &reranker.provider,
                        reranker.model.as_deref(),
                        reranker.window,
                    )?),
                    None => None,
                };
                Ok(HybridCfg {
                    enabled: table.enabled,
                    embedding,
                    reranker,
                    ranking: table
                        .ranking
                        .map_or(Ranking::Lexical, |ranking| match ranking {
                            RankingFile::Lexical => Ranking::Lexical,
                            RankingFile::Rrf => Ranking::Rrf,
                        }),
                    cache: table
                        .cache
                        .map_or_else(CacheCfg::default, |cache| CacheCfg {
                            max_bytes: cache.max_bytes,
                        }),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridTable {
    #[serde(default = "def_true")]
    enabled: bool,
    #[serde(default)]
    embedding: Option<HybridEmbedding>,
    #[serde(default)]
    reranker: Option<HybridReranker>,
    #[serde(default)]
    ranking: Option<RankingFile>,
    #[serde(default)]
    cache: Option<HybridCache>,
}

/// File spelling of the fusion mode (FR-17); unknown values fail parsing.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RankingFile {
    Lexical,
    Rrf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridEmbedding {
    #[serde(default = "def_provider")]
    provider: String,
    // Absent `model`/`dimensions` resolve per provider (FR-5): OpenRouter
    // fills its documented defaults, fastembed requires `model` and takes
    // dims from the registry.
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    dimensions: Option<u32>,
    #[serde(default = "def_true")]
    normalize: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridCache {
    #[serde(default = "def_cache_max_bytes")]
    max_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridReranker {
    #[serde(default = "def_provider")]
    provider: String,
    // Absent `model` resolves per provider (FR-5): OpenRouter uses its
    // default; fastembed requires an explicit registry model.
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "def_window")]
    window: u32,
}

fn def_true() -> bool {
    true
}

fn def_provider() -> String {
    PROVIDER_OPENROUTER.to_owned()
}

fn def_cache_max_bytes() -> u64 {
    CacheCfg::default().max_bytes
}

fn def_window() -> u32 {
    DEFAULT_RERANK_WINDOW
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
        let mut config = Config::default();
        let global = config_dir.join("config.toml");
        if global.exists() {
            config.apply(read_global(&global)?)?;
        }
        config.validate()?;
        Ok(config)
    }

    /// Layers the project config on top of a global snapshot (read when the
    /// project is opened, OQ-6).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the project file exists but cannot be
    /// read or parsed, and [`Error::Admission`] when a limit is zero.
    pub fn for_project(global: &Config, root: &Path) -> Result<Self> {
        let mut config = global.clone();
        config.apply_project(root)?;
        config.validate()?;
        Ok(config)
    }

    /// Same as [`Config::load`] but with an explicit config directory (testable).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when a config file exists but cannot be read
    /// or parsed, and [`Error::Admission`] when a limit is zero.
    pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<Self> {
        let mut config = Self::global_from(config_dir)?;
        if let Some(root) = project_root {
            config.apply_project(root)?;
        }
        config.validate()?;
        Ok(config)
    }

    fn apply_project(&mut self, root: &Path) -> Result<()> {
        let project = root.join(".docsbase.toml");
        if project.exists() {
            let project = read_project(&project)?;
            // The project shape has no providers (FR-1); promote it to the
            // common layer with an empty provider map.
            self.apply(GlobalFileConfig {
                ignores: project.ignores,
                max_file_size: project.max_file_size,
                max_docs_per_project: project.max_docs_per_project,
                auto_index: project.auto_index,
                hybrid: project.hybrid,
                providers: None,
            })?;
        }
        Ok(())
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
        self.validate_hybrid()?;
        Ok(())
    }

    /// Hybrid-specific admission checks: provider allowlist (iteration 1)
    /// and bounded, sane stage values (FR-3, FR-13).
    fn validate_hybrid(&self) -> Result<()> {
        if self.hybrid_cfg.cache.max_bytes == 0 {
            return Err(Error::Admission {
                message: "hybrid.cache.max_bytes must be > 0".to_owned(),
            });
        }
        if self.hybrid_cfg.enabled
            && self.hybrid_cfg.ranking == Ranking::Rrf
            && self.hybrid_cfg.embedding.is_none()
        {
            return Err(Error::Admission {
                message: "hybrid.ranking = \"rrf\" requires the embedding stage (FR-17)".to_owned(),
            });
        }
        if let Some(embedding) = &self.hybrid_cfg.embedding {
            // Provider allowlist and provider-aware defaults are enforced at
            // resolution time (FR-5); here only the cross-stage sanity.
            // Provisioning (runtime/model presence) is deliberately NOT
            // enforced at load time: status/diagnostic commands and
            // unrelated projects must load even when a fastembed pin is not
            // yet provisioned. The same single-action Admission hints (connect
            // vs fetch) surface when a stage is actually constructed
            // (FR-12, SC-3, NFR-6), honoring ORT_DYLIB_PATH (review panel
            // C3/C4).
            if embedding.dimensions == 0 {
                return Err(Error::Admission {
                    message: "hybrid.embedding.dimensions must be > 0".to_owned(),
                });
            }
        }
        if let Some(reranker) = &self.hybrid_cfg.reranker {
            if !(MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&reranker.window) {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.reranker.window must be between {MIN_RERANK_WINDOW} and {MAX_RERANK_WINDOW}, got {}",
                        reranker.window
                    ),
                });
            }
        }
        Ok(())
    }

    /// Applies one raw file layer. Provider records come only from the
    /// global file (the project type has no `providers` field).
    fn apply(&mut self, file: GlobalFileConfig) -> Result<()> {
        if let Some(v) = file.ignores {
            self.ignores = v;
        }
        if let Some(v) = file.max_file_size {
            self.max_file_size = v;
        }
        if let Some(v) = file.max_docs_per_project {
            self.max_docs_per_project = v;
        }
        if let Some(v) = file.auto_index {
            self.auto_index = v;
        }
        if let Some(toggle) = file.hybrid {
            self.hybrid_cfg = toggle.into_cfg()?;
            self.hybrid = self.hybrid_cfg.enabled;
        }
        if let Some(raw) = file.providers {
            self.providers = providers_from_raw(raw)?;
        }
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
