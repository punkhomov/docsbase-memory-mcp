//! Live session registry (FR-2, FR-3, FR-8; NFR-9).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// One registered frontend session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Monotonic session id.
    pub id: u64,
    /// Frontend process id.
    pub pid: u32,
    /// Working directory the session registered from.
    pub cwd: PathBuf,
    /// Project id bound by cwd (I6); `None` for registry-wide sessions.
    pub project_id: Option<i64>,
}

/// Sessions currently served by the daemon.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    next_id: AtomicU64,
    sessions: Mutex<HashMap<u64, SessionInfo>>,
}

/// Runtime counters (design §5 `Response::Stats`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Live sessions.
    pub sessions: u64,
    /// Open file descriptors held by the daemon.
    pub fd_count: u64,
}

impl SessionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session and returns its id.
    pub fn join(&self, pid: u32, cwd: PathBuf, project_id: Option<i64>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.map().insert(
            id,
            SessionInfo {
                id,
                pid,
                cwd,
                project_id,
            },
        );
        id
    }

    /// Removes a session (EOF, error or client death) (FR-8).
    pub fn leave(&self, id: u64) {
        self.map().remove(&id);
    }

    /// Snapshot of one session.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<SessionInfo> {
        self.map().get(&id).cloned()
    }

    /// Number of live sessions.
    #[must_use]
    pub fn count(&self) -> usize {
        self.map().len()
    }

    /// Current counters for `status`/`Stats`.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            sessions: u64::try_from(self.count()).unwrap_or(u64::MAX),
            fd_count: open_fd_count(),
        }
    }

    fn map(&self) -> MutexGuard<'_, HashMap<u64, SessionInfo>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Open file descriptors of this process (Linux `/proc`; 0 elsewhere).
#[must_use]
pub fn open_fd_count() -> u64 {
    std::fs::read_dir("/proc/self/fd").map_or(0, |entries| {
        u64::try_from(entries.count()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_leave_round_trip() {
        let registry = SessionRegistry::new();
        let first = registry.join(1, PathBuf::from("/a"), None);
        let second = registry.join(2, PathBuf::from("/b"), None);
        assert_ne!(first, second);
        assert_eq!(registry.count(), 2);
        assert_eq!(registry.get(first).expect("first").pid, 1);
        registry.leave(first);
        assert_eq!(registry.count(), 1);
        registry.leave(999);
        assert_eq!(registry.count(), 1);
        assert!(registry.stats().sessions >= 1);
    }
}
