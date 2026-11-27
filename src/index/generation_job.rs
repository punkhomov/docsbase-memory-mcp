//! Shared preparation for admitted lexical index jobs against an installed v5 catalog.

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::config::schema::Resolved;
use crate::error::Result;
use crate::index::job::CaptureBudget;
use crate::search::operation::SearchOperation;
use crate::store::generation_build::{
    BuildInputs, LexicalBuildRequest, OperationalBuild, PreparedLexicalPublication,
    UnchangedGeneration, observe_replacement_base, prepare_operational_lexical_generation,
    reuse_unchanged_generation,
};
use crate::store::{
    Db, models::Project, registry_inspect::ObservedProjectIndex, settings::DbSettings,
};

pub(crate) struct GenerationJob<'a> {
    pub(crate) cache: &'a Path,
    pub(crate) project: &'a Project,
    pub(crate) epoch: [u8; 16],
    pub(crate) settings: &'a Resolved<DbSettings>,
    pub(crate) config: &'a Arc<Config>,
}

impl GenerationJob<'_> {
    pub(crate) fn stage(
        &self,
        capture: &mut CaptureBudget<'_>,
        reviewed_base: Option<ObservedProjectIndex>,
    ) -> Result<PreparedLexicalPublication> {
        let limits = crate::config::observe::PlanningLimits::standard().registry;
        let project_id = self.project.id;
        let (base, replacement) = if let Some(base) = reviewed_base {
            (Some(base), None)
        } else {
            observe_replacement_base(self.cache, project_id, self.epoch, self.settings, limits)?
        };
        prepare_operational_lexical_generation(
            self.cache,
            OperationalBuild {
                request: self.request(base),
                replacement,
            },
            self.settings,
            limits,
            Arc::clone(self.config),
            capture,
        )
    }

    pub(crate) fn reuse_unchanged<T>(
        &self,
        operation: &SearchOperation,
        finish: impl FnOnce(&Db, UnchangedGeneration) -> Result<T>,
    ) -> Result<Option<T>> {
        reuse_unchanged_generation(
            self.cache,
            self.project,
            BuildInputs {
                request: self.request(None),
                settings: self.settings,
                limits: crate::config::observe::PlanningLimits::standard().registry,
                config: self.config,
            },
            operation,
            finish,
        )
    }

    fn request(&self, base: Option<ObservedProjectIndex>) -> LexicalBuildRequest {
        // Direct Index, daemon Sync, and reclamation share this ownership key.
        let mut plan = blake3::Hasher::new_derive_key("docsbase.index.direct.lexical.v1");
        plan.update(&self.epoch);
        plan.update(&self.project.id.to_le_bytes());
        LexicalBuildRequest {
            project_id: self.project.id,
            epoch: self.epoch,
            base,
            plan_digest: *plan.finalize().as_bytes(),
            reserved_disk_bytes: self.config.index_writer.staging_disk_bytes(),
        }
    }
}
