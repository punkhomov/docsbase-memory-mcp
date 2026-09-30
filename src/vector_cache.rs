//! Content-addressed embedding cache (T53; ADR-12, phase 2).
//!
//! Contract only: no model ships here and nothing wires the cache into
//! `search_docs` yet (`Config::hybrid` stays off). The point is the key:
//! vectors are addressed by `(model_id, chunk_hash)`, never by path or
//! `(doc_id, seq)`, so switching branches, rebasing or duplicating a
//! worktree never re-embeds unchanged bytes.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// Separate SQLite file in the cache root; one cache per machine user.
pub const VECTOR_DB_FILE: &str = "vectors.db";

/// Cache size bounds; eviction drops the least recently used rows first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Maximum number of cached vectors.
    pub max_entries: u64,
    /// Maximum total size of stored vectors, bytes.
    pub max_bytes: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Anything that can turn chunk texts into vectors.
pub trait Embedder: Send + Sync {
    /// Stable identity of the model; changing it invalidates lookups.
    fn model_id(&self) -> &str;
    /// Embeds one batch of chunk texts.
    ///
    /// # Errors
    /// Implementation-defined failures (model, IO).
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Stable content key of one chunk (blake3, hex).
#[must_use]
pub fn chunk_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Content-addressed vector cache backed by `vectors.db`.
pub struct VectorCache {
    conn: Connection,
    sequence: i64,
    limits: CacheLimits,
}

impl VectorCache {
    /// Opens (creating if needed) the cache under `cache_root`.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on directory or `SQLite` failures.
    pub fn open(cache_root: &Path) -> Result<Self> {
        Self::open_with_limits(cache_root, CacheLimits::default())
    }

