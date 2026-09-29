//! OS seam (ADR-9): the only module that touches platform transport.
//!
//! v1 ships the Unix implementation behind a small facade (`Endpoint`,
//! `Listener`, `Stream`, `BlockingStream`). Signals, process lifecycle,
//! filesystem permissions and path semantics move here in T35/T36.
//!
//! The module is `pub` like the other internal modules of the crate
//! (`daemon`, `ipc`, `store`, …) so integration tests can exercise the seam
//! directly; it is not a stable API.

mod unix;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use unix::{
    BlockingListener, BlockingStream, Listener, ShutdownSignal, Stream, bind, bind_blocking,
    connect_blocking, connect_probe,
};

/// Private directory/file modes and logs (NFR-5).
pub mod fs {
    pub use super::unix::{open_private_log, secure_dir, secure_executable, secure_file};
}

/// Process liveness, counters and detach (Linux `/proc`; no-op elsewhere).
pub mod process {
    pub use super::unix::{detach, fd_count, process_alive, thread_count};
}

/// Address of a local daemon endpoint (FR-33).
///
/// Serialized as a plain path string so existing `daemon.json` files keep
/// their `"socket": "<path>"` shape; phase 3 adds a Windows variant here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// Unix domain socket.
    Unix(PathBuf),
}

impl Endpoint {
    /// Path of the underlying Unix socket.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        match self {
            Self::Unix(path) => path,
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.as_path().display())
    }
}

impl Serialize for Endpoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_path().to_string_lossy())
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = PathBuf::deserialize(deserializer)?;
        Ok(Self::Unix(path))
    }
}

/// Endpoint of the daemon serving `cache` (design §3).
#[must_use]
pub fn daemon_endpoint(cache: &Path) -> Endpoint {
    Endpoint::Unix(cache.join("state").join("daemon.sock"))
}

/// True when the endpoint file exists.
#[must_use]
pub fn exists(endpoint: &Endpoint) -> bool {
    endpoint.as_path().exists()
}

/// Removes the endpoint file; a missing endpoint is already clean.
///
/// # Errors
/// Returns the raw IO error when the file exists but cannot be removed.
pub fn remove(endpoint: &Endpoint) -> std::io::Result<()> {
    match std::fs::remove_file(endpoint.as_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
