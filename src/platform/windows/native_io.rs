//! Bounded application accounting around Mio/Tokio-owned native operations.
//!
//! No OVERLAPPED storage, packet dispatch or completion-port implementation lives
//! here. The private runtime stays alive until dependency retirement guards and
//! dedicated final-drain workers have finished.
use super::connection::NativePipe;
use std::io;
use std::os::windows::io::OwnedHandle;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use tokio::runtime::{Builder, Handle, Runtime};

fn locked<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
fn failure(message: &'static str) -> io::Error {
    io::Error::other(message)
}

pub(super) struct HandleToken {
    native: Mutex<Weak<NativePipe>>,
    native_done: AtomicBool,
    initializing: AtomicBool,
    aborting: AtomicBool,
    helpers: AtomicUsize,
    pub(super) write_active: AtomicBool,
    charges: Mutex<[Option<Arc<dyn Send + Sync>>; 2]>,
    root: Weak<Inner>,
}
impl HandleToken {
    #[cfg(test)]
    pub(super) async fn test_native_terminal(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !self.terminal() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual dependency owner must retire");
    }
    pub(super) fn service(&self) -> io::Result<Service> {
        self.root
            .upgrade()
            .map(|inner| Service { inner })
            .ok_or_else(|| failure("transport runtime has retired"))
    }
    pub(super) fn native(&self) -> io::Result<Arc<NativePipe>> {
        locked(&self.native)
            .upgrade()
            .ok_or_else(|| failure("native pipe has retired"))
    }
    pub(super) fn is_aborting(&self) -> bool {
        self.aborting.load(Ordering::Acquire)
    }
    pub(super) fn report_fault(&self, message: &'static str) {
        if let Some(root) = self.root.upgrade() {
            *locked(&root.fault) = Some(message);
            root.accepting.store(false, Ordering::Release);
            root.capacity.notify_waiters();
        }
    }
    pub(super) fn retain_helper(&self) -> io::Result<Arc<NativePipe>> {
        let native = self.native()?;
        self.helpers.fetch_add(1, Ordering::AcqRel);
        Ok(native)
    }
    pub(super) fn release_helper(&self) {
        self.helpers.fetch_sub(1, Ordering::AcqRel);
    }
    pub(super) fn writes_idle(&self) -> bool {
        !self.write_active.load(Ordering::Acquire)
    }
    pub(super) fn abort(&self) {
        self.aborting.store(true, Ordering::Release);
        if let Ok(native) = self.native() {
            native.cancel();
        }
    }
    pub(super) fn disconnect_failed(&self) {
        if let Ok(native) = self.native() {
            native.disconnect();
        }
    }
    fn terminal(&self) -> bool {
        !self.initializing.load(Ordering::Acquire)
            && self.native_done.load(Ordering::Acquire)
            && self.helpers.load(Ordering::Acquire) == 0
    }
    pub(super) fn retain(&self, request: bool, charge: Arc<dyn Send + Sync>) -> io::Result<()> {
        let mut charges = locked(&self.charges);
        if self.is_aborting() {
            return Err(failure("native connection is aborting"));
        }
        let previous = charges[usize::from(request)].replace(charge);
        drop(charges);
        drop(previous);
        Ok(())
    }
    pub(super) fn request_completed(&self) {
        let previous = locked(&self.charges)[1].take();
        drop(previous);
    }
    fn retired(&self) {
        let previous = std::mem::take(&mut *locked(&self.charges));
        drop(previous);
    }
}

/// The last field of `Mio::Inner` owns this before registration/first I/O.
/// Charges remain reachable from this guard even if application work unwinds.
pub(super) struct RetirementGuard(pub Arc<HandleToken>);
impl Drop for RetirementGuard {
    fn drop(&mut self) {
        // Mio has already destroyed its HANDLE, OVERLAPPEDs and buffer pool.
        self.0.native_done.store(true, Ordering::Release);
        if let Some(root) = self.0.root.upgrade() {
            root.capacity.notify_waiters();
        }
    }
}
pub(super) trait PhysicalOwner: Send + Sync {
    fn abort(&self);
    fn retire_ready(&self) -> io::Result<bool>;
    fn take_native(&self) -> Option<Arc<NativePipe>> {
        None
    }
}
struct Record {
    token: Arc<HandleToken>,
    owner: Option<Arc<dyn PhysicalOwner>>,
}
struct Inner {
    records: Mutex<Vec<Option<Record>>>,
    accepting: AtomicBool,
    fault: Mutex<Option<&'static str>>,
    capacity: tokio::sync::Notify,
    runtime: Handle,
}

