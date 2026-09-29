//! Live session registry (FR-2, FR-3, FR-8; NFR-9).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

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
    cancels: Mutex<HashMap<u64, oneshot::Sender<()>>>,
}

/// Runtime counters surfaced by `status` (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Live sessions.
    pub sessions: u64,
    /// Open file descriptors held by the daemon.
    pub fd_count: u64,
    /// OS threads of the daemon process.
    pub threads: u64,
}

impl SessionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session and returns its id plus the receiver that fires
    /// when the daemon reaps it because the frontend died (NFR-9).
    pub fn join(
        &self,
        pid: u32,
        cwd: PathBuf,
        project_id: Option<i64>,
    ) -> (u64, oneshot::Receiver<()>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        // Hold the session lock across both inserts so a concurrent
        // `prune_dead` can never observe the session without its cancel
        // sender (lock order is always sessions -> cancels).
        let mut sessions = self.map();
        sessions.insert(
            id,
            SessionInfo {
                id,
                pid,
                cwd,
                project_id,
            },
        );
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, cancel_tx);
        drop(sessions);
        (id, cancel_rx)
    }

    /// Removes a session (EOF, error or client death) (FR-8).
    pub fn leave(&self, id: u64) {
        self.map().remove(&id);
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
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
            threads: open_thread_count(),
        }
    }

    /// Drops sessions whose frontend process is gone (NFR-9, C6), cancelling
    /// their connections so no descriptor outlives the 2 s cleanup bound;
    /// returns the removed session ids.
    pub fn prune_dead(&self) -> Vec<u64> {
        let mut sessions = self.map();
        let dead: Vec<u64> = sessions
            .iter()
            .filter(|(_, session)| !crate::platform::process::process_alive(session.pid))
            .map(|(id, _)| *id)
            .collect();
        let mut cancels = self.cancels.lock().unwrap_or_else(PoisonError::into_inner);
        for id in &dead {
            sessions.remove(id);
            if let Some(cancel) = cancels.remove(id) {
                let _ = cancel.send(());
            }
        }
        dead
    }

    fn map(&self) -> MutexGuard<'_, HashMap<u64, SessionInfo>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Open file descriptors of this process (`platform::process` counters).
#[must_use]
pub fn open_fd_count() -> u64 {
    crate::platform::process::fd_count()
}

/// OS threads of this process (`platform::process` counters).
#[must_use]
pub fn open_thread_count() -> u64 {
    crate::platform::process::thread_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_leave_round_trip() {
        let registry = SessionRegistry::new();
        let (first, _first_cancel) = registry.join(1, PathBuf::from("/a"), None);
        let (second, _second_cancel) = registry.join(2, PathBuf::from("/b"), None);
        assert_ne!(first, second);
        assert_eq!(registry.count(), 2);
        assert_eq!(registry.get(first).expect("first").pid, 1);
        registry.leave(first);
        assert_eq!(registry.count(), 1);
        registry.leave(999);
        assert_eq!(registry.count(), 1);
        assert!(registry.stats().sessions >= 1);
    }

    #[test]
    fn prune_dead_reaps_and_cancels() {
        let registry = SessionRegistry::new();
        let dead_pid = {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn");
            let pid = child.id();
            child.kill().expect("kill");
            let _ = child.wait();
            pid
        };
        let (live, _live_cancel) = registry.join(std::process::id(), PathBuf::from("/live"), None);
        let (dead, mut dead_cancel) = registry.join(dead_pid, PathBuf::from("/dead"), None);
        let pruned = registry.prune_dead();
        assert_eq!(pruned, vec![dead]);
        assert!(registry.get(dead).is_none());
        assert!(registry.get(live).is_some());
        assert!(dead_cancel.try_recv().is_ok(), "cancel must fire");
    }
}
