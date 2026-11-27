//! Lazily owned CPU executor shared by native search stages and ANN builds.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};

#[derive(Debug)]
pub(crate) struct CpuPool {
    threads: NonZeroUsize,
    pool: Mutex<Option<Arc<rayon::ThreadPool>>>,
}

impl CpuPool {
    pub(crate) const fn new(threads: NonZeroUsize) -> Self {
        Self {
            threads,
            pool: Mutex::new(None),
        }
    }

    pub(crate) fn install<T: Send>(&self, work: impl FnOnce() -> T + Send) -> Result<T> {
        Ok(self.acquire()?.install(work))
    }

    pub(super) fn acquire(&self) -> Result<Arc<rayon::ThreadPool>> {
        let pool = {
            let mut slot = self
                .pool
                .lock()
                .map_err(|_| Error::internal("native CPU pool lock poisoned"))?;
            if let Some(pool) = slot.as_ref() {
                Arc::clone(pool)
            } else {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(self.threads.get())
                    .thread_name(|index| format!("docsbase-native-{index}"))
                    .build()
                    .map_err(|error| Error::internal_with_source("build native CPU pool", error))?;
                let pool = Arc::new(pool);
                *slot = Some(Arc::clone(&pool));
                pool
            }
        };
        Ok(pool)
    }
}

impl PartialEq for CpuPool {
    fn eq(&self, other: &Self) -> bool {
        // Compare policy. Initialized workers do not change config identity.
        self.threads == other.threads
    }
}

impl Eq for CpuPool {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_parallel_iterators_use_the_owned_pool() {
        use rayon::prelude::*;
        let pool = CpuPool::new(NonZeroUsize::new(2).unwrap());
        assert!(pool.pool.lock().unwrap().is_none());
        let workers = pool
            .install(|| {
                (0..16)
                    .into_par_iter()
                    .map(|_| rayon::current_num_threads())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(workers, vec![2; 16]);
    }
}
