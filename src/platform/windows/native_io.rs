//! Completion-owned Windows operations. No wrapper or dependency owns dirty Drop.
#![allow(unsafe_code)] // Private Win32 seam; each unsafe operation states its ownership proof.
use std::cell::UnsafeCell;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::thread::JoinHandleExt;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
    PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

use crate::ipc::encoding::Region;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
fn failure(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Connect,
    Read,
    Write,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Completion {
    pub bytes: u32,
    pub error: Option<u32>,
}
impl Completion {
    pub(super) fn result(self) -> io::Result<usize> {
        match self.error {
            Some(code) => Err(io::Error::from_raw_os_error(code.cast_signed())),
            None => Ok(self.bytes as usize),
        }
    }
}

#[derive(Default)]
struct State {
    active: bool,
    returned: bool,
    packet: bool,
    accounted: bool,
    cancel_callers: usize,
    terminal: Option<Completion>,
    waker: Option<Waker>,
}

/// Pinned by Arc before submission. Kernel storage never aliases safe Rust views.
#[repr(C)]
pub(super) struct Operation {
    overlapped: UnsafeCell<OVERLAPPED>,
    storage: UnsafeCell<Option<Region>>,
    pointer: *mut u8,
    capacity: usize,
    kind: Kind,
    token: Arc<HandleToken>,
    state: Mutex<State>,
}
// SAFETY: state serializes Rust access. Kernel storage is inaccessible to Rust
// between submission and BOTH return/completion; a raw packet Arc retains it.
unsafe impl Send for Operation {}
// SAFETY: same state/terminal fencing; no public buffer or OVERLAPPED reference.
unsafe impl Sync for Operation {}

impl Operation {
    pub(super) fn new(
        token: Arc<HandleToken>,
        kind: Kind,
        capacity: usize,
    ) -> io::Result<Arc<Self>> {
        let storage = if kind == Kind::Connect {
            None
        } else {
            Some(Region::new(capacity)?)
        };
        let pointer = storage
            .as_ref()
            .map_or(ptr::null_mut(), |region| region.bytes().as_ptr().cast_mut());
        // SAFETY: Win32 OVERLAPPED's initial state is all zero, hEvent=null for IOCP.
        let overlapped = unsafe { std::mem::zeroed() };
        let operation = Arc::new(Self {
            overlapped: UnsafeCell::new(overlapped),
            storage: UnsafeCell::new(storage),
            pointer,
            capacity,
            kind,
            token: token.clone(),
            state: Mutex::new(State::default()),
        });
        let mut slots = locked(&token.operations);
        if slots
            .iter()
            .filter_map(Weak::upgrade)
            .any(|slot| slot.kind == kind)
            || slots.len() == 3
        {
            return Err(failure("native operation slot already reserved"));
        }
        slots.push(Arc::downgrade(&operation));
        Ok(operation)
    }

    /// Copies only before native submission. The same slot cannot be reused
    /// until its previous result was consumed and every cancellation returned.
    pub(super) fn submit(self: &Arc<Self>, bytes: &[u8]) -> io::Result<()> {
        let gate = locked(&self.token.handle);
        if self.token.aborting.load(Ordering::Acquire) {
            return Err(failure("native connection is aborting"));
        }
        let handle = gate
            .as_ref()
            .ok_or_else(|| failure("native pipe is closed"))?
            .as_raw_handle();
        let mut state = locked(&self.state);
        if state.active {
            return Err(failure("native operation is already submitted"));
        }
        let count = if self.kind == Kind::Write {
            bytes.len().min(self.capacity)
        } else {
            self.capacity
        };
        let count = u32::try_from(count).map_err(|_| failure("native buffer exceeds DWORD"))?;
        if self.kind == Kind::Write {
            // SAFETY: Idle slot, unique state lock, checked region length; no kernel user.
            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.pointer, count as usize) };
        }
        // SAFETY: previous packet AND call returned before active became false.
        unsafe { ptr::write(self.overlapped.get(), std::mem::zeroed()) };
        *state = State {
            active: true,
            packet: true,
            accounted: true,
            ..State::default()
        };
        self.token.users.fetch_add(1, Ordering::AcqRel);
        // This exact Arc is the one packet/local-terminal reservation. Installed
        // BEFORE FFI; completion may arrive before the submitting call returns.
        let packet = Arc::into_raw(self.clone());
        drop(state);
        drop(gate);
        // SAFETY: packet + submission Arc retain slot, buffer and physical handle.
        // No Rust lock is held across native submission or completion dispatch.
        let ok = unsafe {
            match self.kind {
                Kind::Connect => ConnectNamedPipe(handle, self.overlapped.get()),
                Kind::Read => ReadFile(
                    handle,
                    self.pointer,
                    count,
                    ptr::null_mut(),
                    self.overlapped.get(),
                ),
                Kind::Write => WriteFile(
                    handle,
                    self.pointer,
                    count,
                    ptr::null_mut(),
                    self.overlapped.get(),
                ),
            }
        };
        // SAFETY: capture thread-local status immediately after failed native call.
        let error = if ok == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        let local = ok == 0 && error != ERROR_IO_PENDING;
        if local {
            let error = if self.kind == Kind::Connect && error == ERROR_PIPE_CONNECTED {
                None
            } else {
                Some(error)
            };
            self.publish(Completion { bytes: 0, error })?;
            // SAFETY: documented immediate no-packet path, including preconnected
            // ConnectNamedPipe. publish consumed this reservation exactly once.
            unsafe { drop(Arc::from_raw(packet)) };
        }
        self.returned();
        if self.token.aborting.load(Ordering::Acquire) {
            self.cancel_native();
        }
        Ok(())
    }

    fn finalize(&self, state: &mut State) {
        if state.returned && state.terminal.is_some() && state.accounted {
            state.accounted = false;
            self.token.users.fetch_sub(1, Ordering::AcqRel);
        }
    }
    fn returned(&self) {
        let mut state = locked(&self.state);
        state.returned = true;
        self.finalize(&mut state);
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn publish(&self, completion: Completion) -> io::Result<()> {
        let mut state = locked(&self.state);
        if !state.active || !state.packet || state.terminal.is_some() {
            return Err(failure("duplicate or unreserved native completion"));
        }
        state.packet = false;
        state.terminal = Some(completion);
        self.finalize(&mut state);
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }
    pub(super) fn poll(&self, cx: &Context<'_>) -> Poll<io::Result<Completion>> {
        let mut state = locked(&self.state);
        if state.returned
            && let Some(terminal) = state.terminal
        {
            return Poll::Ready(Ok(terminal));
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
    /// Consuming output is separate from native completion; no subsequent native
    /// read can overwrite the bytes before the logical owner copies/discards them.
    pub(super) fn consume(&self, copy: impl FnOnce(&[u8])) -> io::Result<Completion> {
        let mut state = locked(&self.state);
        if state.cancel_callers != 0 {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let terminal = state
            .terminal
            .filter(|_| state.returned)
            .ok_or_else(|| failure("native operation is not terminal"))?;
        if terminal.error.is_none()
            && terminal.bytes as usize > self.capacity
            && self.kind != Kind::Connect
        {
            return Err(failure("native result exceeds owned buffer"));
        }
        if self.kind == Kind::Read && terminal.error.is_none() {
            // SAFETY: actual native terminal and submission return observed;
            // bounds checked, state prevents slot reuse while the view exists.
            let bytes =
                unsafe { std::slice::from_raw_parts(self.pointer, terminal.bytes as usize) };
            copy(bytes);
        }
        state.active = false;
        state.terminal = None;
        Ok(terminal)
    }
    fn cancel_native(&self) {
        self.cancel_using(|handle, overlapped| {
            // SAFETY: explicit cancel-caller charge retains handle/slot through
            // this call's RETURN, independently of native operation completion.
            unsafe {
                CancelIoEx(handle, overlapped);
            }
        });
    }
    fn cancel_using(&self, cancel: impl FnOnce(std::os::windows::io::RawHandle, *mut OVERLAPPED)) {
        // Same lock order as submit: handle -> operation. The extra user charge
        // prevents close even if actual IO completes while CancelIoEx is delayed.
        let gate = locked(&self.token.handle);
        let mut state = locked(&self.state);
        if !state.active || (state.returned && state.terminal.is_some()) {
            return;
        }
        let Some(handle) = gate.as_ref().map(AsRawHandle::as_raw_handle) else {
            return;
        };
        state.cancel_callers += 1;
        self.token.users.fetch_add(1, Ordering::AcqRel);
        drop(state);
        drop(gate);
        cancel(handle, self.overlapped.get());
        let mut state = locked(&self.state);
        state.cancel_callers -= 1;
        self.token.users.fetch_sub(1, Ordering::AcqRel);
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// This Arc address is the immutable IOCP key for this physical HANDLE. It is
/// never re-keyed when a listening instance becomes an admitted connection.
pub(super) struct HandleToken {
    handle: Mutex<Option<OwnedHandle>>,
    operations: Mutex<Vec<Weak<Operation>>>,
    users: AtomicUsize,
    aborting: AtomicBool,
}
impl HandleToken {
    pub(super) fn abort(&self) {
        self.aborting.store(true, Ordering::Release);
        let slots: [Option<Arc<Operation>>; 3] = {
            let slots = locked(&self.operations);
            std::array::from_fn(|index| slots.get(index).and_then(Weak::upgrade))
        };
        for operation in slots.into_iter().flatten() {
            operation.cancel_native();
        }
    }
    pub(super) fn close_if_idle(&self) -> bool {
        if !self.aborting.load(Ordering::Acquire) {
            return false;
        }
        let mut handle = locked(&self.handle);
        if self.users.load(Ordering::Acquire) != 0 {
            return false;
        }
        let owned = handle.take();
        drop(handle);
        drop(owned);
        true
    }
}

enum Entry {
    Empty,
    Reserved,
    Live(Arc<HandleToken>),
}
struct PortState {
    entries: Vec<Entry>,
    fault: Option<&'static str>,
}
struct PortInner {
    handle: OwnedHandle,
    state: Mutex<PortState>,
    stopping: AtomicBool,
}

pub(super) struct Port {
    inner: Arc<PortInner>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
pub(super) struct Reservation {
    inner: Arc<PortInner>,
    index: usize,
    consumed: bool,
}
impl Port {
    pub(super) fn new(capacity: usize, stack_bytes: usize) -> io::Result<Self> {
        if capacity == 0 || stack_bytes == 0 {
            return Err(failure("invalid native root capacity"));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| failure("reserve native handle arena"))?;
        entries.resize_with(capacity, || Entry::Empty);
        // SAFETY: INVALID_HANDLE_VALUE creates an independent completion port.
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful creation transfers this sole owned port HANDLE.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let inner = Arc::new(PortInner {
            handle,
            state: Mutex::new(PortState {
                entries,
                fault: None,
            }),
            stopping: AtomicBool::new(false),
        });
        let worker = inner.clone();
        let thread = std::thread::Builder::new()
            .name("docsbase-pipe-completion".into())
            .stack_size(stack_bytes)
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run()));
                match result {
                    Ok(result) => {
                        if result.is_err() {
                            locked(&worker.state).fault = Some("native completion driver failed");
                        }
                        result
                    }
                    Err(_) => {
                        locked(&worker.state).fault = Some("native completion driver unwound");
                        Err(failure("native completion driver unwound"))
                    }
                }
            })?;
        Ok(Self {
            inner,
            thread: Some(thread),
        })
    }
    pub(super) fn reserve(&self) -> io::Result<Reservation> {
        let mut state = locked(&self.inner.state);
        if state.fault.is_some() || self.inner.stopping.load(Ordering::Acquire) {
            return Err(failure("native transport is closing"));
        }
        let index = state
            .entries
            .iter()
            .position(|entry| matches!(entry, Entry::Empty))
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        state.entries[index] = Entry::Reserved;
        Ok(Reservation {
            inner: self.inner.clone(),
            index,
            consumed: false,
        })
    }
    pub(super) fn retire(&self, token: &Arc<HandleToken>) -> bool {
        if !token.close_if_idle() {
            return false;
        }
        let mut state = locked(&self.inner.state);
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| matches!(entry, Entry::Live(found) if Arc::ptr_eq(found,token)))
        {
            *entry = Entry::Empty;
        }
        true
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        {
            let state = locked(&self.inner.state);
            if state.fault.is_some()
                || state
                    .entries
                    .iter()
                    .any(|entry| !matches!(entry, Entry::Empty))
            {
                return Err(failure("native transport retirement incomplete"));
            }
        }
        if !self.inner.stopping.swap(true, Ordering::AcqRel) {
            // SAFETY: reserved null operation/key0 is a root wake, never an IO terminal.
            if unsafe {
                PostQueuedCompletionStatus(self.inner.handle.as_raw_handle(), 0, 0, ptr::null())
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        if let Some(thread) = self.thread.as_ref() {
            // SAFETY: JoinHandle retains this dedicated native thread HANDLE.
            // A zero wait observes actual thread signaling; it never blocks Tokio.
            if unsafe { WaitForSingleObject(thread.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| failure("native progress join unwound"))??;
        }
        Ok(())
    }
}
impl Reservation {
    pub(super) fn associate(mut self, handle: OwnedHandle) -> io::Result<Arc<HandleToken>> {
        let token = Arc::new(HandleToken {
            handle: Mutex::new(Some(handle)),
            operations: Mutex::new(Vec::with_capacity(3)),
            users: AtomicUsize::new(0),
            aborting: AtomicBool::new(false),
        });
        let key = Arc::as_ptr(&token) as usize;
        locked(&self.inner.state).entries[self.index] = Entry::Live(token.clone());
        self.consumed = true;
        let handle = locked(&token.handle)
            .as_ref()
            .map(AsRawHandle::as_raw_handle)
            .ok_or_else(|| failure("missing native handle"))?;
        // SAFETY: token is registered before association and remains pinned for
        // this HANDLE through every connect/read/write packet and final close.
        if unsafe { CreateIoCompletionPort(handle, self.inner.handle.as_raw_handle(), key, 0) }
            .is_null()
        {
            let error = io::Error::last_os_error();
            token.abort();
            token.close_if_idle();
            locked(&self.inner.state).entries[self.index] = Entry::Empty;
            return Err(error);
        }
        Ok(token)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.consumed {
            locked(&self.inner.state).entries[self.index] = Entry::Empty;
        }
    }
}
impl PortInner {
    fn run(&self) -> io::Result<()> {
        loop {
            let mut bytes = 0;
            let mut key = 0;
            let mut overlapped = ptr::null_mut();
            // SAFETY: owned port stays live for this dedicated driver; outputs
            // are initialized and no application lock is held while waiting.
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    self.handle.as_raw_handle(),
                    &raw mut bytes,
                    &raw mut key,
                    &raw mut overlapped,
                    INFINITE,
                )
            };
            // SAFETY: immediate thread-local failure status capture.
            let error = if ok == 0 {
                Some(unsafe { GetLastError() })
            } else {
                None
            };
            if overlapped.is_null() {
                if ok != 0 && key == 0 && self.stopping.load(Ordering::Acquire) {
                    return Ok(());
                }
                locked(&self.state).fault = Some("native completion port failed without operation");
                return Err(failure("native completion port failed without operation"));
            }
            let token = {
                let state = locked(&self.state);
                state.entries.iter().find_map(|entry| match entry {
                    Entry::Live(token) if Arc::as_ptr(token) as usize == key => Some(token.clone()),
                    _ => None,
                })
            }
            .ok_or_else(|| failure("unknown native completion handle key"))?;
            let operation = locked(&token.operations)
                .iter()
                .filter_map(Weak::upgrade)
                .find(|slot| slot.overlapped.get() == overlapped)
                .ok_or_else(|| failure("unknown native completion slot"))?;
            operation.publish(Completion { bytes, error })?;
            // SAFETY: validated known token/slot and publish consumed exactly
            // its one installed packet reference, never a duplicate/stale Arc.
            unsafe { drop(Arc::from_raw(Arc::as_ptr(&operation))) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn pending(handle: Option<OwnedHandle>) -> Arc<Operation> {
        let token = Arc::new(HandleToken {
            handle: Mutex::new(handle),
            operations: Mutex::new(Vec::with_capacity(3)),
            users: AtomicUsize::new(1),
            aborting: AtomicBool::new(false),
        });
        let operation = Operation::new(token, Kind::Read, 8).unwrap();
        *locked(&operation.state) = State {
            active: true,
            packet: true,
            accounted: true,
            ..State::default()
        };
        operation
    }

    #[test]
    fn completion_before_submission_return_keeps_data_and_native_charge_owned() {
        // Controlled production state transition, not native pending-IO proof.
        let operation = pending(None);
        operation
            .publish(Completion {
                bytes: 4,
                error: None,
            })
            .unwrap();
        assert_eq!(operation.token.users.load(Ordering::Acquire), 1);
        assert!(
            operation
                .consume(|_| panic!("submission still owns storage"))
                .is_err()
        );
        let context = Context::from_waker(Waker::noop());
        assert!(operation.poll(&context).is_pending());
        operation.returned();
        assert_eq!(operation.token.users.load(Ordering::Acquire), 0);
        operation
            .consume(|bytes| assert_eq!(bytes, [0; 4]))
            .unwrap();
        assert!(
            operation
                .consume(|_| panic!("terminal consumed twice"))
                .is_err()
        );
    }

    #[test]
    fn duplicate_terminal_cannot_release_a_second_native_reference() {
        let operation = pending(None);
        operation.returned();
        operation
            .publish(Completion {
                bytes: 0,
                error: Some(995),
            })
            .unwrap();
        assert!(
            operation
                .publish(Completion {
                    bytes: 0,
                    error: None
                })
                .is_err()
        );
        assert_eq!(operation.token.users.load(Ordering::Acquire), 0);
    }

    #[test]
    fn delayed_cancel_return_fences_buffer_reuse_and_physical_handle_close() {
        let file = tempfile::tempfile().unwrap();
        let operation = pending(Some(file.into()));
        operation.token.aborting.store(true, Ordering::Release);
        operation.returned();
        let (entered, observed) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let cancelling = operation.clone();
        let thread = std::thread::spawn(move || {
            cancelling.cancel_using(|_, _| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
            })
        });
        observed.recv_timeout(Duration::from_secs(30)).unwrap();
        operation
            .publish(Completion {
                bytes: 1,
                error: None,
            })
            .unwrap();
        let native_users = operation.token.users.load(Ordering::Acquire);
        let buffer_fenced = operation.consume(|_| {}).is_err();
        let handle_fenced = !operation.token.close_if_idle();
        release.send(()).unwrap();
        thread.join().unwrap();
        assert_eq!(
            native_users, 1,
            "cancel caller is still inside native boundary"
        );
        assert!(buffer_fenced);
        assert!(handle_fenced);
        operation.consume(|bytes| assert_eq!(bytes, [0])).unwrap();
        assert!(operation.token.close_if_idle());
    }
}
