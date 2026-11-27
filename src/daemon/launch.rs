//! One bounded global launch capture. Diagnostic stamps confer no CF5 authority.

use std::path::{Path, PathBuf};

use crate::config::capture::{ReadFailure, path_is_absent, read_prefix};
use crate::config::resolve::{LaunchFailure, LaunchUse, has_version_header};
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
    Unavailable { reason: String, versioned: bool },
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
/// Existing fixed identity and pure project policy, captured before startup resources.
pub(crate) struct FixedProject {
    pub project: crate::store::models::Project,
    pub config: std::sync::Arc<Config>,
}

/// Admission is consumed once; lifecycle forwards it without reopening sources.
pub(crate) struct AdmittedLaunch {
    pub launch: LaunchConfig,
    pub website: Option<FixedProject>,
    pub catalog_epoch: Option<[u8; 16]>,
}

impl AdmittedLaunch {
    pub(crate) const fn registry_schema(&self) -> u32 {
        if self.catalog_epoch.is_some() {
            crate::store::GENERATION_SCHEMA_VERSION
        } else {
            crate::store::migrations::SCHEMA_VERSION
        }
    }
}

pub(crate) fn lexical_only(config: &Config) -> Result<()> {
    if config.hybrid_cfg.enabled {
        return Err(Error::Admission {
            message: "website profile requires lexical-only global and project policy".to_owned(),
        });
    }
    Ok(())
}

