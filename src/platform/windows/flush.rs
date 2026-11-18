//! One dedicated, connection-owned drain worker. Cancellation never means return.
#![allow(unsafe_code)] // Owned event/thread/pipe handles; no pool-thread cancellation.
use super::connection::NativePipe;
use super::native_io::HandleToken;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForSingleObject,
};

fn locked<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
fn failure(message: &'static str) -> io::Error {
    io::Error::other(message)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Starting,
    Ready,
    Queued,
    Flushing,
    Returned,
    Exited,
    Faulted,
}
struct State {
    phase: Phase,
    abort: bool,
    result: Option<Result<(), u32>>,
    cancel_callers: usize,
    joining: bool,
    joined: Option<Result<(), &'static str>>,
    cancel_deferred: bool,
    waker: Option<Waker>,
}
struct Shared {
    token: Arc<HandleToken>,
    pipe: usize,
    pipe_owner: Mutex<Option<Arc<NativePipe>>>,
    request: OwnedHandle,
    returned: OwnedHandle,
    thread: AtomicUsize,
    state: Mutex<State>,
    #[cfg(test)]
    before_native: Mutex<Option<BeforeNative>>,
    #[cfg(test)]
    cancel_status: std::sync::atomic::AtomicU32,
}
#[cfg(test)]
struct BeforeNative {
    entered: tokio::sync::oneshot::Sender<()>,
    proceed: std::sync::mpsc::Receiver<()>,
}
pub(super) struct Flush {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: unnamed manual-reset event, null security/name, sole ownership.
    let raw = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful creation transfers ownership exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}
