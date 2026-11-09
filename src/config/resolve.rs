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

use super::{Config, ConfigSpec, GlobalFileConfig, read_global, read_project};

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
        resolution.apply(
            &SourceId::Project(path),
            GlobalFileConfig {
                ignores: project.ignores,
                max_file_size: project.max_file_size,
                max_docs_per_project: project.max_docs_per_project,
                auto_index: project.auto_index,
                hybrid: project.hybrid,
                providers: None,
            },
        )?;
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
