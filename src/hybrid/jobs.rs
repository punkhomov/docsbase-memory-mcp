//! Job and search integration helpers (FR-11, FR-15): embed plan chunks,
//! full backfill passes and coverage reporting.
//!
//! The config-resolving wrappers build the stages (`from_config`) and run the
//! cache calls inside [`super::blocking::enter`]; the `*_with` variants take
//! an explicit embedder so tests can run without the network.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use crate::error::Result;
use crate::store::Db;
use crate::store::repo;
use crate::vector_cache::{CacheLimits, Embedder, VectorCache};

use super::ann::ANN_MIN_VECTORS;
use super::config::Ranking;
use super::dense::{DenseCacheLimits, ann_build_admitted_for_model};
use super::{HybridCfg, blocking, from_config};

/// Backfill lease duration: covers a remote 75k backfill + ANN build/retry.
const BACKFILL_PIN_TTL: Duration = Duration::from_secs(3_600);

/// Debug-only override of the resident ANN cap used by the lease gate, so a
/// test can exercise the admission boundary without a 92k-chunk corpus.
#[cfg(debug_assertions)]
static TEST_ANN_MAX_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test-only hook; `0` restores the default cap. Absent in release builds.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_test_ann_max_bytes(max_bytes: u64) {
    TEST_ANN_MAX_BYTES.store(max_bytes, std::sync::atomic::Ordering::Relaxed);
}

fn ann_max_bytes() -> u64 {
    #[cfg(debug_assertions)]
    {
        let override_bytes = TEST_ANN_MAX_BYTES.load(std::sync::atomic::Ordering::Relaxed);
        if override_bytes > 0 {
            return override_bytes;
        }
    }
    DenseCacheLimits::default().max_bytes
}

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
pub fn embed_texts(
    cache_root: &Path,
    cfg: &HybridCfg,
    providers: &crate::hybrid::config::ProvidersCfg,
    texts: &[String],
) -> Result<u64> {
    if !cfg.enabled || cfg.embedding.is_none() || texts.is_empty() {
        return Ok(0);
    }
    let stages = from_config(cfg, providers)?;
    let Some(embedder) = stages.embedder else {
        return Ok(0);
    };
    embed_texts_with_limits(cache_root, embedder.as_ref(), texts, cfg.cache.limits())
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
    embed_texts_with_limits(cache_root, embedder, texts, CacheLimits::default())
}

/// [`embed_texts_with`] with explicit vector-cache limits.
///
/// # Errors
/// Cache and embedder failures.
pub fn embed_texts_with_limits(
    cache_root: &Path,
    embedder: &dyn Embedder,
    texts: &[String],
    limits: CacheLimits,
) -> Result<u64> {
    if texts.is_empty() {
        return Ok(0);
    }
    let mut cache = VectorCache::open_with_limits(cache_root, limits)?;
    let vectors = blocking::enter(|| cache.embed_missing(embedder, texts))?;
    Ok(vectors.len() as u64)
}

/// Full backfill pass: embeds every chunk of `project_id` that is missing
/// from the content-addressed cache (unchanged chunks cost nothing).
///
/// # Errors
/// Feature/admission, cache and provider failures.
pub fn embed_project_chunks(
    db: &Db,
    cfg: &HybridCfg,
    providers: &crate::hybrid::config::ProvidersCfg,
    project_id: i64,
) -> Result<u64> {
    if !cfg.enabled || cfg.embedding.is_none() {
        return Ok(0);
    }
    let stages = from_config(cfg, providers)?;
    let Some(embedder) = stages.embedder else {
        return Ok(0);
    };
    embed_project_chunks_with_limits_and_pin(
        db,
        embedder.as_ref(),
        project_id,
        cfg.cache.limits(),
        cfg.enabled && cfg.ranking == Ranking::Rrf,
    )
}

/// [`embed_project_chunks`] with an explicit embedder (tests).
///
/// # Errors
/// Registry, cache and embedder failures.
pub fn embed_project_chunks_with(db: &Db, embedder: &dyn Embedder, project_id: i64) -> Result<u64> {
    embed_project_chunks_with_limits(db, embedder, project_id, CacheLimits::default())
}

