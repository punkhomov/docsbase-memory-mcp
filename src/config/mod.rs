//! Configuration loading and precedence: CLI > project > global > defaults
//! (FR-28, FR-29; OQ-4; ADR-6).

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ignores: Vec::new(),
            max_file_size: 1_048_576,
            max_docs_per_project: 20_000,
            auto_index: false,
            hybrid: false,
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
    hybrid: Option<bool>,
}

impl Config {
    /// Loads defaults, then the global config, then the project config.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the config directory cannot be determined
    /// or a config file exists but cannot be read or parsed.
    pub fn load(project_root: Option<&Path>) -> Result<Self> {
        Self::load_from(&paths::config_dir()?, project_root)
    }

    /// Same as [`Config::load`] but with an explicit config directory (testable).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when a config file exists but cannot be read
    /// or parsed.
    pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<Self> {
        let mut config = Config::default();
        let global = config_dir.join("config.toml");
        if global.exists() {
            config.apply(read_file(&global)?);
        }
        if let Some(root) = project_root {
            let project = root.join(".docsbase.toml");
            if project.exists() {
                config.apply(read_file(&project)?);
            }
        }
        Ok(config)
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
            self.hybrid = v;
        }
        self
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
        if let Some(v) = file.hybrid {
            self.hybrid = v;
        }
    }
}

fn read_file(path: &Path) -> Result<FileConfig> {
    let text = std::fs::read_to_string(path).map_err(|err| Error::Internal {
        message: format!("read {}: {err}", path.display()),
    })?;
    toml::from_str(&text).map_err(|err| Error::Internal {
        message: format!("parse {}: {err}", path.display()),
    })
}
