//! Bounded lookup/validation/write kernels. Connections never cross provider awaits.
use crate::error::{Error, Result};
use crate::vector_cache::{VectorCacheSource, chunk_hash};
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
) -> Result<EmbeddingBatch> {
    let mut cache = source.connect(root)?;
    let mut hashes = Vec::new();
    let mut inputs = Vec::new();
    for text in texts {
        let hash = chunk_hash(&text);
        if !hashes.contains(&hash) && cache.get(model, &hash)?.is_none() {
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
) -> Result<()> {
    if vectors.len() != batch.hashes.len()
        || vectors
            .iter()
            .any(|v| v.len() != expected_dim || v.iter().any(|n| !n.is_finite()))
    {
        return Err(Error::internal("invalid embedding batch shape or values"));
    }
    let mut cache = source.connect(root)?;
    for (hash, vector) in batch.hashes.iter().zip(vectors) {
        cache.put(model, hash, vector)?;
    }
    Ok(())
}
