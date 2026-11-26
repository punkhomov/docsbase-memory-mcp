//! Shared preparation for admitted lexical index jobs against an installed v5 catalog.

use std::path::Path;

use crate::config::Config;
use crate::config::schema::Resolved;
use crate::error::Result;
use crate::index::job::{CaptureBudget, JobStats};
use crate::store::generation_build::{
    LexicalBuildRequest, OperationalBuild, PreparedLexicalPublication, observe_replacement_base,
    with_prepared_lexical_generation,
};
use crate::store::{models::Project, settings::DbSettings};

pub(crate) struct GenerationJob<'a> {
    pub(crate) cache: &'a Path,
    pub(crate) project: &'a Project,
    pub(crate) epoch: [u8; 16],
    pub(crate) settings: &'a Resolved<DbSettings>,
    pub(crate) config: &'a Config,
}

impl GenerationJob<'_> {
    pub(crate) fn prepare(
        &self,
        capture: &mut CaptureBudget<'_>,
        finalize: impl FnOnce(PreparedLexicalPublication<'_>) -> Result<JobStats>,
    ) -> Result<JobStats> {
        let limits = crate::config::observe::PlanningLimits::standard().registry;
        let project_id = self.project.id;
        let (base, interrupted) =
            observe_replacement_base(self.cache, project_id, self.epoch, self.settings, limits)?;
        // Direct index and daemon Sync own the same project's lexical staging.
        // The legacy key retains ownership across upgrades. The staged receipt
        // and CAS bind the base and the validated effective index recipe.
        let mut plan = blake3::Hasher::new_derive_key("docsbase.index.direct.lexical.v1");
        plan.update(&self.epoch);
        plan.update(&project_id.to_le_bytes());
        let request = LexicalBuildRequest {
            project_id,
            epoch: self.epoch,
            base,
            plan_digest: *plan.finalize().as_bytes(),
            reserved_disk_bytes: self.config.index_writer.staging_disk_bytes(),
        };
        with_prepared_lexical_generation(
            self.cache,
            OperationalBuild {
                request,
                interrupted,
            },
            self.settings,
            limits,
            self.config,
            capture,
            finalize,
        )
    }
}
