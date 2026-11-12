//! Narrow native `SQLite` lock seam. The immutable pager and native lease share
//! one connection/file; there is no custom VFS registration or second file open.
#![allow(unsafe_code)] // SQLite FILE_POINTER/xLock have no safe rusqlite adapter.

use std::ffi::CStr;
use std::path::Path;
use std::ptr;

use rusqlite::{Connection, OpenFlags, ffi};

use super::{UnavailableReason as Why, sqlite_failure};

type Observed<T> = std::result::Result<T, Why>;

#[derive(Clone, Copy)]
enum NativeVfs {
    Unix,
    Windows,
}

/// The connection owns and closes the native file, including partial locks.
/// No raw pointer or connection escapes the inspector's synchronous scope.
pub(super) struct LockedConnection {
    connection: Connection,
    vfs: NativeVfs,
}
impl LockedConnection {
    pub(super) fn open(path: &Path, before_lock: impl FnOnce()) -> Observed<Self> {
        let uri = immutable_uri(path)?;
        let connection = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sqlite_failure)?;
        // No CREATE: the OS descriptor supports exclusive locks, while immutable
        // makes the SQLite pager read-only. Opening reads only a bootstrap header;
        // pPage1 is unset until the first SQL query (pinned by interleaving tests).
        if !connection.is_readonly("main").map_err(sqlite_failure)? {
            return Err(Why::UnsupportedVfs);
        }
        before_lock();
        let vfs = native_vfs(&connection)?;
        lock_file(&connection, vfs)?;
        let locked = Self { connection, vfs };
        locked.check_namespace()?;
        Ok(locked)
    }

    pub(super) const fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(super) fn configure(&self, value_bytes: i32, cache_kib: i32) -> Observed<()> {
        // SAFETY: the connection is live, private and has no statement in flight.
        // The caller admitted positive representable limits before construction.
        unsafe {
            ffi::sqlite3_limit(
                self.connection.handle(),
                ffi::SQLITE_LIMIT_LENGTH,
                value_bytes,
            );
        }
        self.connection
            .pragma_update(None, "cache_size", -cache_kib)
            .map_err(sqlite_failure)?;
        // Inspection deliberately does not retain mmap views of document payload.
        self.connection
            .pragma_update(None, "mmap_size", 0)
            .map_err(sqlite_failure)?;
        Ok(())
    }

    pub(super) fn check_namespace(&self) -> Observed<()> {
        let mut moved = 0;
        // SAFETY: SQLite expects a writable int for HAS_MOVED. The private
        // connection and native filename remain alive throughout this call.
        let rc = unsafe {
            ffi::sqlite3_file_control(
                self.connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_HAS_MOVED,
                std::ptr::from_mut(&mut moved).cast(),
            )
        };
        match (rc, moved, self.vfs) {
            // Native win32 opens with SHARE_READ|SHARE_WRITE, without
            // SHARE_DELETE: file replacement/deletion is prohibited while open.
            // Do not generalize this exception to arbitrary VFS implementations.
            (ffi::SQLITE_OK, 0, _) | (ffi::SQLITE_NOTFOUND, _, NativeVfs::Windows) => Ok(()),
            (ffi::SQLITE_OK, _, _) => Err(Why::Changed),
            _ => Err(Why::UnsupportedVfs),
        }
    }
}

