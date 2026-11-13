//! Isolated CF5b1 experiment. This file is not linked into docsbase.
//! Copy this file and ../control-state-v1.sql into a scratch crate, see report.
#![cfg(test)]

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use rusqlite::{Connection, OpenFlags, params};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::ZERO)?;
    connection.execute_batch(
        "PRAGMA page_size=4096; PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA;
         PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA mmap_size=0;
         PRAGMA cache_size=-8; PRAGMA cache_spill=ON; PRAGMA max_page_count=16384;",
    )?;
    Ok(connection)
}

fn initialize(path: &Path, count: usize) -> TestResult {
    let mut connection = open(path)?;
    connection.execute_batch(include_str!("control-state-v1.sql"))?;
    let transaction = connection.transaction()?;
    let identity = [1_u8; 32];
    transaction.execute(
        "INSERT INTO namespace VALUES (1, ?1, '/cache', '/config', '/data', x'01')",
        [identity],
    )?;
    transaction.execute(
        "INSERT INTO operation VALUES (1, ?1, 'preparing', 0, 'plan', 'observation',
         'consent', x'01', ?2, ?3, ?3, 1000000, 1073741824, 5000)",
        params![
            identity,
            i64::try_from(count)?,
            i64::try_from(count * 8192)?
        ],
    )?;
    // Synthetic images; hashes/paths are placeholders, NOT full semantic proof.
    for ordinal in 0..count {
        transaction.execute(
            "INSERT INTO source VALUES (?1, ?2, ?3, ?4, ?5, x'01',
             1, zeroblob(8192), ?2, 1, zeroblob(8192), ?2, 'pending')",
            params![
                i64::try_from(ordinal)?,
                identity,
                if ordinal == 0 { "global" } else { "project" },
                if ordinal == 0 {
                    None
                } else {
                    Some(i64::try_from(ordinal)?)
                },
                format!("/source/{ordinal}"),
            ],
        )?;
    }
    transaction.execute("UPDATE operation SET sealed=1", [])?;
    transaction.commit()?;
    Ok(())
}

fn scratch(label: &str) -> std::io::Result<PathBuf> {
    let root = PathBuf::from(std::env::var_os("CF5B1_SCRATCH").expect("explicit scratch root"));
    let path = root.join(format!("{label}-{}", std::process::id()));
    // Caller supplies a private, disposable root; never delete existing paths.
    fs::create_dir(&path)?;
    Ok(path)
}

fn child(mode: &str, path: &Path) -> std::io::Result<ExitStatus> {
    Command::new(std::env::current_exe()?)
        .args(["--exact", "experiment_child", "--nocapture"])
        .env("CF5B1_CHILD_MODE", mode)
        .env("CF5B1_CHILD_PATH", path)
        .status()
}

#[test]
fn experiment_child() -> TestResult {
    let Some(mode) = std::env::var_os("CF5B1_CHILD_MODE") else {
        return Ok(());
    };
    let path = PathBuf::from(std::env::var_os("CF5B1_CHILD_PATH").expect("child path"));
    match mode.to_str().expect("child mode") {
        "before_commit" | "after_commit" => {
            let mut connection = open(&path)?;
            let transaction = connection.transaction()?;
            transaction.execute("UPDATE operation SET state='applying'", [])?;
            transaction.execute("UPDATE source SET progress='new_verified'", [])?;
            if mode == "after_commit" {
                transaction.commit()?;
                // Deliberately bypass connection Drop, as for sudden process loss.
                std::process::exit(92);
            }
            let journal = fs::read(path.with_extension("db-journal"))?;
            assert!(journal.len() > 512 && journal[..8].iter().any(|&byte| byte != 0));
            // No transaction/connection destructor; leaves a real hot journal.
            std::process::exit(91);
        }
        "shared_ok" | "shared_busy" | "exclusive_ok" | "exclusive_busy" => {
            let file = OpenOptions::new().read(true).write(true).open(path)?;
            let result = if mode.to_str().expect("mode").starts_with("shared") {
                file.try_lock_shared()
            } else {
                file.try_lock()
            };
            if mode.to_str().expect("mode").ends_with("busy") {
                assert!(matches!(result, Err(TryLockError::WouldBlock)));
            } else {
                result?;
                file.unlock()?;
            }
        }
        "wal_transaction" | "wal_writer_busy" | "wal_writer_ok" => {
            let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            connection.busy_timeout(std::time::Duration::ZERO)?;
            if mode == "wal_transaction" {
                connection.execute_batch("BEGIN IMMEDIATE")?;
                assert!(path.with_extension("db-wal").exists());
                assert!(path.with_extension("db-shm").exists());
                println!("WAL BEGIN IMMEDIATE created wal/shm before any INSERT");
                connection.execute_batch("ROLLBACK")?;
            } else {
                let result = connection.execute("INSERT INTO registry_probe VALUES (2)", []);
                if mode == "wal_writer_busy" {
                    assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::DatabaseBusy));
                } else {
                    assert_eq!(result?, 1);
                }
            }
        }
        _ => return Err("unknown child mode".into()),
    }
    Ok(())
}

