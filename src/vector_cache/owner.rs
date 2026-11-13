//! Advisory native coordination between current legacy and owned connections.
//! Keep the lock file permanently: unlinking it would split the lock namespace.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::settings::VectorStoreSettings;
use super::{VectorCache, open_connection, read_sequence};
use crate::error::{Error, Result};

const LOCK_FILE: &str = "vectors.lock";

/// One immutable policy owner per canonical cache root, retained by connections.
///
/// This coordinates cooperating callers only. It is not an access-control or
/// persisted activation mechanism; older binaries/raw SQL and external file
/// replacement do not participate. Production startup remains legacy for now.
#[derive(Clone)]
pub struct VectorStore(Arc<StoreInner>);

struct StoreInner {
    root: PathBuf,
    settings: VectorStoreSettings,
    sequence: AtomicI64,
    _lease: ModeLease,
}

impl VectorStore {
    /// Acquire exclusive policy ownership before opening the vector database.
    /// Even an identical policy needs to reuse this owner rather than reacquire.
    ///
    /// # Errors
    /// Admission for invalid settings or an existing legacy/strict lease;
    /// internal errors for directory, file-lock or SQLite initialization failures.
    pub fn acquire(cache_root: &Path, settings: &VectorStoreSettings) -> Result<Self> {
        settings.validate()?;
        let root = prepare_root(cache_root)?;
        let lease = ModeLease::acquire(&root, true)?;
        // Initialize under the exclusive lease. No SQLite transaction survives
        // acquisition; connections have independent short SQL phases.
        let conn = open_connection(&root, settings.busy_timeout)?;
        let sequence = read_sequence(&conn)?;
        drop(conn);
        Ok(Self(Arc::new(StoreInner {
            root,
            settings: settings.clone(),
            sequence: AtomicI64::new(sequence),
            _lease: lease,
        })))
    }

    /// Open an independent connection using this owner's policy.
    ///
    /// # Errors
    /// SQLite initialization or connection failures.
    pub fn connect(&self) -> Result<VectorCache> {
        VectorCache::open_owned(self.clone())
    }

    /// Canonical cache directory used by every owned connection.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.0.root
    }

    /// Immutable policy; connection-local overrides are deliberately unavailable.
    #[must_use]
    pub fn settings(&self) -> &VectorStoreSettings {
        &self.0.settings
    }

    fn next_use(&self) -> i64 {
        // Only a recency counter, not a memory-publication fence. SQLite's
        // immediate write reservation orders the associated database mutations.
        let previous = self
            .0
            .sequence
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
        match previous {
            Ok(value) | Err(value) => value.saturating_add(1),
        }
    }
}

pub(super) fn prepare_root(root: &Path) -> Result<PathBuf> {
    crate::platform::fs::secure_dir(root)
        .map_err(|err| Error::internal_with_source("create vector cache dir", err))?;
    std::fs::canonicalize(root)
        .map_err(|err| Error::internal_with_source("canonicalize vector cache dir", err))
}

/// File close releases the native lock, including process termination. No
/// explicit unlock on failed acquisition and no lockfile removal on release.
pub(super) struct ModeLease {
    file: File,
}
impl Drop for ModeLease {
    fn drop(&mut self) {
        // Close alone can leave the lock held by a transient fork-inherited
        // descriptor before exec. Only successfully acquired leases reach Drop.
        let _ = self.file.unlock();
    }
}
impl ModeLease {
    pub(super) fn legacy(root: &Path) -> Result<Self> {
        Self::acquire(root, false)
    }

    fn acquire(root: &Path, exclusive: bool) -> Result<Self> {
        let file = crate::platform::fs::open_private_lock(&root.join(LOCK_FILE))
            .map_err(|err| Error::internal_with_source("open vector mode lock", err))?;
        let metadata = file
            .metadata()
            .map_err(|err| Error::internal_with_source("inspect vector mode lock", err))?;
        if !crate::platform::fs::is_regular_nofollow(&metadata) {
            return Err(Error::Admission {
                message: "vector mode lock must be a regular file".into(),
            });
        }
        match if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        } {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(Error::Admission {
                message: "vector store is already open in an incompatible ownership mode".into(),
            }),
            Err(TryLockError::Error(err)) => {
                Err(Error::internal_with_source("acquire vector mode lock", err))
            }
        }
    }
}

/// Declared after the connection in `VectorCache` so SQLite closes before release.
pub(super) enum Access {
    Legacy { sequence: i64, _lease: ModeLease },
    Owned { owner: VectorStore },
}
impl Access {
    pub(super) fn is_owned(&self) -> bool {
        matches!(self, Self::Owned { .. })
    }

    pub(super) fn next_use(&mut self) -> i64 {
        match self {
            Self::Legacy { sequence, .. } => {
                *sequence = sequence.saturating_add(1);
                *sequence
            }
            Self::Owned { owner } => owner.next_use(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_connections_install_the_policy_busy_timeout() {
        for millis in [0_u64, 17, 5000] {
            let root = tempfile::tempdir().unwrap();
            let settings = VectorStoreSettings {
                busy_timeout: std::time::Duration::from_millis(millis),
                ..VectorStoreSettings::owner_defaults()
            };
            let owner = VectorStore::acquire(root.path(), &settings).unwrap();
            let clone = owner.clone();
            for connection in [owner.connect().unwrap(), clone.connect().unwrap()] {
                let actual: i64 = connection
                    .conn
                    .pragma_query_value(None, "busy_timeout", |row| row.get(0))
                    .unwrap();
                assert_eq!(actual, i64::try_from(millis).unwrap());
            }
        }
    }

    #[test]
    fn releasing_a_lease_does_not_wait_for_an_inherited_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let lease = ModeLease::acquire(root.path(), true).unwrap();
        // Duplicates retain the same native lock description, as on fork before
        // exec closes CLOEXEC descriptors in a concurrently spawned process.
        let inherited = lease.file.try_clone().unwrap();
        drop(lease);
        assert!(ModeLease::acquire(root.path(), true).is_ok());
        drop(inherited);
    }
}