impl Flush {
    pub(super) fn new(token: Arc<HandleToken>, stack_bytes: usize) -> io::Result<Self> {
        let request = event()?;
        let returned = event()?;
        let pipe_owner = token.retain_helper()?;
        let pipe = pipe_owner.raw() as usize;
        let shared = Arc::new(Shared {
            token,
            pipe,
            pipe_owner: Mutex::new(Some(pipe_owner)),
            request,
            returned,
            thread: AtomicUsize::new(0),
            state: Mutex::new(State {
                phase: Phase::Starting,
                abort: false,
                result: None,
                cancel_callers: 0,
                joining: false,
                joined: None,
                cancel_deferred: false,
                waker: None,
            }),
            #[cfg(test)]
            before_native: Mutex::new(None),
            #[cfg(test)]
            cancel_status: std::sync::atomic::AtomicU32::new(u32::MAX),
        });
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("docsbase-pipe-drain".into())
            .stack_size(stack_bytes)
            .spawn(move || {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run()));
                if !matches!(outcome, Ok(Ok(()))) {
                    let mut state = locked(&worker.state);
                    state.phase = Phase::Faulted;
                    state.abort = true;
                    drop(state);
                    worker.token.report_fault("dedicated drain worker failed");
                    worker.signal();
                }
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(error) => {
                shared.token.release_helper();
                return Err(error);
            }
        };
        shared
            .thread
            .store(thread.as_raw_handle() as usize, Ordering::Release);
        Ok(Self {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }
    pub(super) fn poll_flush(&self, cx: &Context<'_>) -> Poll<io::Result<()>> {
        let waker = cx.waker().clone();
        let mut state = locked(&self.shared.state);
        if state.abort || self.shared.token.is_aborting() {
            return Poll::Ready(Err(failure("pipe drain is aborting")));
        }
        if let Some(result) = state.result.take() {
            state.phase = Phase::Ready;
            return Poll::Ready(
                result.map_err(|code| io::Error::from_raw_os_error(code.cast_signed())),
            );
        }
        if state.phase == Phase::Ready {
            if !self.shared.token.writes_idle() {
                return Poll::Ready(Err(failure("flush started before native write terminal")));
            }
            state.phase = Phase::Queued;
            let previous = state.waker.replace(waker);
            drop(state);
            drop(previous);
            // SAFETY: ticket state published before waking this owned worker.
            if unsafe { SetEvent(self.shared.request.as_raw_handle()) } == 0 {
                return Poll::Ready(Err(io::Error::last_os_error()));
            }
            return Poll::Pending;
        }
        let previous = state.waker.replace(waker);
        drop(state);
        drop(previous);
        Poll::Pending
    }
    pub(super) fn abort(&self) {
        self.shared.token.abort();
        let mut state = locked(&self.shared.state);
        state.abort = true;
        let cancel = !state.joining;
        if !cancel {
            state.cancel_deferred = true;
        }
        if cancel {
            state.cancel_callers += 1;
        }
        let thread = if cancel {
            self.shared.thread.load(Ordering::Acquire)
        } else {
            0
        };
        drop(state);
        if thread != 0 {
            // SAFETY: dedicated helper HANDLE is retained by JoinHandle until
            // ALL cancel callers return; never a reused/pool thread. Result is
            // only an abort request, including NOT_FOUND, never completion.
            let cancelled = unsafe { CancelSynchronousIo(thread as _) };
            #[cfg(test)]
            self.shared.cancel_status.store(
                if cancelled != 0 {
                    0
                } else {
                    // SAFETY: capture this caller's native cancellation status.
                    unsafe { GetLastError() }
                },
                Ordering::Release,
            );
            #[cfg(not(test))]
            let _ = cancelled;
        }
        let mut state = locked(&self.shared.state);
        if cancel {
            state.cancel_callers -= 1;
        }
        drop(state);
        // SAFETY: valid owned event; also wakes a worker not yet in flush.
        unsafe {
            SetEvent(self.shared.request.as_raw_handle());
        }
        self.shared.signal();
    }
    #[cfg(test)]
    pub(super) fn pause_before_native(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        proceed: std::sync::mpsc::Receiver<()>,
    ) {
        *locked(&self.shared.before_native) = Some(BeforeNative { entered, proceed });
    }
    #[cfg(test)]
    pub(super) fn cancel_status(&self) -> u32 {
        self.shared.cancel_status.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn wait_signal(&self) -> io::Result<()> {
        let thread = locked(&self.thread)
            .as_ref()
            .map(AsRawHandle::as_raw_handle);
        let Some(thread) = thread else {
            return Ok(());
        };
        // SAFETY: the single test owner retains JoinHandle, with no concurrent
        // joining/cancellation. This observes the actual native thread signal.
        if unsafe { WaitForSingleObject(thread, 30_000) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        }
    }
    /// Called by the outer owner only; no wait/join in stream/future Drop.
    pub(super) fn join_ready(&self) -> io::Result<bool> {
        let mut state = locked(&self.shared.state);
        if let Some(result) = state.joined {
            return result.map(|()| true).map_err(failure);
        }
        if state.cancel_callers != 0 || state.joining {
            return Ok(false);
        }
        let thread = locked(&self.thread)
            .as_ref()
            .map(AsRawHandle::as_raw_handle);
        let Some(thread) = thread else {
            return Ok(true);
        };
        state.joining = true;
        drop(state);
        // SAFETY: joining fence retains the dedicated HANDLE until the zero
        // wait completes; no Rust lock is held across this native observation.
        let signaled = unsafe { WaitForSingleObject(thread, 0) } == WAIT_OBJECT_0;
        if !signaled {
            let mut state = locked(&self.shared.state);
            state.joining = false;
            let deferred = std::mem::take(&mut state.cancel_deferred);
            drop(state);
            if deferred {
                self.abort();
            }
            return Ok(false);
        }
        let thread = locked(&self.thread).take();
        let outcome = thread.map_or(Ok(()), |thread| {
            thread.join().map_err(|_| "drain worker join unwound")
        });
        self.shared.thread.store(0, Ordering::Release);
        if outcome.is_err() {
            self.shared.token.report_fault("drain worker join unwound");
        }
        drop(locked(&self.shared.pipe_owner).take());
        self.shared.token.release_helper();
        let mut state = locked(&self.shared.state);
        state.joined = Some(outcome);
        state.joining = false;
        state.cancel_deferred = false;
        drop(state);
        outcome.map(|()| true).map_err(failure)
    }
}
impl Shared {
    fn signal(&self) {
        // SAFETY: returned event is owned until worker AND cancel callers end.
        unsafe {
            SetEvent(self.returned.as_raw_handle());
        }
        let waker = locked(&self.state).waker.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn run(&self) -> io::Result<()> {
        locked(&self.state).phase = Phase::Ready;
        self.signal();
        loop {
            // SAFETY: dedicated worker waits only its own retained event.
            if unsafe { WaitForSingleObject(self.request.as_raw_handle(), INFINITE) }
                != WAIT_OBJECT_0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: same owned manual-reset event; no new ticket while busy.
            unsafe {
                ResetEvent(self.request.as_raw_handle());
            }
            let mut state = locked(&self.state);
            if state.abort || self.token.is_aborting() {
                state.phase = Phase::Exited;
                drop(state);
                self.signal();
                return Ok(());
            }
            if state.phase != Phase::Queued {
                continue;
            }
            state.phase = Phase::Flushing;
            drop(state);
            #[cfg(test)]
            {
                let gate = locked(&self.before_native).take();
                if let Some(gate) = gate {
                    gate.entered
                        .send(())
                        .map_err(|()| failure("test observer lost"))?;
                    gate.proceed
                        .recv_timeout(std::time::Duration::from_secs(30))
                        .map_err(io::Error::other)?;
                }
            }
            // SAFETY: retain_helper keeps the pipe open until actual thread
            // signal+join. This is the sole helper and all writes are terminal.
            let ok = unsafe { FlushFileBuffers(self.pipe as _) };
            // SAFETY: capture raw thread-local native status before publication.
            let result = if ok != 0 {
                Ok(())
            } else {
                Err(unsafe { GetLastError() })
            };
            let mut state = locked(&self.state);
            state.result = Some(result);
            state.phase = Phase::Returned;
            drop(state);
            self.signal();
        }
    }
}
