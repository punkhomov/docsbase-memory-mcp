//! Prepared operational metadata readers. No connection is opened on checkout.

use std::ops::Deref;
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use crate::config::schema::{Capabilities, Resolved, SettingsSpec};
use crate::error::{Error, Result};
use crate::store::Db;
use crate::store::settings::{DbSettings, DbSpec, ReadPoolSettings, ReadPoolSpec};

/// A bounded set of independently borrowed read-only metadata connections.
pub struct ReadPool {
    slots: Vec<Mutex<Db>>,
    state: Mutex<State>,
    returned: Condvar,
    settings: Resolved<ReadPoolSettings>,
}

struct State {
    idle: Vec<usize>,
    unusable: bool,
}

impl ReadPool {
    /// Prepares exactly the supplied capacity after the caller's writer setup.
    /// No migration, WAL negotiation, fallback policy or lazy open occurs here.
    ///
    /// # Errors
    /// Revalidates imported policies before filesystem access; returns startup
    /// allocation/open/schema errors without retaining partially opened handles.
    pub fn prepare(
        cache: &Path,
        db_policy: &Resolved<DbSettings>,
        pool_policy: &Resolved<ReadPoolSettings>,
    ) -> Result<Self> {
        let capabilities = Capabilities::current();
        DbSpec::validate(db_policy.settings(), &capabilities)?;
        ReadPoolSpec::validate(pool_policy.settings(), &capabilities)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(pool_policy.settings().max_connections)
            .map_err(|err| Error::internal_with_source("reserve metadata read pool", err))?;
        let mut idle = Vec::new();
        idle.try_reserve_exact(pool_policy.settings().max_connections)
            .map_err(|err| Error::internal_with_source("reserve metadata read pool slots", err))?;
        for index in 0..pool_policy.settings().max_connections {
            slots.push(Mutex::new(Db::open_readonly_with_settings(
                cache, db_policy,
            )?));
            idle.push(index);
        }
        Ok(Self {
            slots,
            state: Mutex::new(State {
                idle,
                unusable: false,
            }),
            returned: Condvar::new(),
            settings: pool_policy.clone(),
        })
    }

    /// Immutable capacity policy and its caller-supplied provenance.
    #[must_use]
    pub const fn settings(&self) -> &Resolved<ReadPoolSettings> {
        &self.settings
    }

    /// Borrows one prepared reader until the lease drops. The caller owns the
    /// absolute deadline; spurious wakes never restart an internal timeout.
    /// Do not retain a lease across provider calls or expensive CPU work.
    ///
    /// # Errors
    /// Admission failure when the deadline elapsed or a returned connection
    /// could not end its transaction. No replacement handle is opened.
    pub fn acquire(&self, deadline: Instant) -> Result<ReadLease<'_>> {
        self.acquire_with(deadline, || {})
    }

    // Private deterministic synchronization seam; invoked with state locked
    // immediately before Condvar atomically releases it and starts waiting.
    fn acquire_with(
        &self,
        deadline: Instant,
        mut before_wait: impl FnMut(),
    ) -> Result<ReadLease<'_>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if state.unusable {
                return Err(Error::Admission {
                    message: "metadata read pool is unusable after transaction cleanup failure; restart the daemon".to_owned(),
                });
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(Error::Admission {
                    message: "metadata read pool acquisition deadline elapsed".to_owned(),
                });
            };
            if remaining.is_zero() {
                return Err(Error::Admission {
                    message: "metadata read pool acquisition deadline elapsed".to_owned(),
                });
            }
            if let Some(index) = state.idle.pop() {
                drop(state);
                let db = self.slots[index]
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                return Ok(ReadLease {
                    db,
                    returned: ReturnSlot {
                        pool: self,
                        index,
                        clean: false,
                    },
                });
            }
            before_wait();
            (state, _) = self
                .returned
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Exclusive ownership of one prepared reader. Return releases SQL snapshots,
/// including when the caller unwinds; a pool outlives every borrowed lease.
pub struct ReadLease<'pool> {
    // Field order matters: unlock the connection before advertising its slot.
    db: MutexGuard<'pool, Db>,
    returned: ReturnSlot<'pool>,
}

