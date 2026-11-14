//! Pure typed layering, plus explicit-path v1 compatibility composition.

use std::marker::PhantomData;
use std::path::Path;

use super::bootstrap::DefaultsContext;
use super::schema::{Capabilities, Provenance, Resolved, SettingsSpec, SourceId};
use crate::error::{Error, Result};

/// A resolution owns one immutable defaults catalog and stages patches atomically.
pub struct Resolution<S: SettingsSpec> {
    defaults: S::Defaults,
    settings: S::Settings,
    origins: Provenance,
    capabilities: Capabilities,
    marker: PhantomData<S>,
}
impl<S: SettingsSpec> Resolution<S> {
    /// Computes the owner catalog once, before any layer is applied.
    #[must_use]
    pub fn new(context: &DefaultsContext) -> Self {
        let defaults = S::defaults(context);
        let settings = S::initial(&defaults);
        Self {
            defaults,
            settings,
            origins: Provenance::initial(&S::fields(), &SourceId::Defaults),
            capabilities: context.capabilities,
            marker: PhantomData,
        }
    }
    /// Starts a new resolution while retaining real snapshot provenance.
    #[must_use]
    pub fn from_resolved(context: &DefaultsContext, snapshot: &Resolved<S::Settings>) -> Self {
        let mut resolution = Self::new(context);
        resolution.settings = snapshot.settings().clone();
        resolution.origins = snapshot.provenance().clone();
        resolution
    }
    /// Starts from legacy data whose earlier sources cannot be reconstructed.
    #[must_use]
    pub fn from_provided(context: &DefaultsContext, snapshot: &S::Settings) -> Self {
        let mut resolution = Self::new(context);
        resolution.settings = snapshot.clone();
        resolution.origins = Provenance::initial(&S::fields(), &SourceId::ProvidedSnapshot);
        resolution
    }
    /// Applies a layer without changing this builder if the owner rejects it.
    ///
    /// # Errors
    /// Forbidden scopes or invalid patch shape.
    pub fn apply(&mut self, source: &SourceId, patch: S::Patch) -> Result<()> {
        if !source.permits(S::SCOPE) {
            return Err(Error::Admission {
                message: "settings scope forbids project override of a shared component".to_owned(),
            });
        }
        let mut settings = self.settings.clone();
        let mut origins = self.origins.clone();
        S::overlay(&mut settings, patch, &self.defaults, source, &mut origins)?;
        self.settings = settings;
        self.origins = origins;
        Ok(())
    }
    /// Validates the current layer boundary without consuming the catalog.
    ///
    /// # Errors
    /// Invalid effective settings.
    pub fn validate(&self) -> Result<()> {
        S::validate(&self.settings, &self.capabilities)
    }
    /// Produces complete, validated settings and their immutable origins.
    ///
    /// # Errors
    /// Invalid effective settings.
    pub fn finish(self) -> Result<Resolved<S::Settings>> {
        self.validate()?;
        Ok(Resolved::new(self.settings, self.origins))
    }
}

/// Resolves typed layers without filesystem, environment or runtime access.
///
/// # Errors
/// Invalid patch, scope or final settings.
pub fn resolve_component<S: SettingsSpec>(
    context: &DefaultsContext,
    layers: impl IntoIterator<Item = (SourceId, S::Patch)>,
) -> Result<Resolved<S::Settings>> {
    let mut resolution = Resolution::<S>::new(context);
    for (source, patch) in layers {
        resolution.apply(&source, patch)?;
    }
    resolution.finish()
}

use super::{Config, ConfigSpec, read_global, read_project};

