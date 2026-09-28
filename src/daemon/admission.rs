//! Admission S1: build/schema checks and the daemon-lifetime lock
//! (FR-4, FR-9; I1; R5; NFR-7).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fd_lock::{RwLock, RwLockWriteGuard};

use crate::daemon::lifecycle::{self, DaemonState};
use crate::error::{Error, Result};

/// Held for the whole daemon lifetime; a second daemon cannot acquire it
/// (FR-4, FR-9). The underlying `RwLock` is leaked into a `'static` allocation
/// so the guard can be stored; the daemon exits with the process anyway.
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
                let actual = recorded(cache).map(|state| state.build_id);
                log_best_effort(
                    cache,
                    &Conflict {
                        kind: "lock_busy",
                        expected: build_id,
                        actual: actual.as_deref().unwrap_or(""),
                        cache_root: cache,
                    },
                );
                return Err(Error::Admission {
                    message: format!(
                        "another daemon holds {}; stop it with `docsbase daemon stop`",
                        path.display()
                    ),
                });
            }
            Err(err) => {
                return Err(Error::internal_with_source("lock admission file", err));
            }
        };

        if let Some(state) = lifecycle::read_state(cache)? {
            let mismatch = if state.build_id != build_id {
                Some(("build_mismatch", state.build_id.as_str()))
            } else if state.schema_version != schema_version {
                Some(("schema_mismatch", ""))
            } else {
                None
            };
            if let Some((kind, actual)) = mismatch {
                log_best_effort(
                    cache,
                    &Conflict {
                        kind,
                        expected: build_id,
                        actual,
                        cache_root: cache,
                    },
                );
                return Err(Error::Admission {
                    message: format!(
                        "daemon.json records build {:?} schema {}, this build is {:?} schema {schema_version}; run `docsbase daemon stop` and retry",
                        state.build_id, state.schema_version, build_id
                    ),
                });
            }
        }
        Ok(Self { guard })
    }
}

struct Conflict<'a> {
    kind: &'a str,
    expected: &'a str,
    actual: &'a str,
    cache_root: &'a Path,
}

fn recorded(cache: &Path) -> Option<DaemonState> {
    lifecycle::read_state(cache).ok().flatten()
}

fn log_best_effort(cache: &Path, conflict: &Conflict<'_>) {
    if let Err(err) = append_conflict(cache, conflict) {
        eprintln!("warning: cannot append conflict log: {err}");
    }
}

fn append_conflict(cache: &Path, conflict: &Conflict<'_>) -> Result<()> {
    let dir = cache.join("logs");
    fs::create_dir_all(&dir).map_err(|err| Error::internal_with_source("create logs dir", err))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| Error::internal_with_source("chmod 0700 logs dir", err))?;
    let mut line = serde_json::to_vec(&ConflictLine {
        ts: unix_now(),
        kind: conflict.kind.to_owned(),
        expected: conflict.expected.to_owned(),
        actual: conflict.actual.to_owned(),
        cache_root: conflict.cache_root.to_path_buf(),
    })
    .map_err(|err| Error::internal_with_source("serialize conflict", err))?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("conflicts.ndjson"))
        .map_err(|err| Error::internal_with_source("open conflicts log", err))?;
    file.write_all(&line)
        .map_err(|err| Error::internal_with_source("append conflict", err))
}

#[derive(serde::Serialize)]
struct ConflictLine {
    ts: i64,
    kind: String,
    expected: String,
    actual: String,
    cache_root: PathBuf,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

fn lock_path(cache: &Path) -> PathBuf {
    lifecycle::state_dir(cache).join("admission.lock")
}
