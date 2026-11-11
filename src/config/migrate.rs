//! Pure v1/v2 source migration proposals. This module never reads or writes files.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::Config;
use super::resolve::preview_sources;
use super::schema::SourceId;
use super::source::{ConfigVersion, PreviewConflict, SourceLayer, SourcePreview, invalid};
use crate::error::Result;
use crate::hybrid::config::{HybridCfg, Ranking};

/// Explicit observation supplied by a caller that inventories registered projects.
#[derive(Debug, Clone)]
pub enum ProjectObservation {
    /// The file was inspected and its contents are available.
    Loaded(Box<SourceLayer>),
    /// The file was inspected and absent; this is not an uninspected project.
    Missing(PathBuf),
}
impl ProjectObservation {
    fn path(&self) -> Result<&Path> {
        match self {
            Self::Loaded(layer) if matches!(layer.source, SourceId::Project(_)) => layer.path(),
            Self::Missing(path) => Ok(path),
            Self::Loaded(_) => Err(invalid("project inventory contains a non-project source")),
        }
    }
    fn layer(&self) -> Option<&SourceLayer> {
        match self {
            Self::Loaded(layer) => Some(layer),
            Self::Missing(_) => None,
        }
    }
}
/// A source-planning choice, not consent to apply a runtime or storage change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorBudgetDecision {
    /// Explicit new global capacity.
    UseBytes(u64),
    /// Explicitly accept the maximum observed effective legacy capacity.
    AcceptSuggestedMaximum,
}
/// A reason a complete source proposal cannot yet be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationBlocker {
    /// Registered project has no loaded/missing observation.
    UninspectedProject(PathBuf),
    /// A proposed shared capacity has not been explicitly selected.
    VectorBudgetDecisionRequired,
}
/// Observed capacity: v1 effective value, v2 global cap, or explicit legacy conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetObservation {
    /// File scope whose effective value was observed.
    pub source: SourceId,
    /// Capacity relevant to the source survey; v2 resets retain the global cap.
    pub max_bytes: u64,
}
/// An intentional resource change that must remain visible to an operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetChange {
    /// Affected source scope.
    pub source: SourceId,
    /// Previously observed capacity (including explicit legacy conflicts).
    pub previous: u64,
    /// Selected shared capacity.
    pub proposed: u64,
}
/// Source-only caveats shown alongside a proposed rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationWarning {
    /// TOML is canonicalized; original comments/formatting are not preserved.
    CanonicalFormatting,
    /// Legacy table replacement requires explicit current resolved values.
    MaterializedHybridDefaults(SourceId),
    /// The supplied registry is the survey boundary; future projects need admission.
    UnregisteredProjectsRequireAdmission,
}
/// Reviewable replacement and stale-source precondition. Debug omits file text.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceEdit {
    /// Exact source identity supplied by the caller.
    pub source: SourceId,
    /// BLAKE3 of the exact original UTF-8 source bytes.
    pub expected_source_hash: String,
    /// Original source for an explicit diff view.
    pub before: String,
    /// Proposed canonical TOML, never written by this module.
    pub after: String,
}
impl fmt::Debug for SourceEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceEdit")
            .field("source", &self.source)
            .field("expected_source_hash", &self.expected_source_hash)
            .finish_non_exhaustive()
    }
}
/// A versioned pure source proposal. It makes no artifact-readiness claim.
#[derive(Debug, Clone)]
pub struct MigrationDraft {
    /// Version of this planning DTO, independent from the target config version.
    pub draft_version: u32,
    /// Known scope/budget facts before migration.
    pub budgets: Vec<BudgetObservation>,
    /// Maximum of observed budgets; incomplete inventory cannot approve it.
    pub suggested_vector_max_bytes: u64,
    /// Explicitly selected capacity, if any.
    pub selected_vector_max_bytes: Option<u64>,
    /// Resource changes, not silently hidden by source equivalence checking.
    pub budget_changes: Vec<BudgetChange>,
    /// Legacy project budget conflicts found under a v2 global source.
    pub scope_conflicts: Vec<PreviewConflict>,
    /// Missing observations or explicit choices.
    pub blockers: Vec<MigrationBlocker>,
    /// Canonicalization/materialization and survey scope caveats.
    pub warnings: Vec<MigrationWarning>,
    /// Empty until the survey and explicit choice are complete.
    pub edits: Vec<SourceEdit>,
    /// False: runtime consent and strict shared-store ownership are later tasks.
    pub runtime_activation_available: bool,
}
impl MigrationDraft {
    /// Whether all source inputs/choices are present; not permission to apply.
    #[must_use]
    pub fn source_proposal_complete(&self) -> bool {
        self.blockers.is_empty()
    }
}
struct Surveyed<'a> {
    source: SourceId,
    layer: Option<&'a SourceLayer>,
    preview: SourcePreview,
}

