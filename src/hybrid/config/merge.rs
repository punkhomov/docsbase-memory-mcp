//! Pure v2 hybrid overlays. The ordinary v1 loader does not use these patches.
use serde::Deserialize;

use super::{
    EmbeddingPatch, HybridCfg, HybridDefaults, HybridPatch, HybridSpec, Ranking, RankingPatch,
    RerankerPatch, resolve_embedding, resolve_reranker,
};
use crate::config::schema::{Provenance, SettingsSpec, SourceId};
use crate::error::{Error, Result};

/// Version-two hybrid input: reset shorthand or field-wise table.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HybridMergePatch {
    /// Same whole-value reset as version one.
    Bool(bool),
    /// Only supplied fields change.
    Table(HybridMergeTable),
}
/// Optional stage state; an omitted field inherits the current stage.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StagePatch<T> {
    /// Only false is valid; enabling uses a table with optional fields.
    Disable(bool),
    /// Enable or update the stage using owner resolution.
    Table(T),
}
/// Shared vector resources are deliberately absent from this project patch.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HybridMergeTable {
    /// Master switch, inherited when absent.
    pub enabled: Option<bool>,
    /// Embedding update or explicit disable.
    pub embedding: Option<StagePatch<EmbeddingPatch>>,
    /// Reranker update or explicit disable.
    pub reranker: Option<StagePatch<RerankerPatch>>,
    /// Candidate ordering, inherited when absent.
    pub ranking: Option<RankingPatch>,
}

pub(crate) fn overlay(
    value: &mut HybridCfg,
    patch: HybridMergePatch,
    defaults: &HybridDefaults,
    source: &SourceId,
    origins: &mut Provenance,
) -> Result<()> {
    let table = match patch {
        HybridMergePatch::Bool(enabled) => {
            return HybridSpec::overlay(
                value,
                HybridPatch::Bool(enabled),
                defaults,
                source,
                origins,
            );
        }
        HybridMergePatch::Table(table) => table,
    };
    if let Some(enabled) = table.enabled {
        value.enabled = enabled;
        origins.set("hybrid.enabled", source)?;
    }
    if let Some(patch) = table.embedding {
        overlay_embedding(value, patch, defaults, source, origins)?;
    }
    if let Some(patch) = table.reranker {
        overlay_reranker(value, patch, defaults, source, origins)?;
    }
    if let Some(ranking) = table.ranking {
        value.ranking = match ranking {
            RankingPatch::Lexical => Ranking::Lexical,
            RankingPatch::Rrf => Ranking::Rrf,
        };
        origins.set("hybrid.ranking", source)?;
    }
    Ok(())
}
fn stage_enable_error() -> Error {
    Error::Admission {
        message: "v2 stage accepts false or a table; use an empty table to enable".to_owned(),
    }
}

fn overlay_embedding(
    value: &mut HybridCfg,
    patch: StagePatch<EmbeddingPatch>,
    defaults: &HybridDefaults,
    source: &SourceId,
    origins: &mut Provenance,
) -> Result<()> {
    match patch {
        StagePatch::Disable(false) => {
            value.embedding = None;
            origins.replace_group("hybrid.embedding.", source)?;
        }
        StagePatch::Disable(true) => return Err(stage_enable_error()),
        StagePatch::Table(patch) => {
            let previous = value.embedding.as_ref();
            let provider = patch
                .provider
                .as_deref()
                .or_else(|| previous.map(|v| v.provider.as_str()))
                .unwrap_or(&defaults.embedding.provider);
            let model = patch
                .model
                .as_deref()
                .or_else(|| previous.map(|v| v.model.as_str()))
                .or_else(|| {
                    (provider == defaults.embedding.provider)
                        .then_some(defaults.embedding.model.as_str())
                });
            let dimensions = patch
                .dimensions
                .or_else(|| previous.map(|v| v.dimensions))
                .or_else(|| {
                    (provider == defaults.embedding.provider)
                        .then_some(defaults.embedding.dimensions)
                });
            let normalize = patch
                .normalize
                .or_else(|| previous.map(|v| v.normalize))
                .unwrap_or(defaults.embedding.normalize);
            let next = resolve_embedding(provider, model, dimensions, normalize)?;
            if previous.is_none() {
                origins.replace_group("hybrid.embedding.", source)?;
            } else {
                for (key, supplied) in [
                    ("hybrid.embedding.provider", patch.provider.is_some()),
                    ("hybrid.embedding.model", patch.model.is_some()),
                    ("hybrid.embedding.dimensions", patch.dimensions.is_some()),
                    ("hybrid.embedding.normalize", patch.normalize.is_some()),
                ] {
                    if supplied {
                        origins.set(key, source)?;
                    }
                }
            }
            value.embedding = Some(next);
        }
    }
    Ok(())
}

fn overlay_reranker(
    value: &mut HybridCfg,
    patch: StagePatch<RerankerPatch>,
    defaults: &HybridDefaults,
    source: &SourceId,
    origins: &mut Provenance,
) -> Result<()> {
    match patch {
        StagePatch::Disable(false) => {
            value.reranker = None;
            origins.replace_group("hybrid.reranker.", source)?;
        }
        StagePatch::Disable(true) => return Err(stage_enable_error()),
        StagePatch::Table(patch) => {
            let previous = value.reranker.as_ref();
            let provider = patch
                .provider
                .as_deref()
                .or_else(|| previous.map(|v| v.provider.as_str()))
                .unwrap_or(&defaults.reranker.provider);
            let model = patch
                .model
                .as_deref()
                .or_else(|| previous.map(|v| v.model.as_str()))
                .or_else(|| {
                    (provider == defaults.reranker.provider)
                        .then_some(defaults.reranker.model.as_str())
                });
            let window = patch
                .window
                .or_else(|| previous.map(|v| v.window))
                .unwrap_or(defaults.reranker.window);
            let next = resolve_reranker(provider, model, window)?;
            if previous.is_none() {
                origins.replace_group("hybrid.reranker.", source)?;
            } else {
                for (key, supplied) in [
                    ("hybrid.reranker.provider", patch.provider.is_some()),
                    ("hybrid.reranker.model", patch.model.is_some()),
                    ("hybrid.reranker.window", patch.window.is_some()),
                ] {
                    if supplied {
                        origins.set(key, source)?;
                    }
                }
            }
            value.reranker = Some(next);
        }
    }
    Ok(())
}
