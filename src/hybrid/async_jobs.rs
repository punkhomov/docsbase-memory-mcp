//! Bounded lookup/validation/write kernels. Connections never cross provider awaits.
use crate::error::{Error, Result};
use crate::vector_cache::{VectorCacheSource, WriteCapacity, chunk_hash};
use std::sync::Arc;

pub(crate) struct EmbeddingBatch {
    pub(crate) hashes: Vec<String>,
    pub(crate) texts: Arc<[String]>,
}
pub(crate) fn missing(
    source: &VectorCacheSource,
    root: &std::path::Path,
    model: &str,
    texts: Vec<String>,
    expected_dimension: Option<usize>,
) -> Result<EmbeddingBatch> {
    let mut cache = source.connect(root)?;
    let mut hashes = Vec::new();
    let mut inputs = Vec::new();
    for text in texts {
        let hash = chunk_hash(&text);
        if hashes.contains(&hash) {
            continue;
        }
        let hit = match expected_dimension {
            Some(dimension) => cache.get_for_dimension(model, &hash, dimension)?,
            None => cache.get(model, &hash)?,
        };
        if hit.is_none() {
            hashes.push(hash);
            inputs.push(text);
        }
    }
    Ok(EmbeddingBatch {
        hashes,
        texts: inputs.into(),
    })
}
pub(crate) fn store(
    source: &VectorCacheSource,
    root: &std::path::Path,
    model: &str,
    batch: &EmbeddingBatch,
    vectors: &[Vec<f32>],
    expected_dim: usize,
    capacity: WriteCapacity,
) -> Result<()> {
    validate(batch, vectors, expected_dim)?;
    let mut cache = source.connect(root)?;
    for (hash, vector) in batch.hashes.iter().zip(vectors) {
        cache.put_with_capacity(model, hash, vector, capacity)?;
    }
    Ok(())
}

pub(crate) fn try_store_without_eviction(
    source: &VectorCacheSource,
    root: &std::path::Path,
    model: &str,
    batch: &EmbeddingBatch,
    vectors: &[Vec<f32>],
    expected_dim: usize,
    operation: &crate::search::operation::SearchOperation,
) -> Result<bool> {
    validate(batch, vectors, expected_dim)?;
    let mut cache = source.connect(root)?;
    let rows = batch
        .hashes
        .iter()
        .map(String::as_str)
        .zip(vectors.iter().map(Vec::as_slice));
    cache.try_store_without_eviction(model, rows, operation)
}

fn validate(batch: &EmbeddingBatch, vectors: &[Vec<f32>], expected_dim: usize) -> Result<()> {
    if vectors.len() != batch.hashes.len()
        || vectors
            .iter()
            .any(|v| v.len() != expected_dim || v.iter().any(|n| !n.is_finite()))
    {
        return Err(Error::internal("invalid embedding batch shape or values"));
    }
    Ok(())
}