/// Builds a deterministic source proposal from an explicit registry survey.
/// No discovery, filesystem access, provider construction or mutation occurs.
///
/// # Errors
/// Invalid sources/inventory, unrepresentable budget or non-equivalent proposal.
#[allow(clippy::too_many_lines)] // one ordered pure survey/choice/roundtrip transaction
pub fn draft_migration(
    global: &SourceLayer,
    registered_projects: &[PathBuf],
    observations: &[ProjectObservation],
    decision: Option<VectorBudgetDecision>,
) -> Result<MigrationDraft> {
    let registry: BTreeSet<_> = registered_projects.iter().cloned().collect();
    if registry.len() != registered_projects.len() {
        return Err(invalid("duplicate registered project source"));
    }
    let mut seen = BTreeMap::new();
    for observation in observations {
        let path = observation.path()?.to_owned();
        if !registry.contains(&path) {
            return Err(invalid("observation is outside the supplied registry"));
        }
        if seen.insert(path, observation).is_some() {
            return Err(invalid("duplicate project observation"));
        }
    }
    let global_preview = preview_sources(global, None)?;
    let mut draft = MigrationDraft {
        draft_version: 1,
        budgets: vec![BudgetObservation {
            source: global.source.clone(),
            max_bytes: global_preview
                .effective
                .settings()
                .hybrid_cfg
                .cache
                .max_bytes,
        }],
        suggested_vector_max_bytes: global_preview
            .effective
            .settings()
            .hybrid_cfg
            .cache
            .max_bytes,
        selected_vector_max_bytes: None,
        budget_changes: Vec::new(),
        scope_conflicts: Vec::new(),
        blockers: Vec::new(),
        warnings: vec![MigrationWarning::UnregisteredProjectsRequireAdmission],
        edits: Vec::new(),
        runtime_activation_available: false,
    };
    let mut surveyed = Vec::new();
    for path in registry {
        let Some(observation) = seen.get(&path) else {
            draft
                .blockers
                .push(MigrationBlocker::UninspectedProject(path));
            continue;
        };
        let preview = preview_sources(global, observation.layer())?;
        let source = SourceId::Project(path);
        // A strict v2 global already owns the capacity. Only an explicit v1
        // project override contributes a conflicting legacy value to the survey;
        // bool/table resets must not resurrect an unrelated old default budget.
        let observed_budget = if global.version == ConfigVersion::V2 {
            observation
                .layer()
                .and_then(SourceLayer::legacy_vector_override)
                .unwrap_or(preview.effective.settings().hybrid_cfg.cache.max_bytes)
        } else {
            preview.legacy_vector_max_bytes
        };
        draft.budgets.push(BudgetObservation {
            source: source.clone(),
            max_bytes: observed_budget,
        });
        draft.suggested_vector_max_bytes = draft.suggested_vector_max_bytes.max(observed_budget);
        draft
            .scope_conflicts
            .extend(preview.conflicts.iter().cloned());
        surveyed.push(Surveyed {
            source,
            layer: observation.layer(),
            preview,
        });
    }
    let selected = match decision {
        Some(VectorBudgetDecision::UseBytes(value)) => Some(value),
        Some(VectorBudgetDecision::AcceptSuggestedMaximum) => {
            Some(draft.suggested_vector_max_bytes)
        }
        None => {
            draft
                .blockers
                .push(MigrationBlocker::VectorBudgetDecisionRequired);
            None
        }
    };
    if let Some(value) = selected {
        if value == 0 || i64::try_from(value).is_err() {
            return Err(invalid(
                "selected vector budget must be positive and representable as a TOML integer",
            ));
        }
        draft.selected_vector_max_bytes = Some(value);
        draft.budget_changes = draft
            .budgets
            .iter()
            .filter(|row| row.max_bytes != value)
            .map(|row| BudgetChange {
                source: row.source.clone(),
                previous: row.max_bytes,
                proposed: value,
            })
            .collect();
    }
    if !draft.blockers.is_empty() {
        return Ok(draft);
    }
    let cap =
        selected.ok_or_else(|| invalid("complete migration draft needs an explicit budget"))?;
    let proposed_global = propose_source(
        global,
        global_preview.effective.settings(),
        Some(cap),
        &mut draft,
    )?;
    let after_global = preview_sources(&proposed_global, None)?;
    verify_equivalent(
        global_preview.effective.settings(),
        after_global.effective.settings(),
        cap,
    )?;
    for project in surveyed {
        let proposed = project
            .layer
            .map(|layer| {
                propose_source(
                    layer,
                    project.preview.effective.settings(),
                    None,
                    &mut draft,
                )
            })
            .transpose()?;
        let after = preview_sources(&proposed_global, proposed.as_ref())?;
        if !after.conflicts.is_empty() {
            return Err(invalid(
                "proposed migration retains a legacy vector scope conflict",
            ));
        }
        verify_equivalent(
            project.preview.effective.settings(),
            after.effective.settings(),
            cap,
        )?;
        // A missing project is observed, not synthesized into a configuration file.
        if project.layer.is_none() && draft.edits.iter().any(|edit| edit.source == project.source) {
            return Err(invalid(
                "migration unexpectedly created a missing project source",
            ));
        }
    }
    if !draft.edits.is_empty() {
        draft
            .warnings
            .insert(0, MigrationWarning::CanonicalFormatting);
    }
    Ok(draft)
}

