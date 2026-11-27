//! Path of the dynamic module that owns a borrowed static symbol.
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[allow(unsafe_code)] // POSIX dynamic loader query for a live symbol.
pub(crate) fn path_of<T>(symbol: &T) -> io::Result<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    // SAFETY: symbol is live; dladdr only looks up its address in the loaded
    // image table and writes a Dl_info into the supplied aligned storage.
    let found = unsafe { libc::dladdr(std::ptr::from_ref(symbol).cast(), info.as_mut_ptr()) };
    if found == 0 {
        return Err(io::Error::other("address is outside the loaded modules"));
    }
    // SAFETY: A successful dladdr initialized the output. If present, the
    // filename is NUL-terminated and valid while the symbol's module is loaded.
    let path = unsafe {
        let info = info.assume_init();
        if info.dli_fname.is_null() {
            return Err(io::Error::other("loaded module has no path"));
        }
        PathBuf::from(OsStr::from_bytes(CStr::from_ptr(info.dli_fname).to_bytes()))
    };
    // The loader may retain an original relative spelling. Resolving it
    // against the current directory could identify a different library.
    if !path.is_absolute() {
        return Err(io::Error::other("loaded module path is relative"));
    }
    Ok(path)
}

#[cfg(windows)]
#[allow(unsafe_code)] // Windows dynamic loader query for a live symbol.
pub(crate) fn path_of<T>(symbol: &T) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };

    const CAPACITY: u32 = 32_768;
    let mut module = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS treats the second parameter as a symbol address,
    // not a UTF-16 string. The caller holds the symbol's module live and
    // UNCHANGED_REFCOUNT means there is no acquired handle to close.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            std::ptr::from_ref(symbol).cast(),
            &raw mut module,
        )
    };
    if found == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = vec![0u16; CAPACITY as usize];
    // SAFETY: module was obtained from the loader and remains live. The
    // writable UTF-16 buffer has room for CAPACITY elements.
    let length = unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), CAPACITY) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= CAPACITY {
        return Err(io::Error::other(
            "loaded module path exceeds the Windows path bound",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(&name[..length as usize])))
}

pub(crate) fn matches_path(loaded: &Path, requested: &Path) -> bool {
    #[cfg(unix)]
    {
        loaded.as_os_str() == requested.as_os_str()
    }
    #[cfg(windows)]
    {
        windows_spelling(loaded) == windows_spelling(requested)
    }
}

#[cfg(windows)]
fn windows_spelling(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    // Ignore only Windows' verbatim spelling of an otherwise identical
    // path. Do not dereference loader names, or fold case/non-Unicode bytes.
    const VERBATIM: [u16; 4] = [92, 92, 63, 92];
    const VERBATIM_UNC: [u16; 8] = [92, 92, 63, 92, 85, 78, 67, 92];
    let mut name = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if name.starts_with(&VERBATIM_UNC) {
        name.drain(2..VERBATIM_UNC.len());
    } else if name.starts_with(&VERBATIM) && name.get(5) == Some(&u16::from(b':')) {
        name.drain(..VERBATIM.len());
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_loader_comparison_preserves_native_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let one = Path::new(OsStr::from_bytes(b"/lib/onnx-\xff.so"));
        let other = Path::new(OsStr::from_bytes(b"/lib/onnx-\xfe.so"));
        assert!(matches_path(one, one));
        assert!(!matches_path(one, other));
        assert!(!matches_path(
            Path::new("/lib/runtime.so"),
            Path::new("/lib/Runtime.so")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_loader_comparison_accepts_verbatim_spelling_only() {
        for (normal, verbatim) in [
            (r"C:\models\ort.dll", r"\\?\C:\models\ort.dll"),
            (r"\\host\share\ort.dll", r"\\?\UNC\host\share\ort.dll"),
        ] {
            assert!(matches_path(Path::new(normal), Path::new(verbatim)));
        }
        assert!(!matches_path(
            Path::new(r"C:\models\ort.dll"),
            Path::new(r"C:\Models\ort.dll")
        ));
    }

    #[test]
    fn native_module_path_is_the_owner_of_a_live_symbol() {
        static OWNED_BY_THIS_BINARY: u8 = 73;
        let loaded = path_of(&OWNED_BY_THIS_BINARY).unwrap();
        let expected = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(loaded.canonicalize().unwrap(), expected);
    }
}