struct ReturnSlot<'pool> {
    pool: &'pool ReadPool,
    index: usize,
    clean: bool,
}

impl Deref for ReadLease<'_> {
    type Target = Db;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

impl Drop for ReadLease<'_> {
    fn drop(&mut self) {
        self.returned.clean = self.db.connection().is_autocommit()
            || self.db.connection().execute_batch("ROLLBACK").is_ok();
    }
}

// Runs after ReadLease.db has unlocked, including during caller unwind. An
// available index therefore always refers to an unlocked slot; checkout never
// waits on a connection mutex after leaving its deadline-aware queue.
impl Drop for ReturnSlot<'_> {
    fn drop(&mut self) {
        let mut state = self
            .pool
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.clean && !state.unusable {
            state.idle.push(self.index);
            self.pool.returned.notify_one();
        } else {
            state.unusable = true;
            self.pool.returned.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(300)
    }

    #[test]
    fn waiter_observes_capacity_and_wakes_on_return() {
        let cache = tempfile::tempdir().expect("cache");
        let db = crate::store::settings::standalone().expect("db policy");
        let _writer = Db::open_with_settings(cache.path(), &db).expect("writer");
        let valid = crate::store::settings::standalone_read_pool().expect("pool policy");
        let mut raw = valid.settings().clone();
        raw.max_connections = 1;
        let policy = Resolved::new(raw, valid.provenance().clone());
        let pool = ReadPool::prepare(cache.path(), &db, &policy).expect("pool");
        let reader = pool.acquire(deadline()).expect("only reader");
        std::thread::scope(|scope| {
            let (enter, entered) = mpsc::sync_channel(1);
            let borrowed = &pool;
            let worker = scope.spawn(move || {
                borrowed
                    .acquire_with(deadline(), || {
                        let _ = enter.try_send(());
                    })
                    .map(|reader| reader.connection().is_autocommit())
            });
            entered
                .recv_timeout(Duration::from_secs(300))
                .expect("waiting after observing empty pool");
            drop(reader);
            assert!(worker.join().expect("join").expect("reused reader"));
        });
    }

    #[test]
    fn exhausted_pool_respects_caller_deadline() {
        let cache = tempfile::tempdir().expect("cache");
        let db = crate::store::settings::standalone().expect("db policy");
        let _writer = Db::open_with_settings(cache.path(), &db).expect("writer");
        let valid = crate::store::settings::standalone_read_pool().expect("pool policy");
        let mut raw = valid.settings().clone();
        raw.max_connections = 1;
        let policy = Resolved::new(raw, valid.provenance().clone());
        let pool = ReadPool::prepare(cache.path(), &db, &policy).expect("pool");
        let _reader = pool.acquire(deadline()).expect("only reader");
        // A short caller budget exercises Condvar timeout; no assertion about
        // wall-clock duration or scheduler speed is made.
        assert!(matches!(
            pool.acquire(Instant::now() + Duration::from_millis(1)),
            Err(Error::Admission { .. })
        ));
    }

    #[test]
    fn malformed_imported_policy_is_rejected_before_filesystem_access() {
        let valid = crate::store::settings::standalone_read_pool().expect("pool policy");
        let db = crate::store::settings::standalone().expect("db policy");
        let mut zero = valid.settings().clone();
        zero.max_connections = 0;
        let mut huge = valid.settings().clone();
        huge.max_connections = usize::MAX;
        for raw in [zero, huge] {
            let policy = Resolved::new(raw, valid.provenance().clone());
            let cache = tempfile::tempdir().expect("cache");
            let absent = cache.path().join("absent");
            assert!(ReadPool::prepare(&absent, &db, &policy).is_err());
            assert!(!absent.exists());
        }
        let mut raw = db.settings().clone();
        raw.wal_attempts = 0;
        let db = Resolved::new(raw, db.provenance().clone());
        let cache = tempfile::tempdir().expect("cache");
        let absent = cache.path().join("absent");
        assert!(ReadPool::prepare(&absent, &db, &valid).is_err());
        assert!(!absent.exists());
    }
}
