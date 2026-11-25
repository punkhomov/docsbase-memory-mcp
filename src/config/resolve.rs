//! Pure typed layering and file-based operational resolution.

use std::marker::PhantomData;
use std::path::Path;

use super::bootstrap::DefaultsContext;
use super::schema::{Capabilities, Provenance, Resolved, SettingsSpec, SourceId};
use crate::error::{Error, Result};
use crate::hybrid::provisioning::ProvisioningSpec;
use crate::hybrid::rrf::settings::RrfSpec;
use crate::index::lexical_settings::LexicalSpec;
use crate::watch::settings::WatchSpec;

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

use super::source::{SourceLayer, invalid};
use super::{Config, ConfigSpec, read_capped, read_capped_if_exists, read_project};

fn global_resolution(config_dir: &Path) -> Result<Resolution<ConfigSpec>> {
    let mut resolution = Resolution::new(&DefaultsContext::standalone());
    let path = config_dir.join("config.toml");
    if let Some(text) = read_capped_if_exists(&path)? {
        let document = super::parse_global_document(&path, &text)?;
        let source = SourceId::Global(path.clone());
        if is_v2(&document) {
            resolution.apply_v2_global(&SourceLayer::from_document(source, &text, document)?)?;
        } else {
            resolution.apply(&source, super::decode_global_document(&path, document)?)?;
        }
    } else {
        resolution.bind_vector_policy(&crate::vector_cache::settings::standalone()?)?;
    }
    // An invalid global is rejected even if a later project could replace it.
    resolution.validate()?;
    Ok(resolution)
}

fn is_v2(document: &toml::Table) -> bool {
    document.get("config_version") == Some(&toml::Value::Integer(2))
}

// A malformed legacy source still has its diagnostic-only daemon adapter.
// A leading version marker remains recognizable when later TOML is broken.
pub(crate) fn has_version_header(bytes: &[u8]) -> bool {
    // Classifying a malformed header never requires decoding the rest of the file.
    bytes
        .split(|&byte| byte == b'\n')
        .map(|line| std::str::from_utf8(line).map(str::trim))
        .find(|line| {
            !line
                .as_ref()
                .is_ok_and(|line| line.is_empty() || line.starts_with('#'))
        })
        .and_then(std::result::Result::ok)
        .is_some_and(|line| {
            toml::from_str::<toml::Table>(line)
                .is_ok_and(|header| header.contains_key("config_version"))
        })
}

impl Resolution<ConfigSpec> {
    fn apply_source(&mut self, layer: &SourceLayer) -> Result<()> {
        self.apply(&layer.source, layer.base.clone())
            .map_err(|_| invalid("invalid base configuration in versioned source"))?;
        if let Some(patch) = &layer.hybrid_merge {
            crate::hybrid::config::merge::overlay(
                &mut self.settings.hybrid_cfg,
                patch.clone(),
                &self.defaults.hybrid,
                &layer.source,
                &mut self.origins,
            )
            .map_err(|_| invalid("invalid hybrid stage settings in versioned source"))?;
            self.settings.hybrid = self.settings.hybrid_cfg.enabled;
        }
        if let Some(patch) = layer.watch {
            WatchSpec::overlay(
                &mut self.settings.watch,
                patch,
                &self.defaults.watch,
                &layer.source,
                &mut self.origins,
            )?;
        }
        if let Some(patch) = layer.lexical {
            LexicalSpec::overlay(
                &mut self.settings.lexical,
                patch,
                &self.defaults.initial.lexical,
                &layer.source,
                &mut self.origins,
            )?;
        }
        if let Some(patch) = layer.rrf {
            RrfSpec::overlay(
                &mut self.settings.rrf,
                patch,
                &self.defaults.initial.rrf,
                &layer.source,
                &mut self.origins,
            )?;
        }
        if let Some(patch) = layer.provisioning {
            ProvisioningSpec::overlay(
                &mut self.settings.provisioning,
                patch,
                &self.defaults.initial.provisioning,
                &layer.source,
                &mut self.origins,
            )?;
        }
        self.validate()
            .map_err(|_| invalid("invalid effective settings in versioned source"))
    }