fn global_resolution(config_dir: &Path) -> Result<Resolution<ConfigSpec>> {
    let mut resolution = Resolution::new(&DefaultsContext::standalone());
    let path = config_dir.join("config.toml");
    if path.exists() {
        resolution.apply(&SourceId::Global(path.clone()), read_global(&path)?)?;
    }
    // v1 rejects an invalid global even if a later project could replace it.
    resolution.validate()?;
    Ok(resolution)
}
fn project_layer(resolution: &mut Resolution<ConfigSpec>, root: &Path) -> Result<()> {
    let path = root.join(".docsbase.toml");
    if path.exists() {
        let project = read_project(&path)?;
        resolution.apply(&SourceId::Project(path), project.into_global())?;
    }
    Ok(())
}
/// Loads v1 files while retaining path provenance and whole-value replacement.
///
/// # Errors
/// File errors, invalid settings or project provider records.
pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<Resolved<Config>> {
    let mut resolution = global_resolution(config_dir)?;
    if let Some(root) = project_root {
        project_layer(&mut resolution, root)?;
    }
    resolution.finish()
}
/// Loads just the global v1 snapshot with real origins.
///
/// # Errors
/// File errors or invalid settings.
pub fn global_from(config_dir: &Path) -> Result<Resolved<Config>> {
    global_resolution(config_dir)?.finish()
}

/// Resolve a launch's already captured v1 bytes; no filesystem access or v2 activation.
pub(crate) fn launch_resolution(
    path: &Path,
    bytes: Option<&[u8]>,
) -> std::result::Result<LaunchResolution, LaunchFailure> {
    let mut resolution = Resolution::<ConfigSpec>::new(&DefaultsContext::standalone());
    let mut website = false;
    if let Some(bytes) = bytes {
        let text = std::str::from_utf8(bytes).map_err(|err| {
            LaunchFailure::Legacy(Error::internal_with_source(
                format!("read {}: {err}", path.display()),
                err,
            ))
        })?;
        // Parse the canonical document once. Decode/validate the owner section
        // first so neither its errors nor a recognized Website selector can be
        // erased by the legacy Shared source-error adapter.
        let mut document =
            super::parse_global_document(path, text).map_err(LaunchFailure::Legacy)?;
        let source = SourceId::Global(path.to_owned());
        // New owner policy errors precede unrelated legacy decoding failures.
        if let Some(remote) = document.remove("remote") {
            let owner_policy = crate::hybrid::remote_settings::RemoteSpec::decode_patch(remote)
                .map_err(LaunchFailure::Remote)?;
            resolution
                .apply(
                    &source,
                    super::GlobalFileConfig {
                        remote: Some(owner_policy),
                        ..super::GlobalFileConfig::default()
                    },
                )
                .map_err(LaunchFailure::Remote)?;
            crate::hybrid::remote_settings::RemoteSpec::validate(
                &resolution.settings.remote,
                &resolution.capabilities,
            )
            .map_err(LaunchFailure::Remote)?;
        }
        if let Some(daemon) = document.remove("daemon") {
            let owner_patch = daemon
                .try_into::<crate::daemon::settings::DaemonPatch>()
                .map_err(|error| {
                    LaunchFailure::Daemon(Error::internal_with_source(
                        format!("parse {} daemon policy: {error}", path.display()),
                        error,
                    ))
                })?;
            resolution
                .apply(
                    &source,
                    super::GlobalFileConfig {
                        daemon: Some(owner_patch),
                        ..super::GlobalFileConfig::default()
                    },
                )
                .map_err(LaunchFailure::Daemon)?;
            crate::daemon::settings::DaemonSpec::validate(
                &resolution.settings.daemon,
                &resolution.capabilities,
            )
            .map_err(LaunchFailure::Daemon)?;
            website = matches!(
                resolution.settings.daemon.profile,
                crate::daemon::settings::DaemonProfile::Website { .. }
            );
        }
        resolution
            .apply(
                &source,
                super::decode_global_document(path, document)
                    .map_err(|error| LaunchFailure::classify(error, website))?,
            )
            .map_err(|error| LaunchFailure::classify(error, website))?;
    }
    // Invalid globals never disappear behind a later CLI selector.
    resolution
        .validate()
        .map_err(|error| LaunchFailure::classify(error, website))?;
    Ok(LaunchResolution(resolution))
}

#[derive(Debug)]
pub(crate) enum LaunchFailure {
    // Existing malformed Shared sources retain their compatibility adapter.
    Legacy(Error),
    // New owner errors and every recognized Website failure must fail closed.
    Daemon(Error),
    // A recognized remote policy is never a legacy Shared source fallback.
    Remote(Error),
}
impl LaunchFailure {
    fn classify(error: Error, website: bool) -> Self {
        if website {
            Self::Daemon(error)
        } else {
            Self::Legacy(error)
        }
    }
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Legacy(error) | Self::Daemon(error) | Self::Remote(error) => error,
        }
    }
}

