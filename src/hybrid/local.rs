//! Local `fastembed` embedder/reranker (ADR-17, spec hybrid-local).
//!
//! The real implementations land in spec tasks T4 (embedder) and T5
//! (reranker). This module exists from T3 so the provisioning checks in
//! `from_config` can fail fast with connect/fetch hints (SC-3, FR-12)
//! before the inference path exists; the constructors below are the
//! documented seam the tasks fill in.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::vector_cache::Embedder;

use super::Reranker;
use super::config::{EmbeddingCfg, ProvidersCfg, RerankerCfg};

/// Builds the local embedding stage. Not yet implemented (T4).
///
/// # Errors
/// Returns [`Error::Internal`] until T4 lands the fastembed embedder.
pub fn embedder(_cfg: &EmbeddingCfg, _providers: &ProvidersCfg) -> Result<Arc<dyn Embedder>> {
    Err(Error::internal(
        "the fastembed embedder is not yet implemented (spec task T4)",
    ))
}

/// Builds the local rerank stage. Not yet implemented (T5).
///
/// # Errors
/// Returns [`Error::Internal`] until T5 lands the fastembed reranker.
pub fn reranker(_cfg: &RerankerCfg, _providers: &ProvidersCfg) -> Result<Arc<dyn Reranker>> {
    Err(Error::internal(
        "the fastembed reranker is not yet implemented (spec task T5)",
    ))
}