    fn apply_v2_global(&mut self, layer: &SourceLayer) -> Result<()> {
        self.apply_source(layer)?;
        let policy = layer
            .vector_store
            .as_ref()
            .ok_or_else(|| invalid("v2 global vector policy absent"))?;
        self.bind_vector_policy(policy)
    }

    fn bind_vector_policy(
        &mut self,
        policy: &Resolved<crate::vector_cache::settings::VectorStoreSettings>,
    ) -> Result<()> {
        use crate::vector_cache::settings::{MAX_BYTES_FIELD, VectorStoreSpec};

        for &field in VectorStoreSpec::FIELDS {
            let origin = policy
                .provenance()
                .get(field.key)
                .ok_or_else(|| invalid("v2 vector policy provenance absent"))?;
            if field == MAX_BYTES_FIELD {
                self.origins
                    .rebind("hybrid.cache.max_bytes", field, &origin.source)?;
            } else {
                self.origins.include(field, &origin.source)?;
            }
        }
        self.settings.hybrid_cfg.cache.max_bytes = policy.settings().max_bytes;
        self.settings.vector_store = Some(policy.settings().clone());
        Ok(())
    }

    fn apply_project(&mut self, root: &Path) -> Result<()> {
        let path = root.join(".docsbase.toml");
        if !path.exists() {
            return Ok(());
        }
        let Some(max_bytes) = self
            .settings
            .vector_store
            .as_ref()
            .map(|policy| policy.max_bytes)
        else {
            let project = read_project(&path)?;
            return self.apply(&SourceId::Project(path), project.into_global());
        };
        let text = read_capped(&path)?;
        let layer = SourceLayer::parse(SourceId::Project(path), &text)?;
        self.apply_v2_project(&layer, max_bytes)
    }

    fn apply_v2_project(&mut self, layer: &SourceLayer, max_bytes: u64) -> Result<()> {
        if layer.legacy_vector_override().is_some() {
            return Err(invalid(
                "remove project hybrid.cache.max_bytes: v2 uses global vector_cache.max_bytes",
            ));
        }
        self.apply_source(layer)?;
        // Legacy whole-value patches and explicit v2 bool resets can now change
        // only the project's hybrid stages; the shared budget belongs to the global.
        self.settings.hybrid_cfg.cache.max_bytes = max_bytes;
        Ok(())
    }
}

/// Resolves a staged v2 file before an editor publishes it.
pub(crate) fn resolve_v2_edit(layer: &SourceLayer, global: Option<&Config>) -> Result<Config> {
    let context = DefaultsContext::standalone();
    let mut resolution = match global {
        Some(config) => Resolution::<ConfigSpec>::from_provided(&context, config),
        None => Resolution::new(&context),
    };
    if let Some(global) = global {
        let max_bytes = global
            .vector_store
            .as_ref()
            .map(|policy| policy.max_bytes)
            .ok_or_else(|| {
                invalid("v2 project configuration requires global config_version = 2")
            })?;
        resolution.apply_v2_project(layer, max_bytes)?;
    } else {
        resolution.apply_v2_global(layer)?;
    }
    resolution.finish().map(Resolved::into_settings)
}

/// Loads versioned files while retaining each source’s provenance.
///
/// # Errors
/// File errors, invalid settings or project provider records.
pub fn load_from(config_dir: &Path, project_root: Option<&Path>) -> Result<Resolved<Config>> {
    let mut resolution = global_resolution(config_dir)?;
    if let Some(root) = project_root {
        resolution.apply_project(root)?;
    }
    resolution.finish()
}
/// Loads just the global snapshot with real origins.
///
/// # Errors
/// File errors or invalid settings.
pub fn global_from(config_dir: &Path) -> Result<Resolved<Config>> {
    global_resolution(config_dir)?.finish()
}