// Retain the same once-computed defaults catalog between source validation and
// the CLI patch. Legacy fallback applies only to classified Shared legacy-source
// errors, never daemon owner, recognized Website or CLI errors.
pub(crate) struct LaunchResolution(Resolution<ConfigSpec>);
impl LaunchResolution {
    pub(crate) fn finish(
        mut self,
        patch: crate::daemon::settings::DaemonPatch,
    ) -> Result<Resolved<Config>> {
        self.0.apply(
            &SourceId::Cli,
            super::GlobalFileConfig {
                daemon: Some(patch),
                ..super::GlobalFileConfig::default()
            },
        )?;
        self.0.finish()
    }
}
/// Layers a project over a typed snapshot, retaining its earlier origins.
///
/// # Errors
/// File errors or invalid settings.
pub fn for_project(global: &Resolved<Config>, root: &Path) -> Result<Resolved<Config>> {
    let mut resolution =
        Resolution::<ConfigSpec>::from_resolved(&DefaultsContext::standalone(), global);
    project_layer(&mut resolution, root)?;
    resolution.finish()
}
pub(super) fn legacy_for_project(global: &Config, root: &Path) -> Result<Resolved<Config>> {
    let mut resolution =
        Resolution::<ConfigSpec>::from_provided(&DefaultsContext::standalone(), global);
    project_layer(&mut resolution, root)?;
    resolution.finish()
}

/// Resolves caller-supplied versioned sources for offline review only.
/// Ordinary file loaders deliberately do not activate the v2 source format.
///
/// # Errors
/// Invalid source order, scope, stage settings or unsupported configuration.
pub fn preview_sources(
    global: &super::source::SourceLayer,
    project: Option<&super::source::SourceLayer>,
) -> Result<super::source::SourcePreview> {
    use super::source::{ConfigVersion, PreviewConflict, SourcePreview, invalid};
    if !matches!(global.source, SourceId::Global(_))
        || project.is_some_and(|p| !matches!(p.source, SourceId::Project(_)))
    {
        return Err(invalid(
            "preview requires a global source followed by a project source",
        ));
    }
    let mut resolution = Resolution::<ConfigSpec>::new(&DefaultsContext::standalone());
    let strict_cap = (global.version == ConfigVersion::V2).then(|| {
        global
            .vector_max_bytes
            .unwrap_or(resolution.settings.hybrid_cfg.cache.max_bytes)
    });
    for layer in std::iter::once(global).chain(project) {
        resolution
            .apply(&layer.source, layer.base.clone())
            .map_err(|_| invalid("invalid base configuration in source preview"))?;
        if let Some(patch) = &layer.hybrid_merge {
            crate::hybrid::config::merge::overlay(
                &mut resolution.settings.hybrid_cfg,
                patch.clone(),
                &resolution.defaults.hybrid,
                &layer.source,
                &mut resolution.origins,
            )
            .map_err(|_| invalid("invalid hybrid stage settings in source preview"))?;
            resolution.settings.hybrid = resolution.settings.hybrid_cfg.enabled;
        }
        resolution
            .validate()
            .map_err(|_| invalid("invalid effective settings in source preview"))?;
    }
    let legacy_vector_max_bytes = resolution.settings.hybrid_cfg.cache.max_bytes;
    let mut conflicts = Vec::new();
    if let Some(cap) = strict_cap {
        if let Some(project) = project
            && let Some(max_bytes) = project.legacy_vector_override()
        {
            conflicts.push(PreviewConflict::LegacyProjectVectorBudget {
                source: project.source.clone(),
                max_bytes,
            });
        }
        resolution.settings.hybrid_cfg.cache.max_bytes = cap;
        let origin = if global.vector_max_bytes.is_some() {
            global.source.clone()
        } else {
            SourceId::Defaults
        };
        resolution.origins.rebind(
            "hybrid.cache.max_bytes",
            crate::vector_cache::V2_MAX_BYTES_FIELD,
            &origin,
        )?;
    }
    Ok(SourcePreview {
        effective: resolution.finish()?,
        legacy_vector_max_bytes,
        conflicts,
        runtime_activation_available: false,
    })
}