impl LaunchConfig {
    pub(crate) fn admit(
        self,
        cache: &Path,
        settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    ) -> Result<AdmittedLaunch> {
        self.config.settings().validate()?;
        let mut catalog_epoch = None;
        let website = match &self.config.settings().daemon.profile {
            crate::daemon::settings::DaemonProfile::Shared => {
                let config = self.config.settings();
                if cfg!(unix)
                    && config.vector_store.is_some()
                    && let Ok(db) =
                        crate::store::Db::open_readonly_for_admission(cache, settings, true)
                    && db.has_generation_catalog()
                {
                    catalog_epoch = Some(db.generation_epoch()?);
                }
                None
            }
            crate::daemon::settings::DaemonProfile::Website { project_root } => {
                lexical_only(self.config.settings())?;
                let root = project_root
                    .canonicalize()
                    .map_err(|error| Error::Admission {
                        message: format!("website project root must exist: {error}"),
                    })?;
                if !root.is_dir() {
                    return Err(Error::Admission {
                        message: "website project root must be a directory".to_owned(),
                    });
                }
                let allow_catalog = cfg!(unix) && self.config.settings().vector_store.is_some();
                let db =
                    crate::store::Db::open_readonly_for_admission(cache, settings, allow_catalog)
                        .map_err(|error| Error::Admission {
                        message: format!(
                            "website requires an existing registered indexed project: {error}"
                        ),
                    })?;
                let project = crate::daemon::registry::list_projects(&db)?
                    .into_iter()
                    .find(|project| {
                        crate::platform::paths::normalize_for_compare(&project.canonical_root)
                            == crate::platform::paths::normalize_for_compare(&root)
                    })
                    .ok_or_else(|| Error::Admission {
                        message: "website root must match an existing registered project"
                            .to_owned(),
                    })?;
                if db.has_generation_catalog() {
                    catalog_epoch = Some(db.generation_epoch()?);
                    crate::store::project_index::with_catalog_pair(
                        &db,
                        &project,
                        |_, _, _| Ok(()),
                    )?;
                } else if project.status != crate::store::models::ProjectStatus::Indexed {
                    return Err(Error::Admission {
                        message: "website project must already be indexed".to_owned(),
                    });
                }
                let config = Config::for_project(self.config.settings(), &project.canonical_root)?;
                lexical_only(&config)?;
                Some(FixedProject {
                    project,
                    config: std::sync::Arc::new(config),
                })
            }
        };
        Ok(AdmittedLaunch {
            launch: self,
            website,
            catalog_epoch,
        })
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
                bytes: CapturedBytes::Unavailable {
                    reason: error.to_string(),
                    versioned: false,
                },
            },
        }
    }

    /// Resolve a complete policy from captured bytes and a whole-value CLI overlay.
    /// No file is read here; existing-project admission belongs to profile activation.
    ///
    /// # Errors
    /// Unavailable/invalid global source or invalid daemon patch.
    pub fn resolve(&self, patch: DaemonPatch) -> Result<Resolved<Config>> {
        self.resolution(LaunchUse::Runtime)
            .map_err(LaunchFailure::into_error)?
            .finish(patch)
    }

    fn resolution(
        &self,
        launch_use: LaunchUse,
    ) -> std::result::Result<crate::config::resolve::LaunchResolution, LaunchFailure> {
        let bytes = match &self.bytes {
            CapturedBytes::Missing => None,
            CapturedBytes::Loaded(bytes) => Some(bytes.as_slice()),
            CapturedBytes::Unavailable { reason, versioned } => {
                let error = Error::internal(format!("global config unavailable: {reason}"));
                return Err(if *versioned {
                    LaunchFailure::Versioned(error)
                } else {
                    LaunchFailure::Legacy(error)
                });
            }
        };
        let path = self.path.as_deref().ok_or_else(|| {
            LaunchFailure::Legacy(Error::internal("global config path unavailable"))
        })?;
        crate::config::resolve::launch_resolution(path, bytes, launch_use)
    }

    pub(crate) fn strict(&self, patch: DaemonPatch) -> Result<LaunchConfig> {
        Ok(LaunchConfig {
            config: self.resolve(patch)?,
            path: self.path.clone(),
            stamp: self.stamp(),
            error: None,
        })
    }

    // The library compatibility constructors retain v1 for an absent
    // source. Both explicit source versions still use the source resolver.
    pub(crate) fn library_compatibility(&self, patch: DaemonPatch) -> Result<LaunchConfig> {
        if matches!(self.bytes, CapturedBytes::Missing) {
            Ok(LaunchConfig {
                config: crate::config::resolve::LaunchResolution::legacy_defaults()
                    .finish(patch)?,
                path: self.path.clone(),
                stamp: self.stamp(),
                error: None,
            })
        } else {
            self.legacy(patch)
        }
    }

    /// Compatibility launch preserves warning/default behavior and blocks project opens
    /// through the retained global error, just as the old Shared constructor did.
    pub(crate) fn legacy(&self, patch: DaemonPatch) -> Result<LaunchConfig> {
        self.legacy_for(patch, LaunchUse::Runtime)
    }

    fn legacy_for(&self, patch: DaemonPatch, launch_use: LaunchUse) -> Result<LaunchConfig> {
        match self.resolution(launch_use) {
            Ok(resolution) => Ok(LaunchConfig {
                config: resolution.finish(patch)?,
                path: self.path.clone(),
                stamp: self.stamp(),
                error: None,
            }),
            Err(
                LaunchFailure::Daemon(error)
                | LaunchFailure::Remote(error)
                | LaunchFailure::Versioned(error),
            ) => Err(error),
            Err(LaunchFailure::Legacy(error)) => {
                // Shared alone retains the source-error compatibility adapter.
                if matches!(
                    &patch.profile,
                    Some(crate::daemon::settings::DaemonProfile::Website { .. })
                ) {
                    return Err(error);
                }
                eprintln!("warning: global config unavailable: {error}");
                let config =
                    crate::config::resolve::LaunchResolution::legacy_defaults().finish(patch)?;
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
    let limit = crate::limits::MAX_STATE_BYTES;
    let unavailable = |error: ReadFailure, versioned| CapturedBytes::Unavailable {
        reason: format!("read {} ({limit} byte limit): {error}", path.display()),
        versioned,
    };
    let snapshot = std::fs::File::open(path)
        .map_err(ReadFailure::Io)
        .and_then(|file| read_prefix(file, limit));
    match snapshot {
        Ok((bytes, false)) => CapturedBytes::Loaded(bytes),
        Ok((prefix, true)) => unavailable(ReadFailure::LimitExceeded, has_version_header(&prefix)),
        Err(ReadFailure::Io(error)) if path_is_absent(path, &error) => CapturedBytes::Missing,
        Err(error) => unavailable(error, false),
    }
}
fn stamp(bytes: &CapturedBytes) -> ConfigStamp {
    match bytes {
        CapturedBytes::Missing => ConfigStamp::Missing,
        CapturedBytes::Loaded(bytes) => ConfigStamp::Loaded(blake3::hash(bytes)),
        CapturedBytes::Unavailable { reason, .. } => ConfigStamp::Unavailable(reason.clone()),
    }
}

pub(crate) fn website_unavailable() -> Error {
    Error::Admission { message: "website daemon unavailable; start it in the foreground with `docsbase serve` (or `docsbase serve --project ABS`)".to_owned() }
}

pub(crate) fn require_shared_without_daemon(launch_use: LaunchUse) -> Result<()> {
    let launch = LaunchSnapshot::capture().legacy_for(DaemonPatch::default(), launch_use)?;
    if matches!(
        launch.config.settings().daemon.profile,
        crate::daemon::settings::DaemonProfile::Website { .. }
    ) {
        return Err(website_unavailable());
    }
    Ok(())
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