/// Resolve already captured launch bytes without filesystem access.
pub(crate) fn launch_resolution(
    path: &Path,
    bytes: Option<&[u8]>,
) -> std::result::Result<LaunchResolution, LaunchFailure> {
    let mut resolution = Resolution::<ConfigSpec>::new(&DefaultsContext::standalone());
    let mut website = false;
    if let Some(bytes) = bytes {
        let text = std::str::from_utf8(bytes).map_err(|err| {
            if has_version_header(bytes) {
                return LaunchFailure::Versioned(invalid(
                    "invalid UTF-8 in versioned configuration",
                ));
            }
            LaunchFailure::Legacy(Error::internal_with_source(
                format!("read {}: {err}", path.display()),
                err,
            ))
        })?;
        // Parse the canonical document once. Decode/validate the owner section
        // first so neither its errors nor a recognized Website selector can be
        // erased by the legacy Shared source-error adapter.
        let mut document = super::parse_global_document(path, text).map_err(|error| {
            if has_version_header(bytes) {
                LaunchFailure::Versioned(invalid("invalid versioned configuration TOML"))
            } else {
                LaunchFailure::Legacy(error)
            }
        })?;
        let source = SourceId::Global(path.to_owned());
        if is_v2(&document) {
            let layer = SourceLayer::from_document(source, text, document)
                .map_err(LaunchFailure::Versioned)?;
            resolution
                .apply_v2_global(&layer)
                .map_err(LaunchFailure::Versioned)?;
            return Ok(LaunchResolution(resolution));
        }
        if document.contains_key("config_version") {
            return Err(LaunchFailure::Versioned(invalid(
                "unsupported explicit global configuration version; expected 2",
            )));
        }
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
    } else {
        let policy =
            crate::vector_cache::settings::standalone().map_err(LaunchFailure::Versioned)?;
        resolution
            .bind_vector_policy(&policy)
            .map_err(LaunchFailure::Versioned)?;
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
    // Versioned sources never use the legacy fallback after version recognition.
    Versioned(Error),
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
            Self::Legacy(error)
            | Self::Daemon(error)
            | Self::Remote(error)
            | Self::Versioned(error) => error,
        }
    }
}

// Retain the same once-computed defaults catalog between source validation and
// the CLI patch. Legacy fallback applies only to classified Shared legacy-source
// errors, never daemon owner, recognized Website or CLI errors.
pub(crate) struct LaunchResolution(Resolution<ConfigSpec>);
impl LaunchResolution {
    pub(crate) fn legacy_defaults() -> Self {
        Self(Resolution::new(&DefaultsContext::standalone()))
    }

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
    resolution.apply_project(root)?;
    resolution.finish()
}
pub(super) fn for_provided_project(global: &Config, root: &Path) -> Result<Resolved<Config>> {
    let mut resolution =
        Resolution::<ConfigSpec>::from_provided(&DefaultsContext::standalone(), global);
    resolution.apply_project(root)?;
    resolution.finish()
}

/// Resolves caller-supplied versioned sources for offline review only.
/// A preview confers no permission to publish or apply file changes.
///
/// # Errors
/// Invalid source order, scope, stage settings or unsupported configuration.
pub fn preview_sources(
    global: &super::source::SourceLayer,
    project: Option<&super::source::SourceLayer>,
) -> Result<super::source::SourcePreview> {
    use super::source::{PreviewConflict, SourcePreview, invalid};
    if !matches!(global.source, SourceId::Global(_))
        || project.is_some_and(|p| !matches!(p.source, SourceId::Project(_)))
    {
        return Err(invalid(
            "preview requires a global source followed by a project source",
        ));
    }
    let mut resolution = Resolution::<ConfigSpec>::new(&DefaultsContext::standalone());
    for layer in std::iter::once(global).chain(project) {
        resolution.apply_source(layer)?;
    }
    let legacy_vector_max_bytes = resolution.settings.hybrid_cfg.cache.max_bytes;
    let mut conflicts = Vec::new();
    if let Some(policy) = &global.vector_store {
        if let Some(project) = project
            && let Some(max_bytes) = project.legacy_vector_override()
        {
            conflicts.push(PreviewConflict::LegacyProjectVectorBudget {
                source: project.source.clone(),
                max_bytes,
            });
        }
        resolution.bind_vector_policy(policy)?;
    }
    Ok(SourcePreview {
        effective: resolution.finish()?,
        legacy_vector_max_bytes,
        conflicts,
        runtime_activation_available: false,
    })
}
