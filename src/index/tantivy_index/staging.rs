//! Bounds the bytes admitted to an unpublished index and its merge workers.

use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, TerminatingWrite,
    WatchCallback, WatchHandle, WritePtr,
};

use crate::platform::file_lease::ExclusiveFileLease;

pub(super) struct WriteBudget {
    remaining: AtomicU64,
    exhausted: AtomicBool,
    // Tantivy can report one failed worker before the remaining handles
    // finish dropping. Those workers still own this generation.
    _lease: ExclusiveFileLease,
}

impl std::fmt::Debug for WriteBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteBudget")
            .field("remaining", &self.remaining)
            .field("exhausted", &self.exhausted)
            .finish_non_exhaustive()
    }
}

impl WriteBudget {
    pub(super) fn new(bytes: u64, lease: ExclusiveFileLease) -> Self {
        Self {
            remaining: AtomicU64::new(bytes),
            exhausted: AtomicBool::new(false),
            _lease: lease,
        }
    }

    pub(super) fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Relaxed)
    }

    fn charge(&self, bytes: usize) -> io::Result<()> {
        if self
            .remaining
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(bytes as u64)
            })
            .is_err()
        {
            self.exhausted.store(true, Ordering::Relaxed);
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "staged Tantivy write exceeds its disk reservation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct StagingDirectory {
    inner: MmapDirectory,
    budget: Arc<WriteBudget>,
}

impl StagingDirectory {
    pub(super) fn new(inner: MmapDirectory, budget: Arc<WriteBudget>) -> Self {
        Self { inner, budget }
    }
}

impl Directory for StagingDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        // A removed segment can still be mapped by a reader. Retain its
        // charge for the entire stage, including across atomic replacements.
        self.inner.delete(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let writer = self.inner.open_write(path)?;
        Ok(BufWriter::new(Box::new(BoundedWriter {
            writer,
            budget: self.budget.clone(),
        })))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.budget.charge(data.len())?;
        self.inner.atomic_write(path, data)
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(callback)
    }
}

struct BoundedWriter {
    writer: WritePtr,
    budget: Arc<WriteBudget>,
}

impl Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.budget.charge(buf.len())?;
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl TerminatingWrite for BoundedWriter {
    fn terminate_ref(&mut self, token: AntiCallToken) -> io::Result<()> {
        self.writer.terminate_ref(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, OpenOptions};

    #[test]
    fn competing_writes_share_the_budget_and_hold_the_lease() {
        let scratch = tempfile::tempdir().unwrap();
        let lease_path = scratch.path().join(".read.lock");
        let lease =
            ExclusiveFileLease::try_acquire(File::create_new(&lease_path).unwrap()).unwrap();
        let directory = StagingDirectory::new(
            MmapDirectory::open(scratch.path()).unwrap(),
            Arc::new(WriteBudget::new(128 * 1024, lease)),
        );
        let competing = directory.clone();
        let (atomic, stream) = std::thread::scope(|threads| {
            let atomic = threads
                .spawn(move || competing.atomic_write(Path::new("atomic"), &vec![1; 80 * 1024]));
            let mut stream = directory.open_write(Path::new("stream")).unwrap();
            let stream = stream
                .write_all(&vec![2; 80 * 1024])
                .and_then(|()| stream.terminate());
            (atomic.join().unwrap(), stream)
        });
        let committed = match (atomic, stream) {
            (Ok(()), Err(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                "atomic"
            }
            (Err(error), Ok(())) => {
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                "stream"
            }
            results => panic!("one of the competing writes had to be refused: {results:?}"),
        };
        assert_eq!(
            fs::metadata(scratch.path().join(committed)).unwrap().len(),
            80 * 1024
        );
        directory.delete(Path::new(committed)).unwrap();
        assert_eq!(
            directory
                .atomic_write(Path::new("replacement"), &vec![3; 80 * 1024])
                .unwrap_err()
                .kind(),
            io::ErrorKind::StorageFull
        );
        let handle = directory.open_write(Path::new("alive")).unwrap();
        drop(directory);
        let competing = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lease_path)
            .unwrap();
        let mut lock = fd_lock::RwLock::new(competing);
        assert_eq!(
            lock.try_write().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(handle);
        assert!(lock.try_write().is_ok());
    }
}
