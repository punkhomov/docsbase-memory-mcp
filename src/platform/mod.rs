//! OS seam (ADR-9/10): the only module that touches platform APIs.
//!
//! v1 ships the Unix implementation behind a small facade: transport
//! (`Endpoint`, `Listener`, `Stream`, `BlockingStream`), signals, process
//! lifecycle, filesystem permissions and path semantics (T34–T36). The
//! Windows x64 backend lives in [`windows`] (ADR-10, T39–T41).
//!
//! The module is `pub` like the other internal modules of the crate
//! (`daemon`, `ipc`, `store`, …) so integration tests can exercise the seam
//! directly; it is not a stable API.

pub mod paths;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(unix)]
pub use unix::{
    BlockingListener, BlockingStream, Listener, ShutdownSignal, Stream, bind, bind_blocking,
    connect_blocking, connect_probe, daemon_endpoint, exists, remove,
};
#[cfg(windows)]
pub use windows::{
    BlockingListener, BlockingStream, Listener, ShutdownSignal, Stream, bind, bind_blocking,
    connect_blocking, connect_probe, daemon_endpoint, exists, remove,
};

/// Private directory/file modes and logs (NFR-5).
#[cfg(unix)]
pub mod fs {
    pub use super::unix::{open_private_log, secure_dir, secure_executable, secure_file};
}
/// Private-path policy on Windows is best-effort (ADR-10: `%LOCALAPPDATA%`
/// ACLs; no chmod semantics).
#[cfg(windows)]
pub mod fs {
    pub use super::windows::{open_private_log, secure_dir, secure_executable, secure_file};
}

/// Process liveness, counters and detach (Linux `/proc`; ADR-10 on Windows).
#[cfg(unix)]
pub mod process {
    pub use super::unix::{detach, fd_count, process_alive, thread_count};
}
#[cfg(windows)]
pub mod process {
    pub use super::windows::{detach, fd_count, process_alive, thread_count};
}

/// Address of a local daemon endpoint (FR-33).
///
/// Serialized as a plain string so existing `daemon.json` files keep their
/// `"socket": "<name>"` shape on both platforms (ADR-10).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// Unix domain socket path.
    Unix(PathBuf),
    /// Windows named-pipe name inside the `\\.\pipe\` namespace.
    Pipe(String),
}

impl Endpoint {
    /// File path of a Unix endpoint; pipe names expose their name as a path
    /// for diagnostics and legacy manifest rows.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        match self {
            Self::Unix(path) => path,
            Self::Pipe(name) => Path::new(name),
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
        match self {
            Self::Unix(path) => serializer.serialize_str(&path.to_string_lossy()),
            Self::Pipe(name) => serializer.serialize_str(name),
        }
    }
}

#[cfg(unix)]
impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = PathBuf::deserialize(deserializer)?;
        Ok(Self::Unix(path))
    }
}

#[cfg(windows)]
impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Self::Pipe(name))
    }
}

/// Deterministic pipe name for `cache` (ADR-10): `docsbase-<blake3>` keeps two
/// caches from colliding inside the machine-wide `\\.\pipe\` namespace and
/// stays well below the platform name limit.
#[must_use]
pub fn pipe_name(cache: &Path) -> String {
    let key = cache.canonicalize().unwrap_or_else(|_| cache.to_path_buf());
    let hash = blake3::hash(key.to_string_lossy().as_bytes());
    format!("docsbase-{}", &hash.to_hex()[..32])
}
