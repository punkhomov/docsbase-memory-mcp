//! Configuration loading and precedence: CLI > project > global > defaults
//! (FR-28, FR-29; OQ-4; ADR-6).

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::hybrid::config::{
    CacheCfg, DEFAULT_DIMENSIONS, DEFAULT_EMBEDDING_MODEL, DEFAULT_RERANK_MODEL,
    DEFAULT_RERANK_WINDOW, EmbeddingCfg, HybridCfg, MAX_RERANK_WINDOW, MIN_RERANK_WINDOW,
    PROVIDER_OPENROUTER, Ranking, RerankerCfg,
};

pub mod paths;

/// Effective configuration after merging all sources.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Raw file shape; every key optional so sources can be layered.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    ignores: Option<Vec<String>>,
    max_file_size: Option<u64>,
    max_docs_per_project: Option<usize>,
    auto_index: Option<bool>,
    hybrid: Option<HybridToggle>,
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
    fn into_cfg(self) -> HybridCfg {
        match self {
            Self::Bool(true) => HybridCfg::enabled_with_defaults(),
            Self::Bool(false) => HybridCfg::default(),
            Self::Table(table) => HybridCfg {
                enabled: table.enabled,
                embedding: table.embedding.map(|embedding| EmbeddingCfg {
                    provider: embedding.provider,
                    model: embedding.model,
                    dimensions: embedding.dimensions,
                    normalize: embedding.normalize,
                }),
                reranker: table.reranker.map(|reranker| RerankerCfg {
                    provider: reranker.provider,
                    model: reranker.model,
                    window: reranker.window,
                }),
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
            },
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
    #[serde(default = "def_embedding_model")]
    model: String,
    #[serde(default = "def_dimensions")]
    dimensions: u32,
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
    #[serde(default = "def_rerank_model")]
    model: String,
    #[serde(default = "def_window")]
    window: u32,
}

fn def_true() -> bool {
    true
}

fn def_provider() -> String {
    PROVIDER_OPENROUTER.to_owned()
}

fn def_embedding_model() -> String {
    DEFAULT_EMBEDDING_MODEL.to_owned()
}

fn def_dimensions() -> u32 {
    DEFAULT_DIMENSIONS
}

fn def_cache_max_bytes() -> u64 {
    CacheCfg::default().max_bytes
}

fn def_rerank_model() -> String {
    DEFAULT_RERANK_MODEL.to_owned()
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
            config.apply(read_file(&global)?);
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
            self.apply(read_file(&project)?);
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
            if embedding.provider != PROVIDER_OPENROUTER {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.embedding.provider \"{}\" is not supported in iteration 1 (only \"{PROVIDER_OPENROUTER}\")",
                        embedding.provider
                    ),
                });
            }
            if embedding.dimensions == 0 {
                return Err(Error::Admission {
                    message: "hybrid.embedding.dimensions must be > 0".to_owned(),
                });
            }
        }
        if let Some(reranker) = &self.hybrid_cfg.reranker {
            if reranker.provider != PROVIDER_OPENROUTER {
                return Err(Error::Admission {
                    message: format!(
                        "hybrid.reranker.provider \"{}\" is not supported in iteration 1 (only \"{PROVIDER_OPENROUTER}\")",
                        reranker.provider
                    ),
                });
            }
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

    fn apply(&mut self, file: FileConfig) {
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
            self.hybrid_cfg = toggle.into_cfg();
            self.hybrid = self.hybrid_cfg.enabled;
        }
    }
}

fn read_file(path: &Path) -> Result<FileConfig> {
    // Config files are repo-controlled: cap them before reading (T48).
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
    let text = std::fs::read_to_string(path).map_err(|err| {
        Error::internal_with_source(format!("read {}: {err}", path.display()), err)
    })?;
    toml::from_str(&text)
        .map_err(|err| Error::internal_with_source(format!("parse {}: {err}", path.display()), err))
}
