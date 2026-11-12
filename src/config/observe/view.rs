//! Safe projection: no original/proposed TOML or arbitrary configuration values.
use std::io::{self, Write};

use super::source::{SourceObservation, revision};
use crate::config::change::ChangePlan;
use crate::config::migrate::{MigrationDraft, MigrationWarning, VectorBudgetDecision};
use crate::config::schema::SourceId;
use crate::config::source::invalid;
use crate::error::Result;
use crate::store::registry_inspect::RegistryObservation;
use serde::Serialize;

/// Bounded, source-only plan. It is neither execution consent nor runtime state.
#[derive(Debug, Serialize)]
pub struct MigrationPlan {
    pub(super) observation_version: u32,
    pub(super) observation_id: String,
    pub(super) config_root: std::path::PathBuf,
    pub(super) registry: RegistryObservation,
    pub(super) registry_stable: bool,
    pub(super) sources: Vec<SourceObservation>,
    pub(super) decision: DecisionView,
    pub(super) summary: Option<Summary>,
    pub(super) plan: ChangePlan,
    pub(super) atomic_snapshot: bool,
    pub(super) runtime_activation_available: bool,
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(super) enum DecisionView {
    None,
    UseBytes(u64),
    AcceptSuggestedMaximum,
}
impl From<Option<VectorBudgetDecision>> for DecisionView {
    fn from(decision: Option<VectorBudgetDecision>) -> Self {
        match decision {
            None => Self::None,
            Some(VectorBudgetDecision::UseBytes(n)) => Self::UseBytes(n),
            Some(VectorBudgetDecision::AcceptSuggestedMaximum) => Self::AcceptSuggestedMaximum,
        }
    }
}
#[derive(Debug, Serialize)]
pub(super) struct Summary {
    suggested_vector_max_bytes: u64,
    selected_vector_max_bytes: Option<u64>,
    budgets: Vec<Budget>,
    scope_conflicts: Vec<ScopeConflict>,
    rewrites: Vec<Rewrite>,
    canonical_formatting: bool,
    materialized_hybrid_defaults: Vec<SourceId>,
    unregistered_projects_require_admission: bool,
}
#[derive(Debug, Serialize)]
struct Budget {
    source: SourceId,
    previous: u64,
    proposed: Option<u64>,
}
#[derive(Debug, Serialize)]
struct ScopeConflict {
    source: SourceId,
    legacy_vector_max_bytes: u64,
}
#[derive(Debug, Serialize)]
struct Rewrite {
    source: SourceId,
    target_version: u32,
    proposed_revision: String,
}
impl Summary {
    pub(super) fn from_draft(draft: &MigrationDraft) -> Self {
        Self {
            suggested_vector_max_bytes: draft.suggested_vector_max_bytes,
            selected_vector_max_bytes: draft.selected_vector_max_bytes,
            budgets: draft
                .budgets
                .iter()
                .map(|budget| Budget {
                    source: budget.source.clone(),
                    previous: budget.max_bytes,
                    proposed: draft.selected_vector_max_bytes,
                })
                .collect(),
            scope_conflicts: draft
                .scope_conflicts
                .iter()
                .map(|conflict| match conflict {
                    crate::config::source::PreviewConflict::LegacyProjectVectorBudget {
                        source,
                        max_bytes,
                    } => ScopeConflict {
                        source: source.clone(),
                        legacy_vector_max_bytes: *max_bytes,
                    },
                })
                .collect(),
            rewrites: draft
                .edits
                .iter()
                .map(|edit| Rewrite {
                    source: edit.source.clone(),
                    target_version: 2,
                    proposed_revision: revision(&edit.after),
                })
                .collect(),
            canonical_formatting: draft
                .warnings
                .contains(&MigrationWarning::CanonicalFormatting),
            materialized_hybrid_defaults: draft
                .warnings
                .iter()
                .filter_map(|warning| match warning {
                    MigrationWarning::MaterializedHybridDefaults(source) => Some(source.clone()),
                    _ => None,
                })
                .collect(),
            unregistered_projects_require_admission: true,
        }
    }
}
impl MigrationPlan {
    /// Serialize with an explicit limit on retained encoded bytes (not allocator RSS).
    ///
    /// # Errors
    /// The complete JSON does not fit the limit or serialization fails.
    pub fn to_json(&self, max_bytes: usize) -> Result<String> {
        json(self, max_bytes)
    }
    pub(super) fn identify(mut self, max_bytes: usize) -> Result<Self> {
        // The identity field is empty in the canonical identity payload.
        let bytes = json(&self, max_bytes)?;
        let mut hash = blake3::Hasher::new_derive_key("docsbase.observed-migration.v1");
        hash.update(bytes.as_bytes());
        self.observation_id = format!("observed-migration-v1:{}", hash.finalize().to_hex());
        self.to_json(max_bytes)?;
        Ok(self)
    }
}
fn json(value: &impl Serialize, max_bytes: usize) -> Result<String> {
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit: max_bytes,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| invalid("migration output byte limit exceeded or serialization failed"))?;
    String::from_utf8(writer.bytes).map_err(|_| invalid("invalid migration output encoding"))
}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("migration output limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
