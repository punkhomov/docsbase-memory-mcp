//! Admission S1: build/schema checks and the daemon-lifetime lock
//! (FR-4, FR-9; I1; R5; NFR-7).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fd_lock::{RwLock, RwLockWriteGuard};

use crate::conflict::{self, Conflict};
use crate::daemon::lifecycle::{self, DaemonState};
use crate::error::{Error, Result};

/// Held for the whole daemon lifetime; a second daemon cannot acquire it
/// (FR-4, FR-9). The underlying `RwLock` is leaked into a `'static` allocation
/// so the guard can be stored; the daemon exits with the process anyway.
/// Refused acquires also leak one allocation/fd per call — acceptable for the
/// once-per-process daemon path and tests.
#[derive(Debug)]
pub struct Lease {
    #[expect(dead_code, reason = "RAII guard: held, never read, releases on drop")]
    guard: RwLockWriteGuard<'static, File>,
}

impl Lease {
    /// Acquires the admission lock, then checks the recorded build/schema.
    ///
    /// # Errors
    /// Returns [`Error::Admission`] when another daemon holds the lock or the
    /// recorded `daemon.json` does not match this build (FR-4).
    pub fn acquire(cache: &Path, build_id: &str, schema_version: u32) -> Result<Self> {
        lifecycle::create_state_dir(cache)?;
        let path = lock_path(cache);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| Error::internal_with_source("open admission lock", err))?;
        let lock: &'static mut RwLock<File> = Box::leak(Box::new(RwLock::new(file)));
        let guard = match lock.try_write() {
            Ok(guard) => guard,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                let incumbent = recorded(cache);
                let recorded_build = incumbent.as_ref().map(|state| state.build_id.as_str());
                let recorded_schema = incumbent.as_ref().map(|state| state.schema_version);
                conflict::record(
                    cache,
                    &Conflict {
                        kind: "lock_busy",
                        expected: build_id,
                        actual: recorded_build.unwrap_or(""),
                        build_id,
                        schema_version,
                        cache_root: cache,
                        pid: std::process::id(),
                        recorded_build_id: recorded_build,
                        recorded_schema_version: recorded_schema,
                        holder_pid: incumbent.as_ref().map(|state| state.pid),
                    },
                );
                let holder = incumbent.as_ref().map_or_else(
                    || format!("another daemon holds {}", path.display()),
                    |state| format!("daemon (pid {}) holds {}", state.pid, path.display()),
                );
                return Err(Error::Admission {
                    message: format!(
                        "{holder}; run `docsbase daemon stop` to free it, or `docsbase install` to update after replacing the binary"
                    ),
                });
            }
            Err(err) => {
                return Err(Error::internal_with_source("lock admission file", err));
            }
        };

        if let Some(state) = lifecycle::read_state(cache)? {
            let recorded_schema = state.schema_version.to_string();
            let expected_schema = schema_version.to_string();
            let mismatch = if state.build_id != build_id {
                Some(("build_mismatch", build_id, state.build_id.as_str()))
            } else if state.schema_version != schema_version {
                Some((
                    "schema_mismatch",
                    expected_schema.as_str(),
                    recorded_schema.as_str(),
                ))
            } else {
                None
            };
            if let Some((kind, expected, actual)) = mismatch {
                conflict::record(
                    cache,
                    &Conflict {
                        kind,
                        expected,
                        actual,
                        build_id,
                        schema_version,
                        cache_root: cache,
                        pid: std::process::id(),
                        recorded_build_id: Some(state.build_id.as_str()),
                        recorded_schema_version: Some(state.schema_version),
                        holder_pid: Some(state.pid),
                    },
                );
                return Err(Error::Admission {
                    message: format!(
                        "cache was created by build {:?} schema {}, this build is {:?} schema {schema_version}; run `docsbase install` to update, then `docsbase index` to rebuild stale indexes",
                        state.build_id, state.schema_version, build_id
                    ),
                });
            }
        }
        Ok(Self { guard })
    }
}

fn recorded(cache: &Path) -> Option<DaemonState> {
    lifecycle::read_state(cache).ok().flatten()
}

fn lock_path(cache: &Path) -> PathBuf {
    lifecycle::state_dir(cache).join("admission.lock")
}