#[test]
fn pragmas_and_schema_constraints() -> TestResult {
    let path = scratch("schema")?.join("control.db");
    initialize(&path, 2)?;
    let connection = open(&path)?;
    let sqlite: String = connection.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
    println!("bundled SQLite {sqlite}; journal=DELETE, synchronous=EXTRA");
    for (pragma, expected) in [
        ("synchronous", 3_i64),
        ("application_id", 1_145_193_286),
        ("user_version", 1),
        ("foreign_keys", 1),
        ("page_size", 4096),
        ("max_page_count", 16384),
    ] {
        assert_eq!(
            connection.pragma_query_value(None, pragma, |r| r.get::<_, i64>(0))?,
            expected
        );
    }
    assert_eq!(
        connection.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?,
        "delete"
    );
    for sql in [
        "UPDATE operation SET state='strict'", // Cannot skip applying.
        "UPDATE operation SET vector_max_bytes=1", // Cannot alter sealed policy.
        "UPDATE operation SET sealed=0",
        "DELETE FROM namespace",
        "DELETE FROM source",
        "UPDATE source SET before_bytes=x'00'",
    ] {
        assert!(connection.execute(sql, []).is_err(), "must refuse: {sql}");
    }
    connection.execute("UPDATE operation SET state='applying'", [])?;
    assert!(
        connection
            .execute("UPDATE operation SET state='strict'", [])
            .is_err()
    );
    connection.execute("UPDATE source SET progress='new_verified'", [])?;
    connection.execute("UPDATE operation SET state='strict'", [])?;
    drop(connection);
    let connection = open(&path)?;
    assert!(
        connection
            .execute("UPDATE operation SET state='restoring'", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM operation", []).is_err());
    Ok(())
}

#[test]
fn namespace_insert_replace_is_refused() -> TestResult {
    let path = scratch("replace-namespace")?.join("control.db");
    initialize(&path, 2)?;
    let connection = open(&path)?;
    connection.pragma_update(None, "recursive_triggers", false)?;
    let replacement = connection.execute(
        "INSERT OR REPLACE INTO namespace SELECT singleton, namespace_id,
         '/different-cache', config_root, data_root, root_identities FROM namespace",
        [],
    );
    assert!(
        replacement.is_err(),
        "REPLACE must not bypass immutable namespace"
    );
    assert_eq!(
        connection.query_row("SELECT cache_root FROM namespace", [], |r| r
            .get::<_, String>(0))?,
        "/cache"
    );
    Ok(())
}

#[test]
fn strict_operation_insert_replace_is_refused() -> TestResult {
    let path = scratch("replace-operation")?.join("control.db");
    initialize(&path, 2)?;
    let connection = open(&path)?;
    connection.pragma_update(None, "recursive_triggers", false)?;
    connection.execute_batch(
        "UPDATE operation SET state='applying';
        UPDATE source SET progress='new_verified'; UPDATE operation SET state='strict';",
    )?;
    let replacement = connection.execute(
        "INSERT OR REPLACE INTO operation SELECT singleton, operation_id, 'preparing',
         0, plan_id, observation_id, consent_id, registry_snapshot, source_count,
         before_total, after_total, vector_max_entries, 1, vector_busy_timeout_ms
         FROM operation",
        [],
    );
    assert!(
        replacement.is_err(),
        "REPLACE must not reset strict state or policy"
    );
    assert_eq!(
        connection.query_row("SELECT state FROM operation", [], |r| r.get::<_, String>(0))?,
        "strict"
    );
    assert_eq!(
        connection.query_row("SELECT vector_max_bytes FROM operation", [], |r| r
            .get::<_, i64>(0))?,
        1_073_741_824
    );
    Ok(())
}

#[test]
fn hot_journal_requires_explicit_writable_recovery() -> TestResult {
    let path = scratch("hot")?.join("control.db");
    initialize(&path, 257)?;
    assert_eq!(child("before_commit", &path)?.code(), Some(91));
    let journal_path = path.with_extension("db-journal");
    let before_db = fs::read(&path)?;
    let before_journal = fs::read(&journal_path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let result = connection.query_row("SELECT state FROM operation", [], |r| r.get::<_, String>(0));
    let error = result.expect_err("read-only must not recover hot journal");
    println!("read-only hot journal error: {error}");
    assert!(matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.extended_code == 776));
    drop(connection);
    assert_eq!(fs::read(&path)?, before_db);
    assert_eq!(fs::read(&journal_path)?, before_journal);
    // Writable open is a separate, explicit recovery action; no immutable shortcut.
    let connection = open(&path)?;
    assert_eq!(
        connection.query_row("SELECT state FROM operation", [], |r| r.get::<_, String>(0))?,
        "preparing"
    );
    assert_eq!(
        connection.query_row(
            "SELECT count(*) FROM source WHERE progress='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        257
    );
    assert!(!journal_path.exists());
    Ok(())
}

#[test]
fn commit_survives_process_exit_and_restore_is_terminal() -> TestResult {
    let path = scratch("commit")?.join("control.db");
    initialize(&path, 257)?;
    assert_eq!(child("after_commit", &path)?.code(), Some(92));
    let connection = open(&path)?;
    assert_eq!(
        connection.query_row("SELECT state FROM operation", [], |r| r.get::<_, String>(0))?,
        "applying"
    );
    assert_eq!(
        connection.query_row(
            "SELECT count(*) FROM source WHERE progress='new_verified'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        257
    );
    connection.execute("UPDATE operation SET state='restoring'", [])?;
    connection.execute("UPDATE source SET progress='old_verified'", [])?;
    connection.execute("UPDATE operation SET state='restored_legacy'", [])?;
    assert!(
        connection
            .execute("UPDATE operation SET state='strict'", [])
            .is_err()
    );
    // Explicit new adoption may replace the single restored record atomically.
    connection
        .execute_batch("BEGIN IMMEDIATE; DELETE FROM source; DELETE FROM operation; ROLLBACK;")?;
    assert_eq!(
        connection.query_row("SELECT state FROM operation", [], |r| r.get::<_, String>(0))?,
        "restored_legacy"
    );
    Ok(())
}

#[test]
fn native_session_gate_across_processes() -> TestResult {
    let path = scratch("lease")?.join("session.lock");
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)?;
    file.try_lock_shared()?;
    assert!(child("shared_ok", &path)?.success());
    assert!(child("exclusive_busy", &path)?.success());
    file.unlock()?;
    file.try_lock()?;
    assert!(child("shared_busy", &path)?.success());
    assert!(child("exclusive_busy", &path)?.success());
    file.unlock()?;
    assert!(child("exclusive_ok", &path)?.success());
    // Never unlink lockfiles. File still exists after release.
    assert!(File::open(path).is_ok());
    Ok(())
}

// Research excerpt of the existing registry_inspect/lease.rs primitive. Linux
// only: it is not a replacement for that module's VFS/namespace/platform checks.
#[cfg(target_os = "linux")]
fn native_registry_reservation(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    use rusqlite::ffi;
    use std::ffi::CStr;
    use std::ptr;

    let mut uri = String::from("file:");
    for byte in path.to_str().ok_or("UTF-8 scratch path")?.bytes() {
        use std::fmt::Write;
        write!(uri, "%{byte:02X}")?;
    }
    uri.push_str("?immutable=1");
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    assert!(connection.is_readonly("main")?);
    let mut vfs: *mut ffi::sqlite3_vfs = ptr::null_mut();
    // SAFETY: live private connection; control writes exactly one borrowed VFS pointer.
    assert_eq!(
        unsafe {
            ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_VFS_POINTER,
                ptr::from_mut(&mut vfs).cast(),
            )
        },
        ffi::SQLITE_OK
    );
    assert!(!vfs.is_null());
    // SAFETY: SQLite owns the live VFS descriptor and its NUL-terminated name.
    let name = unsafe { (*vfs).zName };
    assert!(!name.is_null());
    // SAFETY: non-null name supplied by that registered native descriptor.
    assert_eq!(unsafe { CStr::from_ptr(name) }.to_bytes(), b"unix");
    let mut file: *mut ffi::sqlite3_file = ptr::null_mut();
    // SAFETY: FILE_POINTER writes one borrowed file pointer owned by this connection.
    assert_eq!(
        unsafe {
            ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_FILE_POINTER,
                ptr::from_mut(&mut file).cast(),
            )
        },
        ffi::SQLITE_OK
    );
    assert!(!file.is_null());
    // SAFETY: file remains live, aligned and private until owning connection drop.
    let methods = unsafe { (*file).pMethods };
    assert!(!methods.is_null());
    // SAFETY: native method table has v1 fields; check version before v2 xShmMap.
    assert!(unsafe { (*methods).iVersion >= 2 && (*methods).xShmMap.is_some() });
    // SAFETY: xLock is a v1 field of this checked, live native method table.
    let lock = unsafe { (*methods).xLock }.ok_or("native xLock")?;
    for level in [ffi::SQLITE_LOCK_SHARED, ffi::SQLITE_LOCK_EXCLUSIVE] {
        // SAFETY: same native protocol/file as existing product inspector. The
        // immutable pager does not alter its locks; connection xClose releases them.
        assert_eq!(unsafe { lock(file, level) }, ffi::SQLITE_OK);
    }
    Ok(connection)
}

#[cfg(target_os = "linux")]
#[test]
fn registry_reservation_has_no_wal_transaction_effects() -> TestResult {
    let path = scratch("registry")?.join("registry.db");
    let connection = Connection::open(&path)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL; CREATE TABLE registry_probe(id INTEGER);
        INSERT INTO registry_probe VALUES(1);",
    )?;
    drop(connection); // Clean shutdown/checkpoint is fixture setup, not admission.
    assert!(child("wal_transaction", &path)?.success());
    let before = fs::read(&path)?; // Never open a second DB descriptor under POSIX lease.
    assert_eq!(&before[18..20], &[2, 2]); // Persistent WAL format, not DELETE.
    for suffix in ["db-wal", "db-shm", "db-journal"] {
        assert!(!path.with_extension(suffix).exists());
    }
    let reserved = native_registry_reservation(&path)?;
    assert!(child("wal_writer_busy", &path)?.success());
    assert_eq!(
        reserved.query_row("SELECT count(*) FROM registry_probe", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    // Prove that immutable pager initialization did not release the native lease.
    assert!(child("wal_writer_busy", &path)?.success());
    for suffix in ["db-wal", "db-shm", "db-journal"] {
        assert!(!path.with_extension(suffix).exists());
    }
    drop(reserved);
    assert_eq!(fs::read(&path)?, before);
    assert!(child("wal_writer_ok", &path)?.success());
    Ok(())
}