fn verify_equivalent(before: &Config, after: &Config, cap: u64) -> Result<()> {
    let mut expected = before.clone();
    expected.hybrid_cfg.cache.max_bytes = cap;
    if &expected != after {
        return Err(invalid(
            "proposed source migration changes non-resource effective settings",
        ));
    }
    Ok(())
}
fn propose_source(
    layer: &SourceLayer,
    effective: &Config,
    global_cap: Option<u64>,
    draft: &mut MigrationDraft,
) -> Result<SourceLayer> {
    let mut document = layer.document.clone();
    document.insert("config_version".into(), toml::Value::Integer(2));
    if layer.version == ConfigVersion::V1
        && document.get("hybrid").is_some_and(toml::Value::is_table)
    {
        document.insert("hybrid".into(), hybrid_table(&effective.hybrid_cfg));
        draft
            .warnings
            .push(MigrationWarning::MaterializedHybridDefaults(
                layer.source.clone(),
            ));
    }
    if let Some(cap) = global_cap {
        let mut table = toml::Table::new();
        table.insert(
            "max_bytes".into(),
            toml::Value::Integer(
                i64::try_from(cap).map_err(|_| invalid("unrepresentable vector capacity"))?,
            ),
        );
        document.insert("vector_cache".into(), toml::Value::Table(table));
    }
    let after = toml::to_string_pretty(&document)
        .map_err(|_| invalid("cannot serialize proposed configuration"))?;
    let proposed = SourceLayer::parse(layer.source.clone(), &after)?;
    if layer.original != after {
        draft.edits.push(SourceEdit {
            source: layer.source.clone(),
            expected_source_hash: blake3::hash(layer.original.as_bytes()).to_hex().to_string(),
            before: layer.original.clone(),
            after,
        });
    }
    Ok(proposed)
}
fn hybrid_table(config: &HybridCfg) -> toml::Value {
    let mut table = toml::Table::new();
    table.insert("enabled".into(), config.enabled.into());
    table.insert(
        "ranking".into(),
        match config.ranking {
            Ranking::Lexical => "lexical",
            Ranking::Rrf => "rrf",
        }
        .into(),
    );
    table.insert(
        "embedding".into(),
        config
            .embedding
            .as_ref()
            .map_or(toml::Value::Boolean(false), |stage| {
                let mut t = toml::Table::new();
                t.insert("provider".into(), stage.provider.clone().into());
                t.insert("model".into(), stage.model.clone().into());
                t.insert("dimensions".into(), i64::from(stage.dimensions).into());
                t.insert("normalize".into(), stage.normalize.into());
                toml::Value::Table(t)
            }),
    );
    table.insert(
        "reranker".into(),
        config
            .reranker
            .as_ref()
            .map_or(toml::Value::Boolean(false), |stage| {
                let mut t = toml::Table::new();
                t.insert("provider".into(), stage.provider.clone().into());
                t.insert("model".into(), stage.model.clone().into());
                t.insert("window".into(), i64::from(stage.window).into());
                toml::Value::Table(t)
            }),
    );
    toml::Value::Table(table)
}