    /// [`Self::open`] with explicit eviction bounds (tests).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on directory or `SQLite` failures.
    pub fn open_with_limits(cache_root: &Path, limits: CacheLimits) -> Result<Self> {
        crate::platform::fs::secure_dir(cache_root)
            .map_err(|err| Error::internal_with_source("create vector cache dir", err))?;
        let path = cache_root.join(VECTOR_DB_FILE);
        let conn = Connection::open(&path)
            .map_err(|err| Error::internal_with_source("open vector cache", err))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|err| Error::internal_with_source("set vector cache timeout", err))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_cache (
                 model_id TEXT NOT NULL,
                 chunk_hash TEXT NOT NULL,
                 dim INTEGER NOT NULL,
                 vector BLOB NOT NULL,
                 used_at INTEGER NOT NULL,
                 PRIMARY KEY (model_id, chunk_hash)
             );",
        )
        .map_err(|err| Error::internal_with_source("create vector cache schema", err))?;
        let sequence: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(used_at), 0) FROM vector_cache",
                [],
                |row| row.get(0),
            )
            .map_err(|err| Error::internal_with_source("read vector cache sequence", err))?;
        Ok(Self {
            conn,
            sequence,
            limits,
        })
    }

    /// Looks a vector up and refreshes its recency.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on `SQLite` failures or a corrupt row.
    pub fn get(&mut self, model_id: &str, chunk_hash: &str) -> Result<Option<Vec<f32>>> {
        let row = self
            .conn
            .query_row(
                "SELECT dim, vector FROM vector_cache
                 WHERE model_id = ?1 AND chunk_hash = ?2",
                params![model_id, chunk_hash],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(|err| Error::internal_with_source("read vector", err))?;
        let Some((dim, bytes)) = row else {
            return Ok(None);
        };
        let used_at = self.next_use();
        self.conn
            .execute(
                "UPDATE vector_cache SET used_at = ?3
                 WHERE model_id = ?1 AND chunk_hash = ?2",
                params![model_id, chunk_hash, used_at],
            )
            .map_err(|err| Error::internal_with_source("touch vector", err))?;
        let dim = usize::try_from(dim)
            .map_err(|_| Error::internal(format!("vector dim {dim} does not fit usize")))?;
        Ok(Some(decode_vector(dim, &bytes)?))
    }

    /// Stores one vector and evicts least-recently-used rows over the limits.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on `SQLite` failures or an empty vector.
    pub fn put(&mut self, model_id: &str, chunk_hash: &str, vector: &[f32]) -> Result<()> {
        if vector.is_empty() {
            return Err(Error::internal("refusing to cache an empty vector"));
        }
        let used_at = self.next_use();
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for value in vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let dim = i64::try_from(vector.len())
            .map_err(|_| Error::internal("vector dim does not fit i64"))?;
        self.conn
            .execute(
                "INSERT INTO vector_cache (model_id, chunk_hash, dim, vector, used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(model_id, chunk_hash) DO UPDATE SET
                     dim = excluded.dim,
                     vector = excluded.vector,
                     used_at = excluded.used_at",
                params![model_id, chunk_hash, dim, bytes, used_at],
            )
            .map_err(|err| Error::internal_with_source("store vector", err))?;
        self.evict()
    }

    /// Embeds `texts`, reusing cached vectors keyed by `(model, chunk_hash)`.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on cache failures or an embedder that
    /// returns a wrong count or inconsistent dimensions.
    pub fn embed_missing(
        &mut self,
        embedder: &dyn Embedder,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        let model_id = embedder.model_id().to_owned();
        let hashes: Vec<String> = texts.iter().map(|text| chunk_hash(text)).collect();
        let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        let mut missing = Vec::new();
        for (index, hash) in hashes.iter().enumerate() {
            if let Some(vector) = self.get(&model_id, hash)? {
                vectors.push(Some(vector));
            } else {
                vectors.push(None);
                missing.push(index);
            }
        }
        if !missing.is_empty() {
            let request: Vec<String> = missing.iter().map(|index| texts[*index].clone()).collect();
            let batch_vectors = embedder.embed(&request)?;
            if batch_vectors.len() != request.len() {
                return Err(Error::internal(format!(
                    "embedder returned {} vectors for {} chunks",
                    batch_vectors.len(),
                    request.len()
                )));
            }
            // Validate the whole batch before storing anything: a broken
            // embedder must not leave a partially filled cache generation.
            let dim = batch_vectors.first().map_or(0, Vec::len);
            if dim == 0
                || batch_vectors
                    .iter()
                    .any(|vector| vector.is_empty() || vector.len() != dim)
            {
                return Err(Error::internal(format!(
                    "embedder returned inconsistent dimensions (expected {dim})"
                )));
            }
            for (vector, index) in batch_vectors.into_iter().zip(missing) {
                self.put(&model_id, &hashes[index], &vector)?;
                vectors[index] = Some(vector);
            }
        }
        vectors
            .into_iter()
            .enumerate()
            .map(|(index, vector)| {
                vector.ok_or_else(|| Error::internal(format!("missing vector for chunk {index}")))
            })
            .collect()
    }

    /// Number of cached vectors (diagnostics, tests).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on `SQLite` failures.
    pub fn len(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vector_cache", [], |row| row.get(0))
            .map_err(|err| Error::internal_with_source("count vectors", err))?;
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    /// True when no vectors are cached (diagnostics, tests).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on `SQLite` failures.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    fn next_use(&mut self) -> i64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    fn evict(&mut self) -> Result<()> {
        loop {
            let (count, bytes): (i64, i64) = self
                .conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(LENGTH(vector)), 0) FROM vector_cache",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|err| Error::internal_with_source("measure vector cache", err))?;
            let count = u64::try_from(count).unwrap_or(u64::MAX);
            let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
            if count <= self.limits.max_entries && bytes <= self.limits.max_bytes {
                return Ok(());
            }
            let removed = self
                .conn
                .execute(
                    "DELETE FROM vector_cache WHERE rowid = (
                         SELECT rowid FROM vector_cache
                         ORDER BY used_at ASC, rowid ASC LIMIT 1
                     )",
                    [],
                )
                .map_err(|err| Error::internal_with_source("evict vector", err))?;
            if removed == 0 {
                return Ok(());
            }
        }
    }
}

fn decode_vector(dim: usize, bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() != dim * 4 {
        return Err(Error::internal(format!(
            "corrupt vector: {} bytes for dim {dim}",
            bytes.len()
        )));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lru_eviction_keeps_recent_entries() {
        let dir = TempDir::new().expect("dir");
        let limits = CacheLimits {
            max_entries: 2,
            max_bytes: u64::MAX,
        };
        let mut cache = VectorCache::open_with_limits(dir.path(), limits).expect("cache");
        cache.put("m", "h1", &[1.0]).expect("put h1");
        cache.put("m", "h2", &[2.0]).expect("put h2");
        assert!(cache.get("m", "h1").expect("get").is_some());
        cache.put("m", "h3", &[3.0]).expect("put h3");

        assert_eq!(cache.len().expect("len"), 2);
        assert!(cache.get("m", "h1").expect("get h1").is_some());
        assert!(cache.get("m", "h2").expect("get h2").is_none());
        assert!(cache.get("m", "h3").expect("get h3").is_some());
    }

    #[test]
    fn chunk_hashes_are_stable() {
        assert_eq!(chunk_hash("same text"), chunk_hash("same text"));
        assert_ne!(chunk_hash("same text"), chunk_hash("same text "));
    }
}