pub(super) struct Port {
    inner: Arc<Inner>,
    runtime: Option<Runtime>,
}
#[derive(Clone)]
pub(super) struct Service {
    inner: Arc<Inner>,
}
pub(super) struct Reservation {
    inner: Arc<Inner>,
    token: Arc<HandleToken>,
}
impl Port {
    pub(super) fn new(capacity: usize, stack_bytes: usize) -> io::Result<Self> {
        if capacity == 0 || stack_bytes == 0 {
            return Err(failure("invalid transport runtime capacity"));
        }
        let mut records = Vec::new();
        records
            .try_reserve_exact(capacity)
            .map_err(|_| failure("reserve physical handle arena"))?;
        records.resize_with(capacity, || None);
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("docsbase-pipe-io")
            .thread_stack_size(stack_bytes)
            .enable_io()
            .enable_time()
            .build()?;
        let inner = Arc::new(Inner {
            records: Mutex::new(records),
            accepting: AtomicBool::new(true),
            fault: Mutex::new(None),
            capacity: tokio::sync::Notify::new(),
            runtime: runtime.handle().clone(),
        });
        Ok(Self {
            inner,
            runtime: Some(runtime),
        })
    }
    pub(super) fn service(&self) -> Service {
        Service {
            inner: self.inner.clone(),
        }
    }
    #[cfg(test)]
    pub(super) fn retire(&self, token: &Arc<HandleToken>) -> bool {
        self.service().retire(token)
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        if locked(&self.inner.records).iter().any(Option::is_some) {
            return Err(failure("transport runtime retirement incomplete"));
        }
        // No dependency owner or helper remains. Runtime shutdown joins the
        // dedicated I/O worker; no application task/blocking work runs on it.
        drop(self.runtime.take());
        Ok(())
    }
}
impl Service {
    pub(super) fn capacity_changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.inner.capacity.notified()
    }
    pub(super) fn reserve(&self) -> io::Result<Reservation> {
        let mut records = locked(&self.inner.records);
        if !self.inner.accepting.load(Ordering::Acquire) || locked(&self.inner.fault).is_some() {
            return Err(failure("transport runtime is closing"));
        }
        let slot = records
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        let token = Arc::new(HandleToken {
            native: Mutex::new(Weak::new()),
            native_done: AtomicBool::new(true),
            initializing: AtomicBool::new(true),
            aborting: AtomicBool::new(false),
            helpers: AtomicUsize::new(0),
            write_active: AtomicBool::new(false),
            charges: Mutex::new([None, None]),
            root: Arc::downgrade(&self.inner),
        });
        *slot = Some(Record {
            token: token.clone(),
            owner: None,
        });
        Ok(Reservation {
            inner: self.inner.clone(),
            token,
        })
    }
    pub(super) fn attach(
        &self,
        token: &Arc<HandleToken>,
        owner: Arc<dyn PhysicalOwner>,
    ) -> io::Result<()> {
        let previous = {
            let mut records = locked(&self.inner.records);
            let record = records
                .iter_mut()
                .flatten()
                .find(|record| Arc::ptr_eq(&record.token, token))
                .ok_or_else(|| failure("native owner registration disappeared"))?;
            record.owner.replace(owner)
        };
        drop(previous);
        Ok(())
    }
    pub(super) fn accepted(&self, token: &Arc<HandleToken>) -> io::Result<Arc<NativePipe>> {
        let owner = locked(&self.inner.records)
            .iter()
            .flatten()
            .find(|record| Arc::ptr_eq(&record.token, token))
            .and_then(|record| record.owner.clone())
            .ok_or_else(|| failure("listening owner disappeared"))?;
        owner
            .take_native()
            .ok_or_else(|| failure("listening handle already transferred"))
    }
    pub(super) fn retire(&self, token: &Arc<HandleToken>) -> bool {
        if !token.terminal() {
            return false;
        }
        let removed = {
            let mut records = locked(&self.inner.records);
            records
                .iter_mut()
                .find(|entry| {
                    entry
                        .as_ref()
                        .is_some_and(|record| Arc::ptr_eq(&record.token, token))
                })
                .and_then(Option::take)
        };
        if removed.is_none() {
            return false;
        }
        token.retired();
        drop(removed);
        self.inner.capacity.notify_waiters();
        true
    }
    pub(super) fn quiesce(&self) {
        self.inner.accepting.store(false, Ordering::Release);
        self.inner.capacity.notify_waiters();
    }
    pub(super) fn abort(&self) {
        self.quiesce();
        let length = locked(&self.inner.records).len();
        for index in 0..length {
            let item = locked(&self.inner.records)[index]
                .as_ref()
                .map(|record| (record.token.clone(), record.owner.clone()));
            if let Some((token, owner)) = item {
                token.abort();
                if let Some(owner) = owner {
                    owner.abort();
                }
            }
        }
    }
    pub(super) fn reap(&self) -> io::Result<usize> {
        if let Some(message) = *locked(&self.inner.fault) {
            return Err(failure(message));
        }
        let length = locked(&self.inner.records).len();
        let mut retained = 0;
        for index in 0..length {
            let item = locked(&self.inner.records)[index]
                .as_ref()
                .map(|record| (record.token.clone(), record.owner.clone()));
            if let Some((token, owner)) = item {
                if token.is_aborting() {
                    if let Some(owner) = owner {
                        owner.abort();
                        if !owner.retire_ready()? {
                            retained += 1;
                            continue;
                        }
                    }
                    if !self.retire(&token) {
                        retained += 1;
                    }
                } else {
                    retained += 1;
                }
            }
        }
        Ok(retained)
    }
}
impl Reservation {
    pub(super) fn associate(
        self,
        handle: OwnedHandle,
        server: bool,
        capacity: usize,
    ) -> io::Result<(Arc<HandleToken>, Arc<NativePipe>)> {
        if !self.inner.accepting.load(Ordering::Acquire) || self.token.is_aborting() {
            return Err(failure("transport runtime is closing"));
        }
        self.token.native_done.store(false, Ordering::Release);
        let _entered = self.inner.runtime.enter();
        let pipe = Arc::new(NativePipe::new(
            handle,
            server,
            capacity,
            self.token.clone(),
        )?);
        *locked(&self.token.native) = Arc::downgrade(&pipe);
        Ok((self.token.clone(), pipe))
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.token.initializing.store(false, Ordering::Release);
        if locked(&self.token.native).strong_count() == 0 {
            self.token.aborting.store(true, Ordering::Release);
            // A failed open has no HANDLE, dependency owner or I/O to reap.
            // Release that reservation before the caller's next native retry.
            // An association failure with a still-live guard is not terminal
            // and remains charged for the outer reaper.
            Service {
                inner: self.inner.clone(),
            }
            .retire(&self.token);
        }
        self.inner.capacity.notify_waiters();
    }
}
