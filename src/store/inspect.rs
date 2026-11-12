//! No-write observation of metadata schema identity, not a SQL row snapshot.

use std::path::Path;
use std::time::SystemTime;

use crate::config::schema::{
    ArtifactFact, ArtifactFacts, ArtifactId, ArtifactState, Capabilities, Resolved, SettingsSpec,
};
use crate::error::Result;

use super::registry_inspect::lease::LockedConnection;
use super::settings::{DbSettings, DbSpec};

pub(super) fn schema(cache: &Path, settings: &Resolved<DbSettings>) -> Result<ArtifactFacts> {
    schema_with(cache, settings, || {})
}

fn schema_with(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    before_recheck: impl FnOnce(),
) -> Result<ArtifactFacts> {
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    let path = cache.join(super::DB_FILE);
    let sidecars = sidecars_present(cache);
    if matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        before_recheck();
        let still_missing = matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        let state = match (still_missing, sidecars, sidecars_present(cache)) {
            (true, Some(false), Some(false)) => ArtifactState::Missing,
            (true, _, Some(true)) => ArtifactState::NeedsRecovery,
            _ => ArtifactState::Unknown,
        };
        return Ok(facts(state, None));
    }
    if sidecars != Some(false) {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    let Ok(directory) = std::fs::canonicalize(cache) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    let path = directory.join(super::DB_FILE);
    let Ok(locked) = LockedConnection::open(&path, || {}) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    if sidecars_present(&directory) != Some(false) {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    let Some(first) = observe(&locked, &path) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    before_recheck();
    if locked.check_namespace().is_err() {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    let Some(second) = observe(&locked, &path) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    if first != second
        || sidecars_present(&directory) != Some(false)
        || locked.check_namespace().is_err()
        || std::fs::canonicalize(cache).ok().as_ref() != Some(&directory)
    {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    // SQLite file-format offsets, not runtime policy: schema cookie at40,
    // user_version at60. No table/row/query-readiness claim is made here.
    let cookie = u32::from_be_bytes([
        first.header[40],
        first.header[41],
        first.header[42],
        first.header[43],
    ]);
    let version = u32::from_be_bytes([
        first.header[60],
        first.header[61],
        first.header[62],
        first.header[63],
    ]);
    Ok(facts(
        ArtifactState::Ready,
        Some(format!("sqlite-schema-v1:{version}:{cookie}")),
    ))
}

fn sidecars_present(cache: &Path) -> Option<bool> {
    for suffix in ["-wal", "-shm", "-journal"] {
        match std::fs::symlink_metadata(cache.join(format!("{}{suffix}", super::DB_FILE))) {
            Ok(_) => return Some(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(false)
}

#[derive(PartialEq, Eq)]
struct Observation {
    // Fixed SQLite database-file header length (file format, not a budget).
    header: [u8; 100],
    length: u64,
    modified: SystemTime,
}

fn observe(locked: &LockedConnection, path: &Path) -> Option<Observation> {
    // Metadata queries do not open/close a DB descriptor. A Rust File here would
    // release other connections' POSIX locks when dropped, bypassing SQLite.
    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.is_file() || before.file_type().is_symlink() {
        return None;
    }
    let header = locked.header().ok()?;
    let after = std::fs::symlink_metadata(path).ok()?;
    if before.len() != after.len() || before.modified().ok()? != after.modified().ok()? {
        return None;
    }
    let encoded_size = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded_size == 1 {
        65_536
    } else {
        u64::from(encoded_size)
    };
    if &header[..16] != b"SQLite format 3\0"
        || !(512..=65_536).contains(&page_size)
        || !page_size.is_power_of_two()
        || before.len() < page_size
        || before.len() % page_size != 0
        || !matches!(header[18], 1 | 2)
        || !matches!(header[19], 1 | 2)
        || header[21..24] != [64, 32, 32]
    {
        return None;
    }
    Some(Observation {
        header,
        length: after.len(),
        modified: after.modified().ok()?,
    })
}

fn facts(state: ArtifactState, identity: Option<String>) -> ArtifactFacts {
    ArtifactFacts {
        artifacts: vec![ArtifactFact {
            id: ArtifactId {
                component: "metadata_db_schema".to_owned(),
                project_id: None,
            },
            state,
            identity,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_db() -> (tempfile::TempDir, Resolved<DbSettings>) {
        let cache = tempfile::tempdir().expect("cache");
        let settings = crate::store::settings::standalone().expect("settings");
        drop(crate::store::Db::open_with_settings(cache.path(), &settings).expect("db"));
        assert_eq!(
            schema(cache.path(), &settings).expect("stable").artifacts[0].state,
            ArtifactState::Ready
        );
        (cache, settings)
    }

    #[test]
    fn sidecar_appearing_during_inspection_is_unknown() {
        let (cache, settings) = closed_db();
        let wal = cache.path().join(format!("{}-wal", super::super::DB_FILE));
        let facts = schema_with(cache.path(), &settings, || {
            std::fs::write(&wal, b"new recovery data").expect("concurrent WAL");
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
        assert_eq!(std::fs::read(wal).expect("retained"), b"new recovery data");
    }

    #[test]
    fn changed_header_is_unknown() {
        let (cache, settings) = closed_db();
        let facts = schema_with(cache.path(), &settings, || {
            use std::io::{Seek, SeekFrom, Write};
            // Deliberately bypass SQLite in this corruption fixture: a normal
            // cooperating writer is now blocked by the inspection lease.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(cache.path().join(super::super::DB_FILE))
                .expect("raw header mutation");
            file.seek(SeekFrom::Start(60)).expect("user_version offset");
            file.write_all(&(crate::store::migrations::SCHEMA_VERSION + 1).to_be_bytes())
                .expect("changed schema header");
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }

    #[test]
    fn db_appearing_during_missing_inspection_is_unknown() {
        let cache = tempfile::tempdir().expect("cache");
        let settings = crate::store::settings::standalone().expect("settings");
        let facts = schema_with(cache.path(), &settings, || {
            drop(crate::store::Db::open_with_settings(cache.path(), &settings).expect("new db"));
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn schema_inspection_does_not_follow_a_symlink() {
        let (target, settings) = closed_db();
        let cache = tempfile::tempdir().expect("cache");
        std::os::unix::fs::symlink(
            target.path().join(super::super::DB_FILE),
            cache.path().join(super::super::DB_FILE),
        )
        .expect("symlink");
        let facts = schema(cache.path(), &settings).expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }

    #[cfg(unix)]
    mod fifo {
        use std::io::{Read, Write};
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        use std::process::{Child, Command, Stdio};
        use std::sync::mpsc;
        use std::thread::JoinHandle;
        use std::time::Duration;

        use super::*;

        struct InspectionChild {
            process: Child,
            output: Option<JoinHandle<std::io::Result<String>>>,
        }

        impl Drop for InspectionChild {
            fn drop(&mut self) {
                let _ = self.process.kill();
                let _ = self.process.wait();
                if let Some(output) = self.output.take() {
                    let _ = output.join();
                }
            }
        }

        fn make_fifo(path: &Path) {
            assert!(
                Command::new("mkfifo")
                    .arg(path)
                    .status()
                    .expect("mkfifo")
                    .success()
            );
        }

        fn fifo_metadata(path: &Path) -> (u64, u32, u64, SystemTime) {
            let metadata = std::fs::symlink_metadata(path).expect("FIFO metadata");
            assert!(metadata.file_type().is_fifo(), "FIFO must remain intact");
            (
                metadata.ino(),
                metadata.mode(),
                metadata.len(),
                metadata.modified().expect("FIFO mtime"),
            )
        }

        #[test]
        fn inspection_child() {
            let Some(cache) = std::env::var_os("DOCSBASE_TEST_FIFO_CACHE") else {
                return;
            };
            let cache = std::path::PathBuf::from(cache);
            let path = cache.join(super::super::super::DB_FILE);
            let replacing = std::env::var_os("DOCSBASE_TEST_FIFO_REPLACE").is_some();
            let settings = crate::store::settings::standalone().expect("settings");
            let mut before = (!replacing).then(|| fifo_metadata(&path));
            println!("fifo-inspection-start");
            std::io::stdout().flush().expect("flush readiness");
            let facts = schema_with(&cache, &settings, || {
                assert!(
                    replacing,
                    "an initial FIFO must be rejected on first observation"
                );
                std::fs::remove_file(&path).expect("replace observed DB");
                make_fifo(&path);
                before = Some(fifo_metadata(&path));
                println!("fifo-replaced");
                std::io::stdout().flush().expect("flush replacement");
            })
            .expect("FIFO facts");
            assert_eq!(facts.artifacts.len(), 1);
            assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
            assert!(facts.artifacts[0].identity.is_none());
            assert_eq!(fifo_metadata(&path), before.expect("FIFO fixture"));
            assert_eq!(std::fs::read_dir(&cache).expect("cache entries").count(), 1);
        }

        fn inspect_fifo(replacing: bool) {
            let cache = if replacing {
                closed_db().0
            } else {
                let cache = tempfile::tempdir().expect("cache");
                make_fifo(&cache.path().join(super::super::super::DB_FILE));
                cache
            };
            let mut command = Command::new(std::env::current_exe().expect("test binary"));
            command
                .args([
                    "--exact",
                    "store::inspect::tests::fifo::inspection_child",
                    "--nocapture",
                ])
                .env("DOCSBASE_TEST_FIFO_CACHE", cache.path())
                .env_remove("DOCSBASE_TEST_FIFO_REPLACE")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if replacing {
                command.env("DOCSBASE_TEST_FIFO_REPLACE", "1");
            }
            let mut child = InspectionChild {
                process: command.spawn().expect("FIFO inspection child"),
                output: None,
            };
            let mut stdout = child.process.stdout.take().expect("child stdout");
            let (completed_tx, completed_rx) = mpsc::channel();
            child.output = Some(std::thread::spawn(move || {
                let mut output = String::new();
                let result = stdout.read_to_string(&mut output).map(|_| output);
                let _ = completed_tx.send(());
                result
            }));
            // X11 test-control watchdog: detect deadlock, not processing latency.
            let completed = completed_rx.recv_timeout(Duration::from_secs(10)).is_ok();
            if !completed {
                let _ = child.process.kill();
            }
            let status = child.process.wait().expect("reap FIFO inspection child");
            let output = child
                .output
                .take()
                .expect("stdout reader")
                .join()
                .expect("join reader")
                .expect("read child output");
            assert!(
                completed,
                "FIFO inspection blocked without a writer; child killed and reaped:\n{output}"
            );
            assert!(status.success(), "FIFO inspection failed:\n{output}");
            assert!(output.contains("fifo-inspection-start"));
            assert!(fifo_metadata(&cache.path().join(super::super::super::DB_FILE)).2 == 0);
        }

        #[test]
        fn schema_inspection_of_fifo_without_writer_is_unknown() {
            inspect_fifo(false);
        }

        #[test]
        fn schema_inspection_of_replacement_fifo_is_unknown() {
            inspect_fifo(true);
        }
    }
}
