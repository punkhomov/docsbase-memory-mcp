//! Admission conflict log (FR-4, NFR-8): one NDJSON record per refused
//! version/schema/lock conflict.
//!
//! Records never contain document content — only paths, versions and pids
//! (NFR-5).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// Fields of one conflict record. `build_id`/`schema_version`/`cache_root`/
/// `pid` describe the process that was refused; `recorded_*`/`holder_pid`
/// describe the incumbent when it could be read from `daemon.json`.
#[derive(Debug)]
pub struct Conflict<'a> {
    /// Stable kind: `lock_busy`, `build_mismatch`, `schema_mismatch`,
    /// `db_schema_newer`, `db_schema_mismatch`, `hello_build_mismatch`.
    pub kind: &'a str,
    /// Expected value of the mismatch (build id or schema string).
    pub expected: &'a str,
    /// Recorded/actual value of the mismatch.
    pub actual: &'a str,
    /// Build of the refused process.
    pub build_id: &'a str,
    /// Schema of the refused process.
    pub schema_version: u32,
    /// Cache root involved.
    pub cache_root: &'a Path,
    /// Process id that was refused (the writer of this record).
    pub pid: u32,
    /// Build recorded in `daemon.json`, when readable.
    pub recorded_build_id: Option<&'a str>,
    /// Schema recorded in `daemon.json`, when readable.
    pub recorded_schema_version: Option<u32>,
    /// Pid recorded in `daemon.json`, when readable.
    pub holder_pid: Option<u32>,
}

/// Appends one record; logging failures never mask the admission error.
pub fn record(cache: &Path, conflict: &Conflict<'_>) {
    if let Err(err) = append(cache, conflict) {
        eprintln!("warning: cannot append conflict log: {err}");
    }
}

fn append(cache: &Path, conflict: &Conflict<'_>) -> Result<()> {
    let dir = cache.join("logs");
    fs::create_dir_all(&dir).map_err(|err| Error::internal_with_source("create logs dir", err))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| Error::internal_with_source("chmod 0700 logs dir", err))?;
    let mut line = serde_json::to_vec(&ConflictLine {
        ts: unix_now(),
        kind: conflict.kind.to_owned(),
        expected: conflict.expected.to_owned(),
        actual: conflict.actual.to_owned(),
        build_id: conflict.build_id.to_owned(),
        schema_version: conflict.schema_version,
        cache_root: conflict.cache_root.to_path_buf(),
        pid: conflict.pid,
        recorded_build_id: conflict.recorded_build_id.map(ToOwned::to_owned),
        recorded_schema_version: conflict.recorded_schema_version,
        holder_pid: conflict.holder_pid,
    })
    .map_err(|err| Error::internal_with_source("serialize conflict", err))?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("conflicts.ndjson"))
        .map_err(|err| Error::internal_with_source("open conflicts log", err))?;
    // `mode` only applies at creation; tighten pre-existing files too.
    let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    file.write_all(&line)
        .map_err(|err| Error::internal_with_source("append conflict", err))
}

#[derive(serde::Serialize)]
struct ConflictLine {
    ts: i64,
    kind: String,
    expected: String,
    actual: String,
    build_id: String,
    schema_version: u32,
    cache_root: PathBuf,
    pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded_build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded_schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    holder_pid: Option<u32>,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}
