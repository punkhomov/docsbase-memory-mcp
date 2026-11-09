//! Explicit bootstrap facts. Capturing environment is separate from resolution.

use std::ffi::OsString;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use super::schema::{Capabilities, SourceId};
use crate::error::{Error, Result};

/// How the controlling boundary was entered; no consent policy is applied yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entrypoint {
    /// Direct CLI.
    Cli,
    /// Service manager.
    Service,
    /// MCP frontend.
    Mcp,
    /// Daemon autostart.
    Autostart,
    /// Deterministic legacy/embedded defaults.
    Standalone,
}

/// Captured launch facts, not process-global reads inside settings components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrypointFacts {
    /// Launch class.
    pub mode: Entrypoint,
    /// Interactive controlling terminal exists.
    pub is_tty: bool,
}
impl EntrypointFacts {
    /// Context for deterministic compatibility adapters.
    #[must_use]
    pub const fn standalone() -> Self {
        Self {
            mode: Entrypoint::Standalone,
            is_tty: false,
        }
    }
}

/// Only data needed to choose defaults; no environment map or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultsContext {
    /// Positive CPU count captured by the caller.
    pub available_parallelism: NonZeroUsize,
    /// Compiled features, not provisioning state.
    pub capabilities: Capabilities,
    /// Explicit launch context.
    pub entrypoint: EntrypointFacts,
}
impl DefaultsContext {
    /// Legacy defaults need neither a home directory nor ambient environment.
    #[must_use]
    pub const fn standalone() -> Self {
        Self {
            available_parallelism: NonZeroUsize::MIN,
            capabilities: Capabilities::current(),
            entrypoint: EntrypointFacts::standalone(),
        }
    }
}

/// Resolved dependency input plus its source; no raw env string is retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapValue<T> {
    /// Validated value.
    pub value: T,
    /// Captured origin.
    pub source: SourceId,
}

/// Whitelisted bootstrap paths and dependency settings, not an ambient env map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapContext {
    /// OS/launch facts passed to pure owners.
    pub defaults: DefaultsContext,
    /// Explicit directory overrides, if present; no directory is created.
    pub cache_dir: Option<BootstrapValue<PathBuf>>,
    /// Explicit global-config directory override.
    pub config_dir: Option<BootstrapValue<PathBuf>>,
    /// Explicit data-directory override.
    pub data_dir: Option<BootstrapValue<PathBuf>>,
    /// Runtime override captured before local provider construction.
    pub ort_dylib_path: Option<BootstrapValue<PathBuf>>,
    /// Daemon executable override.
    pub daemon_exe: Option<BootstrapValue<PathBuf>>,
    /// Resolved Tokio worker count.
    pub tokio_workers: BootstrapValue<NonZeroUsize>,
    /// Resolved Rayon worker count.
    pub rayon_threads: BootstrapValue<NonZeroUsize>,
    /// Process-wide tokenizers parallelism.
    pub tokenizers_parallelism: BootstrapValue<bool>,
}

impl BootstrapContext {
    /// Captures only supported bootstrap inputs through an injectable lookup.
    /// Credentials such as `OPENROUTER_API_KEY` are never requested.
    ///
    /// # Errors
    /// Invalid CPU facts or malformed Tokio input; diagnostics name the key,
    /// never echo the supplied value. Rayon/tokenizer legacy fallback rules remain.
    pub fn capture_with(
        available_parallelism: usize,
        entrypoint: EntrypointFacts,
        mut lookup: impl FnMut(&'static str) -> Option<OsString>,
    ) -> Result<Self> {
        let cores = NonZeroUsize::new(available_parallelism).ok_or_else(|| Error::Admission {
            message: "bootstrap CPU count must be positive".to_owned(),
        })?;
        let tokio_workers = match lookup("TOKIO_WORKER_THREADS") {
            Some(value) => BootstrapValue {
                value: parse_positive("TOKIO_WORKER_THREADS", &value)?,
                source: SourceId::Environment("TOKIO_WORKER_THREADS"),
            },
            None => BootstrapValue {
                value: cores,
                source: SourceId::Os,
            },
        };
        let rayon_threads = rayon_threads(cores, &mut lookup);
        let tokenizers_parallelism = match lookup("TOKENIZERS_PARALLELISM") {
            Some(value) => {
                // tokenizers treats unrecognised spellings as true. Preserve
                // that policy without retaining or logging the raw value.
                let enabled = value.to_str().is_none_or(|value| {
                    !matches!(
                        value.to_ascii_lowercase().as_str(),
                        "" | "off" | "false" | "f" | "no" | "n" | "0"
                    )
                });
                BootstrapValue {
                    value: enabled,
                    source: SourceId::Environment("TOKENIZERS_PARALLELISM"),
                }
            }
            None => BootstrapValue {
                value: true,
                source: SourceId::Defaults,
            },
        };
        Ok(Self {
            defaults: DefaultsContext {
                available_parallelism: cores,
                capabilities: Capabilities::current(),
                entrypoint,
            },
            cache_dir: capture_path("DOCSBASE_CACHE_DIR", &mut lookup),
            config_dir: capture_path("DOCSBASE_CONFIG_DIR", &mut lookup),
            data_dir: capture_path("DOCSBASE_DATA_DIR", &mut lookup),
            ort_dylib_path: capture_path("ORT_DYLIB_PATH", &mut lookup),
            daemon_exe: capture_path("DOCSBASE_DAEMON_EXE", &mut lookup),
            tokio_workers,
            rayon_threads,
            tokenizers_parallelism,
        })
    }
}

fn parse_positive(name: &str, value: &std::ffi::OsStr) -> Result<NonZeroUsize> {
    value
        .to_str()
        .and_then(|text| text.parse::<usize>().ok())
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| Error::Admission {
            message: format!("{name} must be a positive integer"),
        })
}

fn rayon_threads(
    cores: NonZeroUsize,
    lookup: &mut impl FnMut(&'static str) -> Option<OsString>,
) -> BootstrapValue<NonZeroUsize> {
    // Rayon zero means CPU default, whereas invalid text falls through to
    // the deprecated spelling. Never consult that unused fallback on a hit.
    if let Some(value) = lookup("RAYON_NUM_THREADS")
        && let Some(parsed) = value.to_str().and_then(|text| text.parse::<usize>().ok())
    {
        return BootstrapValue {
            value: NonZeroUsize::new(parsed).unwrap_or(cores),
            source: SourceId::Environment("RAYON_NUM_THREADS"),
        };
    }
    if let Some(value) = lookup("RAYON_RS_NUM_CPUS")
        && let Some(parsed) = value
            .to_str()
            .and_then(|text| text.parse::<usize>().ok())
            .and_then(NonZeroUsize::new)
    {
        return BootstrapValue {
            value: parsed,
            source: SourceId::Environment("RAYON_RS_NUM_CPUS"),
        };
    }
    BootstrapValue {
        value: cores,
        source: SourceId::Os,
    }
}

fn capture_path(
    key: &'static str,
    lookup: &mut impl FnMut(&'static str) -> Option<OsString>,
) -> Option<BootstrapValue<PathBuf>> {
    lookup(key).map(|value| BootstrapValue {
        value: PathBuf::from(value),
        source: SourceId::Environment(key),
    })
}
