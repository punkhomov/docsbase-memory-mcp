//! CF5b2a Linux-only throwaway fixture; not a product publisher or path capability.
#![cfg(all(test, target_os = "linux"))]

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn private_file(path: &Path, bytes: &[u8]) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    Ok(file)
}

fn fixture(label: &str) -> io::Result<PathBuf> {
    let root = PathBuf::from(std::env::var_os("CF5B2_SCRATCH").expect("explicit scratch root"));
    let dir = root.join(format!("{label}-{}", std::process::id()));
    fs::create_dir(&dir)?;
    fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
    let file = private_file(&dir.join("config.toml"), b"old")?;
    set_probe(&file, b"preserve-me")?;
    file.sync_all()?;
    File::open(&dir)?.sync_all()?;
    Ok(dir)
}

fn set_probe(file: &File, value: &[u8]) -> io::Result<()> {
    // SAFETY: borrowed live fd; static NUL-terminated name; value is readable
    // for exactly value.len() bytes during this synchronous call.
    let result = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            c"user.docsbase.probe".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn read_probe(file: &File, maximum: usize) -> io::Result<Vec<u8>> {
    // SAFETY: live fd, static name, NULL/0 requests only the required length.
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"user.docsbase.probe".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    let size = usize::try_from(size).map_err(|_| io::Error::last_os_error())?;
    if size > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "attribute bound",
        ));
    }
    let mut value = vec![0_u8; size];
    // SAFETY: live fd and name; admitted vector is writable for exactly size.
    let actual = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"user.docsbase.probe".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(actual).ok() != Some(size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "attribute changed",
        ));
    }
    Ok(value)
}

fn target(dir: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join("config.toml"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ordinary singly linked target required",
        ));
    }
    Ok(file)
}

fn prepared_stage(dir: &Path, copy_probe: bool) -> io::Result<File> {
    let source = target(dir)?;
    let stage = private_file(&dir.join("stage"), b"new")?;
    if copy_probe {
        set_probe(&stage, &read_probe(&source, 64)?)?;
    }
    stage.sync_all()?;
    Ok(stage)
}

// Once replacement was attempted, callers must inspect namespace even on Err.
// This fixture returns no success proof when a later parent flush fails.
fn replace(dir: &Path, parent_flush: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    fs::rename(dir.join("stage"), dir.join("config.toml"))?;
    parent_flush()
}

#[test]
fn rename_alone_loses_target_xattr() -> TestResult {
    let dir = fixture("naive")?;
    let source = target(&dir)?;
    let old_inode = source.metadata()?.ino();
    let stage = prepared_stage(&dir, false)?;
    replace(&dir, || File::open(&dir)?.sync_all())?;
    let current = target(&dir)?;
    assert_ne!(old_inode, current.metadata()?.ino());
    assert_eq!(current.metadata()?.ino(), stage.metadata()?.ino());
    assert_eq!(current.metadata()?.mode() & 0o777, 0o600);
    assert_eq!(
        read_probe(&current, 64).unwrap_err().raw_os_error(),
        Some(libc::ENODATA)
    );
    assert_eq!(read_probe(&source, 64)?, b"preserve-me");
    Ok(())
}

#[test]
fn copied_probe_and_open_old_reader_survive_replace() -> TestResult {
    let dir = fixture("copied")?;
    let mut source = target(&dir)?;
    let stage = prepared_stage(&dir, true)?;
    replace(&dir, || File::open(&dir)?.sync_all())?;
    let current = target(&dir)?;
    assert_eq!(read_probe(&current, 64)?, b"preserve-me");
    assert_eq!(current.metadata()?.ino(), stage.metadata()?.ino());
    assert_eq!(current.metadata()?.uid(), source.metadata()?.uid());
    assert_eq!(current.metadata()?.gid(), source.metadata()?.gid());
    let mut old = Vec::new();
    source.read_to_end(&mut old)?;
    assert_eq!(old, b"old");
    assert_eq!(fs::read(dir.join("config.toml"))?, b"new");
    Ok(())
}

#[test]
fn parent_flush_error_does_not_imply_unchanged_source() -> TestResult {
    let dir = fixture("flush-error")?;
    let _stage = prepared_stage(&dir, true)?;
    let error = replace(&dir, || Err(io::Error::from_raw_os_error(libc::EIO))).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EIO));
    assert_eq!(fs::read(dir.join("config.toml"))?, b"new");
    assert!(!dir.join("stage").exists());
    Ok(())
}

#[test]
fn source_process_child() -> TestResult {
    let Some(mode) = std::env::var_os("CF5B2_CHILD") else {
        return Ok(());
    };
    let dir = PathBuf::from(std::env::var_os("CF5B2_CHILD_DIR").expect("child dir"));
    let _stage = prepared_stage(&dir, true)?;
    if mode == "before" {
        std::process::exit(81);
    }
    if mode == "between" {
        replace(&dir, || std::process::exit(83))?;
        return Err("unexpected return after process exit".into());
    }
    assert_eq!(mode, "after");
    replace(&dir, || File::open(&dir)?.sync_all())?;
    std::process::exit(82);
}

#[test]
fn process_exit_leaves_observable_old_or_new_sources() -> TestResult {
    for (mode, code, expected) in [
        ("before", 81, b"old"),
        ("between", 83, b"new"),
        ("after", 82, b"new"),
    ] {
        let dir = fixture(mode)?;
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "source_process_child"])
            .env("CF5B2_CHILD", mode)
            .env("CF5B2_CHILD_DIR", &dir)
            .status()?;
        assert_eq!(status.code(), Some(code));
        assert_eq!(fs::read(dir.join("config.toml"))?, expected);
        assert_eq!(dir.join("stage").exists(), mode == "before");
        assert_eq!(read_probe(&target(&dir)?, 64)?, b"preserve-me");
    }
    Ok(())
}

#[test]
fn missing_target_noreplace_preserves_unexpected_file() -> TestResult {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let dir = fixture("noreplace")?;
    let _stage = prepared_stage(&dir, true)?;
    // This existing file models a source that appeared after an earlier Missing.
    let stage = CString::new(dir.join("stage").as_os_str().as_bytes())?;
    let dest = CString::new(dir.join("config.toml").as_os_str().as_bytes())?;
    // SAFETY: both buffers are live and NUL-terminated; this research fixture
    // uses absolute paths and a private directory. No handle ownership changes.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            stage.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    assert_eq!(result, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EEXIST)
    );
    assert_eq!(fs::read(dir.join("config.toml"))?, b"old");
    assert_eq!(fs::read(dir.join("stage"))?, b"new");
    Ok(())
}

#[test]
fn bound_nofollow_and_hardlink_refuse_before_staging() -> TestResult {
    let dir = fixture("bounds")?;
    assert_eq!(
        read_probe(&target(&dir)?, 2).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    fs::hard_link(dir.join("config.toml"), dir.join("alias"))?;
    assert!(prepared_stage(&dir, true).is_err());
    assert!(!dir.join("stage").exists());
    fs::remove_file(dir.join("config.toml"))?;
    std::os::unix::fs::symlink("alias", dir.join("config.toml"))?;
    assert!(prepared_stage(&dir, true).is_err());
    assert!(!dir.join("stage").exists());
    assert_eq!(fs::read(dir.join("alias"))?, b"old");
    Ok(())
}