fn lock_file(connection: &Connection, vfs: NativeVfs) -> Observed<()> {
    let file = native_file(connection)?;
    // SAFETY: FILE_POINTER borrows this live, private connection's native
    // allocation. Only this thread accesses it; callbacks are checked below.
    let methods = unsafe { (*file).pMethods };
    if methods.is_null() {
        return Err(Why::UnsupportedVfs);
    }
    // In bundled SQLite's native Unix tables, only posixIoMethods has xShmMap.
    // macOS's "unix" finder can also choose no-lock/dotfile/AFP/NFS methods;
    // refuse those BEFORE xLock (dotfile locking itself creates a directory).
    // This is a pinned backend discriminator, not a generic VFS capability claim.
    // SAFETY: the live native table has its v1 fields; short-circuit iVersion
    // before accessing its v2 field. We inspect, never invoke, xShmMap.
    if matches!(vfs, NativeVfs::Unix)
        && unsafe { (*methods).iVersion < 2 || (*methods).xShmMap.is_none() }
    {
        return Err(Why::UnsupportedVfs);
    }
    // SAFETY: the native file supplies a live sqlite3_io_methods table.
    let lock = unsafe { (*methods).xLock }.ok_or(Why::UnsupportedVfs)?;
    for level in [ffi::SQLITE_LOCK_SHARED, ffi::SQLITE_LOCK_EXCLUSIVE] {
        // SAFETY: calls follow SQLite's native lock protocol on this exact
        // file. The immutable pager does not alter these locks. On failure,
        // connection drop invokes native xClose and releases partial locks.
        status(unsafe { lock(file, level) })?;
    }
    Ok(())
}

fn native_file(connection: &Connection) -> Observed<*mut ffi::sqlite3_file> {
    let mut file: *mut ffi::sqlite3_file = ptr::null_mut();
    // SAFETY: FILE_POINTER writes a borrowed sqlite3_file pointer into this slot.
    // It is used only while the owning private connection is alive.
    status(unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            std::ptr::from_mut(&mut file).cast(),
        )
    })?;
    if file.is_null() {
        return Err(Why::UnsupportedVfs);
    }
    Ok(file)
}
fn native_vfs(connection: &Connection) -> Observed<NativeVfs> {
    let mut vfs: *mut ffi::sqlite3_vfs = ptr::null_mut();
    // SAFETY: VFS_POINTER writes the descriptor used by this live connection.
    status(unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_VFS_POINTER,
            std::ptr::from_mut(&mut vfs).cast(),
        )
    })?;
    if vfs.is_null() {
        return Err(Why::UnsupportedVfs);
    }
    // SAFETY: SQLite VFS descriptors have a live NUL-terminated zName. Null is
    // checked explicitly; unsupported native/plugin backends are refused.
    let name = unsafe { (*vfs).zName };
    if name.is_null() {
        return Err(Why::UnsupportedVfs);
    }
    // SAFETY: the descriptor and its registered name outlive the connection.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"unix" => Ok(NativeVfs::Unix),
        b"win32" => Ok(NativeVfs::Windows),
        _ => Err(Why::UnsupportedVfs),
    }
}
fn status(code: i32) -> Observed<()> {
    match code & 0xff {
        ffi::SQLITE_OK => Ok(()),
        ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED => Err(Why::Busy),
        _ => Err(Why::Io),
    }
}
fn immutable_uri(path: &Path) -> Observed<String> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let path = path.to_str().ok_or(Why::Io)?;
    let mut uri = String::from("file:");
    // Encode separators too: a leading double slash must not become a URI
    // authority. Parameters/fragments, literal percent and backslashes are data;
    // SQLite decodes the filename before handing it to its native VFS.
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b":-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    uri.push_str("?immutable=1");
    Ok(uri)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn alternate_unix_locking_methods_are_refused_without_sidecars() {
        let cache = tempfile::tempdir().unwrap();
        drop(crate::store::Db::open(cache.path()).unwrap());
        let path = cache.path().join(crate::store::DB_FILE);
        let before = std::fs::read(&path).unwrap();
        for vfs in ["unix-none", "unix-dotfile"] {
            let connection = Connection::open_with_flags_and_vfs(
                immutable_uri(&path).unwrap(),
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                vfs,
            )
            .unwrap();
            // These are real alternate method tables that macOS can select for
            // its ordinary "unix" VFS. Exercise the concrete-method guard.
            assert!(matches!(
                lock_file(&connection, NativeVfs::Unix),
                Err(Why::UnsupportedVfs)
            ));
            drop(connection);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
        }
    }
}
