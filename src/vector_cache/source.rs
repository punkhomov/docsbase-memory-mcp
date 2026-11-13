//! Explicit connection ownership at consumer boundaries.

use std::path::Path;

use super::{CacheLimits, VectorCache, VectorStore};
use crate::error::{Error, Result};

/// Connection source selected by the composition root, never by a consumer.
#[derive(Clone)]
pub enum VectorCacheSource {
    /// Compatibility mode with the caller's complete legacy limits.
    Legacy(CacheLimits),
    /// Shared immutable policy and native lifetime ownership.
    Owned(VectorStore),
}

impl VectorCacheSource {
    /// Check an owned root without creating directories or opening a database.
    /// Legacy callers preserve their existing root semantics.
    ///
    /// # Errors
    /// Admission when the root is missing or belongs to another store.
    pub fn validate_root(&self, root: &Path) -> Result<()> {
        if let Self::Owned(owner) = self {
            let canonical = std::fs::canonicalize(root).map_err(|_| Error::Admission {
                message: "owned vector cache root is unavailable".into(),
            })?;
            if canonical != owner.root() {
                return Err(Error::Admission {
                    message: "vector source belongs to a different cache root".into(),
                });
            }
        }
        Ok(())
    }

    /// Open an independent connection. Owned mode has no legacy fallback or
    /// connection-local policy override.
    ///
    /// # Errors
    /// Root admission, native legacy-mode conflict or SQLite failures.
    pub fn connect(&self, root: &Path) -> Result<VectorCache> {
        self.validate_root(root)?;
        match self {
            Self::Legacy(limits) => VectorCache::open_with_limits(root, *limits),
            Self::Owned(owner) => owner.connect(),
        }
    }
}
