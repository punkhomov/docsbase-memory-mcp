//! XDG paths for the runtime layout (design §3).
//!
//! `DOCSBASE_CACHE_DIR` / `DOCSBASE_CONFIG_DIR` override XDG discovery; this is
//! what integration tests use to isolate the daemon from the real user dirs.

use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::{Error, Result};

const APP: &str = "docsbase-memory-mcp";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP).ok_or_else(|| Error::Internal {
        message: "cannot determine XDG directories (no home?)".to_owned(),
    })
}

/// Cache root (`$XDG_CACHE_HOME/docsbase-memory-mcp`), overridable via
/// `DOCSBASE_CACHE_DIR`.
///
/// # Errors
/// Returns [`Error::Internal`] when neither the override nor XDG discovery
/// yields a path.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCSBASE_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    project_dirs().map(|dirs| dirs.cache_dir().to_path_buf())
}

/// Config directory (`$XDG_CONFIG_HOME/docsbase-memory-mcp`), overridable via
/// `DOCSBASE_CONFIG_DIR`.
///
/// # Errors
/// Returns [`Error::Internal`] when neither the override nor XDG discovery
/// yields a path.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCSBASE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    project_dirs().map(|dirs| dirs.config_dir().to_path_buf())
}