/// [`embed_project_chunks_with`] with explicit vector-cache limits.
///
/// # Errors
/// Registry, cache and embedder failures.
pub fn embed_project_chunks_with_limits(
    db: &Db,
    embedder: &dyn Embedder,
    project_id: i64,
    limits: CacheLimits,
) -> Result<u64> {
    embed_project_chunks_with_limits_and_pin(db, embedder, project_id, limits, true)
}

/// [`embed_project_chunks_with_limits`] with control over the backfill lease.
///
/// Pinning protects vectors from eviction while an ANN build may run, but it
/// also suspends the cache byte budget until the TTL expires. Callers that
/// cannot build (no RRF dense channel, or a corpus the resident cap cannot
/// admit) pass `pin_for_ann = false`; `embed_project_chunks` derives this from
/// the project config and the admission rule.
///
/// # Errors
/// Registry, cache and embedder failures.
pub fn embed_project_chunks_with_limits_and_pin(
    db: &Db,
    embedder: &dyn Embedder,
    project_id: i64,
    limits: CacheLimits,
    pin_for_ann: bool,
) -> Result<u64> {
    let texts = repo::chunk_texts(db.connection(), project_id)?;
    if texts.is_empty() {
        return Ok(0);
    }
    let hashes: Vec<String> = texts
        .iter()
        .map(|text| crate::vector_cache::chunk_hash(text))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    // Pin only when a build can actually follow: the resident cap must admit
    // graph plus exact rows, otherwise the lease would suspend the cache budget
    // for nothing (Delta 0.3.1).
    let pin_for_ann = pin_for_ann
        && ann_build_admitted_for_model(
            // Chunk rows, not distinct hashes: `resolve_rows` admits by
            // `chunks.len()`, and duplicate texts still produce one row each.
            texts.len(),
            embedder.model_id(),
            ann_max_bytes(),
        );
    let mut cache = VectorCache::open_with_limits(db.cache_root(), limits)?;
    // Lease the project's vectors for the ANN build; without this a
    // concurrent backfill can evict them and coverage never reaches 100%.
    if pin_for_ann {
        cache.pin(project_id, embedder.model_id(), &hashes, BACKFILL_PIN_TTL)?;
    }
    let result = blocking::enter(|| cache.embed_missing(embedder, &texts));
    match result {
        Ok(vectors) => {
            if pin_for_ann && hashes.len() >= ANN_MIN_VECTORS {
                // The backfill may have taken close to the lease duration;
                // restart it so the ANN build has the full window.
                cache.pin(project_id, embedder.model_id(), &hashes, BACKFILL_PIN_TTL)?;
            } else if pin_for_ann {
                // No ANN tier will use these vectors; do not pin small caches.
                cache.unpin(project_id, embedder.model_id(), &hashes)?;
            }
            Ok(vectors.len() as u64)
        }
        Err(err) => {
            if pin_for_ann {
                let _ = cache.unpin(project_id, embedder.model_id(), &hashes);
            }
            Err(err)
        }
    }
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
    // stays cheap and never builds runtimes per status call (NFR-1). The
    // canonical key (FR-9) keeps coverage aligned with the embedder and the
    // ANN warm path (SC-10).
    coverage_for_key_with_limits(
        db,
        &super::config::embedding_model_id(embedding),
        project_id,
        cfg.cache.limits(),
    )
    .map(Some)
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
    coverage_for_key_with_limits(db, model_id, project_id, CacheLimits::default())
}

/// [`coverage_for_key`] with explicit vector-cache limits.
///
/// # Errors
/// Registry and cache failures.
pub fn coverage_for_key_with_limits(
    db: &Db,
    model_id: &str,
    project_id: i64,
    limits: CacheLimits,
) -> Result<Coverage> {
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
    let cache = VectorCache::open_with_limits(db.cache_root(), limits)?;
    let cached = cache.count_present(model_id, &unique)?;
    Ok(Coverage { cached, total })
}
