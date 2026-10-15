//! Job and search integration helpers (FR-11, FR-15): embed plan chunks,
//! full backfill passes and coverage reporting.
//!
//! The config-resolving wrappers build the stages (`from_config`) and run the
//! cache calls inside [`super::blocking::enter`]; the `*_with` variants take
//! an explicit embedder so tests can run without the network.

use std::path::Path;

use crate::error::Result;
use crate::store::Db;
use crate::store::repo;
use crate::vector_cache::{Embedder, VectorCache};

use super::{HybridCfg, blocking, from_config};

/// Coverage of one project under the configured embedding model (FR-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Chunks with a cached vector for the current model key.
    pub cached: u64,
    /// Chunks in the project registry.
    pub total: u64,
}

/// Embeds `texts` through the configured stage; no-op when the stage is off.
///
/// # Errors
/// Feature/admission errors when hybrid is enabled in an offline build, plus
/// cache and provider failures (failures stay explicit, NFR-2).
pub fn embed_texts(cache_root: &Path, cfg: &HybridCfg, texts: &[String]) -> Result<u64> {
    if !cfg.enabled || cfg.embedding.is_none() || texts.is_empty() {
        return Ok(0);
    }
    let stages = from_config(cfg, cfg!(feature = "hybrid-remote"))?;
    let Some(embedder) = stages.embedder else {
        return Ok(0);
    };
    embed_texts_with(cache_root, embedder.as_ref(), texts)
}

/// [`embed_texts`] with an explicit embedder (tests, future local models).
///
/// # Errors
/// Cache and embedder failures.
pub fn embed_texts_with(
    cache_root: &Path,
    embedder: &dyn Embedder,
    texts: &[String],
) -> Result<u64> {
    if texts.is_empty() {
        return Ok(0);
    }
    let mut cache = VectorCache::open(cache_root)?;
    let vectors = blocking::enter(|| cache.embed_missing(embedder, texts))?;
    Ok(vectors.len() as u64)
}

/// Full backfill pass: embeds every chunk of `project_id` that is missing
/// from the content-addressed cache (unchanged chunks cost nothing).
///
/// # Errors
/// Feature/admission, cache and provider failures.
pub fn embed_project_chunks(db: &Db, cfg: &HybridCfg, project_id: i64) -> Result<u64> {
    if !cfg.enabled || cfg.embedding.is_none() {
        return Ok(0);
    }
    let stages = from_config(cfg, cfg!(feature = "hybrid-remote"))?;
    let Some(embedder) = stages.embedder else {
        return Ok(0);
    };
    embed_project_chunks_with(db, embedder.as_ref(), project_id)
}

/// [`embed_project_chunks`] with an explicit embedder (tests).
///
/// # Errors
/// Registry, cache and embedder failures.
pub fn embed_project_chunks_with(db: &Db, embedder: &dyn Embedder, project_id: i64) -> Result<u64> {
    let texts = repo::chunk_texts(db.connection(), project_id)?;
    embed_texts_with(db.cache_root(), embedder, &texts)
}

/// Cached/total chunk coverage for the configured stage; `None` when the
/// embedding stage is off (status shows nothing to cover).
///
/// # Errors
/// Registry/cache failures; feature errors when enabled in an offline build.
pub fn coverage(db: &Db, cfg: &HybridCfg, project_id: i64) -> Result<Option<Coverage>> {
    if !cfg.enabled {
        return Ok(None);
    }
    let Some(embedding) = &cfg.embedding else {
        return Ok(None);
    };
    // Coverage needs only the cache key, not a provider client: reporting
    // stays cheap and never builds runtimes per status call (NFR-1).
    coverage_for_key(db, &super::config::cache_model_id(embedding), project_id).map(Some)
}

/// [`coverage`] with an explicit embedder (tests).
///
/// # Errors
/// Registry and cache failures.
pub fn coverage_with(db: &Db, embedder: &dyn Embedder, project_id: i64) -> Result<Coverage> {
    coverage_for_key(db, embedder.model_id(), project_id)
}

/// Coverage for one cache key. The cache is content-addressed, so coverage
/// counts *distinct* chunk hashes: duplicated chunk texts share one vector
/// and must not keep coverage below 100% forever (FR-15).
///
/// # Errors
/// Registry and cache failures.
pub fn coverage_for_key(db: &Db, model_id: &str, project_id: i64) -> Result<Coverage> {
    let texts = repo::chunk_texts(db.connection(), project_id)?;
    let hashes: std::collections::HashSet<String> = texts
        .iter()
        .map(|text| crate::vector_cache::chunk_hash(text))
        .collect();
    let total = hashes.len() as u64;
    if total == 0 {
        return Ok(Coverage {
            cached: 0,
            total: 0,
        });
    }
    let unique: Vec<String> = hashes.into_iter().collect();
    let cache = VectorCache::open(db.cache_root())?;
    let cached = cache.count_present(model_id, &unique)?;
    Ok(Coverage { cached, total })
}
