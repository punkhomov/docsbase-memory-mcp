//! A file lock whose owner can move independently of the caller's stack.

use std::{fs::File, io};

#[ouroboros::self_referencing]
pub(crate) struct ExclusiveFileLease {
    lock: fd_lock::RwLock<File>,
    #[borrows(mut lock)]
    #[not_covariant]
    guard: fd_lock::RwLockWriteGuard<'this, File>,
}

impl ExclusiveFileLease {
    pub(crate) fn try_acquire(file: File) -> io::Result<Self> {
        ExclusiveFileLeaseTryBuilder {
            lock: fd_lock::RwLock::new(file),
            guard_builder: fd_lock::RwLock::try_write,
        }
        .try_build()
    }
}

#[cfg(feature = "hybrid-local")]
#[ouroboros::self_referencing]
#[derive(Debug)]
pub(crate) struct SharedFileLease {
    lock: fd_lock::RwLock<File>,
    #[borrows(lock)]
    #[not_covariant]
    guard: fd_lock::RwLockReadGuard<'this, File>,
}

#[cfg(feature = "hybrid-local")]
impl SharedFileLease {
    pub(crate) fn try_acquire(file: File) -> io::Result<Self> {
        SharedFileLeaseTryBuilder {
            lock: fd_lock::RwLock::new(file),
            guard_builder: fd_lock::RwLock::try_read,
        }
        .try_build()
    }
}
