//! Owner projection of hybrid settings; never constructs a stage.
use serde_json::json;

use super::HybridCfg;
use crate::config::view::{ViewBuilder, capability};
use crate::error::Result;

pub(crate) fn fields(settings: &HybridCfg, view: &mut ViewBuilder<'_>) -> Result<()> {
    view.push("hybrid.enabled", json!(settings.enabled), None)?;
    let embedding = settings.embedding.as_ref();
    let cap = embedding
        .map(|stage| capability(&stage.provider))
        .transpose()?;
    for (key, value) in [
        (
            "hybrid.embedding.provider",
            json!(embedding.map(|s| &s.provider)),
        ),
        ("hybrid.embedding.model", json!(embedding.map(|s| &s.model))),
        (
            "hybrid.embedding.dimensions",
            json!(embedding.map(|s| s.dimensions)),
        ),
        (
            "hybrid.embedding.normalize",
            json!(embedding.map(|s| s.normalize)),
        ),
    ] {
        view.push(key, value, cap.clone())?;
    }
    let reranker = settings.reranker.as_ref();
    let cap = reranker
        .map(|stage| capability(&stage.provider))
        .transpose()?;
    for (key, value) in [
        (
            "hybrid.reranker.provider",
            json!(reranker.map(|s| &s.provider)),
        ),
        ("hybrid.reranker.model", json!(reranker.map(|s| &s.model))),
        ("hybrid.reranker.window", json!(reranker.map(|s| s.window))),
    ] {
        view.push(key, value, cap.clone())?;
    }
    view.push(
        "hybrid.ranking",
        json!(match settings.ranking {
            super::Ranking::Lexical => "lexical",
            super::Ranking::Rrf => "rrf",
        }),
        None,
    )?;
    let key = if view.has_field(crate::vector_cache::V2_MAX_BYTES_FIELD.key) {
        crate::vector_cache::V2_MAX_BYTES_FIELD.key
    } else {
        "hybrid.cache.max_bytes"
    };
    view.push(key, json!(settings.cache.max_bytes), None)
}
