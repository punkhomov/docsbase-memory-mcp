//! One bounded global launch capture. Diagnostic stamps confer no CF5 authority.

use std::path::{Path, PathBuf};

use crate::config::capture::{ReadFailure, read_file};
use crate::config::resolve::LaunchFailure;
use crate::config::schema::Resolved;
use crate::config::{Config, paths};
use crate::daemon::settings::DaemonPatch;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigStamp {
    Missing,
    Loaded(blake3::Hash),
    Unavailable(String),
}

enum CapturedBytes {
    Missing,
    Loaded(Vec<u8>),
    Unavailable(String),
}

/// Immutable bytes captured before launch policy resolution and filesystem writes.
/// This is a restart diagnostic snapshot, not an authoritative source revision.
pub struct LaunchSnapshot {
    path: Option<PathBuf>,
    bytes: CapturedBytes,
}

/// Complete immutable launch data passed to resource consumers.
pub(crate) struct LaunchConfig {
    pub config: Resolved<Config>,
    pub path: Option<PathBuf>,
    pub stamp: ConfigStamp,
    pub error: Option<String>,
}
impl LaunchConfig {
    pub(crate) fn admit_startup(&self) -> Result<()> {
        // Intermediate S1a boundary: S1b removes this refusal only when all fixed
        // project, hybrid, watcher and lifecycle gates protect startup and requests.
        if matches!(
            self.config.settings().daemon.profile,
            crate::daemon::settings::DaemonProfile::Website { .. }
        ) {
            return Err(Error::Admission {
                message: "website profile startup is not supported by this build".to_owned(),
            });
        }
        Ok(())
    }
}

impl LaunchSnapshot {
    /// Capture the explicit global source once, using the existing trusted bootstrap cap.
    #[must_use]
    pub fn capture_from(config_dir: &Path) -> Self {
        let path = config_dir.join("config.toml");
        let bytes = capture(&path);
        Self {
            path: Some(path),
            bytes,
        }
    }

    /// Capture the system-selected global source once. Errors remain visible to adapters.
    #[must_use]
    pub fn capture() -> Self {
        match paths::config_dir() {
            Ok(path) => Self::capture_from(&path),
            Err(error) => Self {
                path: None,
                bytes: CapturedBytes::Unavailable(error.to_string()),
            },
        }
    }

    /// Resolve a complete policy from captured bytes and a whole-value CLI overlay.
    /// No file is read here; existing-project admission belongs to profile activation.
    ///
    /// # Errors
    /// Unavailable/invalid global source or invalid daemon patch.
    pub fn resolve(&self, patch: DaemonPatch) -> Result<Resolved<Config>> {
        self.resolution()
            .map_err(LaunchFailure::into_error)?
            .finish(patch)
    }

    fn resolution(
        &self,
    ) -> std::result::Result<crate::config::resolve::LaunchResolution, LaunchFailure> {
        let bytes = match &self.bytes {
            CapturedBytes::Missing => None,
            CapturedBytes::Loaded(bytes) => Some(bytes.as_slice()),
            CapturedBytes::Unavailable(reason) => {
                return Err(LaunchFailure::Legacy(Error::internal(format!(
                    "global config unavailable: {reason}"
                ))));
            }
        };
        let path = self.path.as_deref().ok_or_else(|| {
            LaunchFailure::Legacy(Error::internal("global config path unavailable"))
        })?;
        crate::config::resolve::launch_resolution(path, bytes)
    }

    pub(crate) fn strict(&self, patch: DaemonPatch) -> Result<LaunchConfig> {
        Ok(LaunchConfig {
            config: self.resolve(patch)?,
            path: self.path.clone(),
            stamp: self.stamp(),
            error: None,
        })
    }

    /// Compatibility launch preserves warning/default behavior and blocks project opens
    /// through the retained global error, just as the old Shared constructor did.
    pub(crate) fn legacy(&self, patch: DaemonPatch) -> Result<LaunchConfig> {
        match self.resolution() {
            Ok(resolution) => Ok(LaunchConfig {
                config: resolution.finish(patch)?,
                path: self.path.clone(),
                stamp: self.stamp(),
                error: None,
            }),
            Err(LaunchFailure::Daemon(error)) => Err(error),
            Err(LaunchFailure::Legacy(error)) => {
                eprintln!("warning: global config unavailable: {error}");
                let config =
                    crate::config::resolve::launch_resolution(Path::new("config.toml"), None)
                        .map_err(LaunchFailure::into_error)?
                        .finish(patch)?;
                Ok(LaunchConfig {
                    config,
                    path: self.path.clone(),
                    stamp: self.stamp(),
                    error: Some(error.to_string()),
                })
            }
        }
    }

    fn stamp(&self) -> ConfigStamp {
        stamp(&self.bytes)
    }
}

fn capture(path: &Path) -> CapturedBytes {
    match read_file(path, crate::limits::MAX_STATE_BYTES) {
        Ok(bytes) => CapturedBytes::Loaded(bytes),
        Err(ReadFailure::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            CapturedBytes::Missing
        }
        Err(error) => CapturedBytes::Unavailable(format!(
            "read {} ({} byte limit): {error}",
            path.display(),
            crate::limits::MAX_STATE_BYTES
        )),
    }
}
fn stamp(bytes: &CapturedBytes) -> ConfigStamp {
    match bytes {
        CapturedBytes::Missing => ConfigStamp::Missing,
        CapturedBytes::Loaded(bytes) => ConfigStamp::Loaded(blake3::hash(bytes)),
        CapturedBytes::Unavailable(reason) => ConfigStamp::Unavailable(reason.clone()),
    }
}

pub(crate) fn restart_notice(path: Option<&Path>, baseline: &ConfigStamp) -> Option<String> {
    let current = path.map_or_else(
        || ConfigStamp::Unavailable("global config path unavailable".to_owned()),
        |path| stamp(&capture(path)),
    );
    match (&current, baseline) {
        (ConfigStamp::Unavailable(reason), _) => Some(format!(
            "global config unavailable ({reason}); restart the daemon: `docsbase daemon stop`"
        )),
        (ConfigStamp::Missing, ConfigStamp::Missing) => None,
        (ConfigStamp::Loaded(a), ConfigStamp::Loaded(b)) if a == b => None,
        _ => Some("global config changed; restart the daemon: `docsbase daemon stop`".to_owned()),
    }
}
