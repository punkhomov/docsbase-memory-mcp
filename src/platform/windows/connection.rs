//! Raw owned pipe endpoints; IOCP keeps write buffers alive after logical acceptance.
#![allow(unsafe_code)] // Only constructors transfer fresh Win32 pipe HANDLE ownership.
use super::flush::Flush;
#[cfg(test)]
use super::native_io::Port;
use super::native_io::{HandleToken, Kind, Operation, PhysicalOwner, Service};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES,
};

fn failure(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn name(path: &str) -> io::Result<Vec<u16>> {
    if path.contains('\0') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut wide: Vec<u16> = path.encode_utf16().collect();
    if wide.len() > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native pipe name exceeds Windows limit",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// The listening physical HANDLE/token is transferred intact, never re-keyed.
pub(super) struct Listening {
    pub token: Arc<HandleToken>,
    connect: Arc<Operation>,
    transferred: AtomicBool,
}
impl Listening {
    #[cfg(test)]
    pub(super) fn new(port: &Port, path: &str, chunk: usize, first: bool) -> io::Result<Arc<Self>> {
        Self::new_in(&port.service(), path, chunk, first)
    }
    pub(super) fn new_in(
        service: &Service,
        path: &str,
        chunk: usize,
        first: bool,
    ) -> io::Result<Arc<Self>> {
        let reservation = service.reserve()?;
        let name = name(path)?;
        let quota = u32::try_from(chunk).map_err(|_| failure("pipe quota exceeds DWORD"))?;
        let flags = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: fresh byte-mode local pipe, checked owned name/quotas. The
        // application reservation, not the sentinel, bounds actual instances.
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                flags,
                PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                quota,
                quota,
                0,
                ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: one successful native creation, transferred exactly once.
        let token = reservation.associate(unsafe { OwnedHandle::from_raw_handle(raw) })?;
        let connect = Operation::new(token.clone(), Kind::Connect, 0)?;
        let owner = Arc::new(Self {
            token,
            connect,
            transferred: AtomicBool::new(false),
        });
        service.attach(&owner.token, owner.clone())?;
        Ok(owner)
    }
    pub(super) fn poll_accept(&self, cx: &Context<'_>) -> Poll<io::Result<Arc<HandleToken>>> {
        if self.transferred.load(Ordering::Acquire) {
            return Poll::Ready(Err(failure("listening handle was already transferred")));
        }
        if !self.connect.active()
            && let Err(error) = self.connect.submit(&[])
        {
            return Poll::Ready(Err(error));
        }
        match self.connect.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => Poll::Ready(
                self.connect
                    .consume()
                    .and_then(super::native_io::Completion::result)
                    .and_then(|_| {
                        self.transferred
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .map(|_| self.token.clone())
                            .map_err(|_| failure("listening handle was already transferred"))
                    }),
            ),
        }
    }
    pub(super) fn abort(&self) {
        self.token.abort();
    }
}

pub(super) struct Connection {
    pub token: Arc<HandleToken>,
    read: Arc<Operation>,
    write: Arc<Operation>,
    drain: OnceLock<io::Result<Flush>>,
    initializing: AtomicBool,
    logical_claimed: AtomicBool,
    charges: Mutex<[Option<Arc<dyn Send + Sync>>; 2]>,
    helper_stack_bytes: usize,
    closing: AtomicBool,
    closed: AtomicBool,
    chunk: usize,
    accepted: AtomicUsize,
    dirty: AtomicBool,
    server: bool,
    disconnected: AtomicBool,
}
impl Connection {
    pub(super) fn accepted(
        token: Arc<HandleToken>,
        chunk: usize,
        stack: usize,
    ) -> io::Result<Arc<Self>> {
        let service = token.service()?;
        Self::new(&service, token, chunk, stack, true)
    }
    #[cfg(test)]
    pub(super) fn client(
        port: &Port,
        path: &str,
        chunk: usize,
        stack: usize,
    ) -> io::Result<Arc<Self>> {
        Self::client_in(&port.service(), path, chunk, stack)
    }
    pub(super) fn client_in(
        service: &Service,
        path: &str,
        chunk: usize,
        stack: usize,
    ) -> io::Result<Arc<Self>> {
        let reservation = service.reserve()?;
        let name = name(path)?;
        // SAFETY: single nonblocking native open; busy/absence is returned to
        // caller-owned connect admission, never WaitNamedPipe/helper fallback.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sole fresh client HANDLE, no interprocess/Mio dirty owner.
        let token = reservation.associate(unsafe { OwnedHandle::from_raw_handle(raw) })?;
        Self::new(service, token, chunk, stack, false)
    }
    fn new(
        service: &Service,
        token: Arc<HandleToken>,
        chunk: usize,
        stack: usize,
        server: bool,
    ) -> io::Result<Arc<Self>> {
        let read =
            Operation::new(token.clone(), Kind::Read, chunk).inspect_err(|_| token.abort())?;
        let write =
            Operation::new(token.clone(), Kind::Write, chunk).inspect_err(|_| token.abort())?;
        // Every connection reserves one possible helper in its root policy,
        // but ordinary exchanges allocate only the owned native I/O regions.
        // Register the envelope before exposing any cancellable submission.
        let owner = Arc::new(Self {
            token,
            read,
            write,
            drain: OnceLock::new(),
            initializing: AtomicBool::new(false),
            logical_claimed: AtomicBool::new(false),
            charges: Mutex::new([None, None]),
            helper_stack_bytes: stack,
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            chunk,
            accepted: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            server,
            disconnected: AtomicBool::new(false),
        });
        service.attach(&owner.token, owner.clone())?;
        Ok(owner)
    }
    fn drain(&self) -> io::Result<&Flush> {
        // Synchronous construction is fenced from outer reaping. retain_helper
        // independently fences the HANDLE before the worker can enter Win32.
        // On unwind this flag stays set and the root preserves quarantine.
        self.initializing.store(true, Ordering::Release);
        let result = self
            .drain
            .get_or_init(|| Flush::new(self.token.clone(), self.helper_stack_bytes));
        self.initializing.store(false, Ordering::Release);
        match result {
            Ok(drain) => Ok(drain),
            Err(error) => {
                self.abort();
                Err(error.raw_os_error().map_or_else(
                    || io::Error::new(error.kind(), error.to_string()),
                    io::Error::from_raw_os_error,
                ))
            }
        }
    }
    #[cfg(test)]
    pub(super) fn test_read_active(&self) -> bool {
        self.read.active()
    }
    #[cfg(test)]
    pub(super) fn test_hold_write_cancellation(
        &self,
        entered: &std::sync::mpsc::Sender<()>,
        release: &std::sync::mpsc::Receiver<()>,
    ) {
        self.write.controlled_cancel(|handle, overlapped| {
            // SAFETY: controlled_cancel retains the original native handle and
            // operation storage through this complete cancellation boundary.
            unsafe {
                windows_sys::Win32::System::IO::CancelIoEx(handle, overlapped);
            }
            let _ = entered.send(());
            let _ = release.recv();
        });
    }
    /// Install ownership before submission; old owners drop outside the lock.
    pub(super) fn retain(&self, request: bool, charge: Arc<dyn Send + Sync>) -> io::Result<()> {
        let old = {
            let mut charges = self.charges.lock().unwrap_or_else(PoisonError::into_inner);
            if self.token.is_aborting() {
                return Err(failure("native connection is aborting"));
            }
            charges[usize::from(request)].replace(charge)
        };
        drop(old);
        Ok(())
    }
    pub(super) fn request_completed(&self) {
        // A final-drain worker keeps the request through its actual thread
        // exit/join. Persistent replies end at the native-write barrier.
        if self.closing.load(Ordering::Acquire) || self.write.active() {
            return;
        }
        let old = self.charges.lock().unwrap_or_else(PoisonError::into_inner)[1].take();
        drop(old);
    }
    pub(super) fn abort(&self) {
        self.token.abort();
        if let Some(Ok(drain)) = self.drain.get() {
            drain.abort();
        }
        if self.server
            && self.dirty.load(Ordering::Acquire)
            && !self.disconnected.swap(true, Ordering::AcqRel)
        {
            self.token.disconnect_failed();
        }
    }
    /// Outer cleanup calls this while progress is alive, never from async Drop.
    #[cfg(test)]
    pub(super) fn retire(&self, port: &Port) -> io::Result<bool> {
        self.abort();
        if !self.retire_ready()? {
            return Ok(false);
        }
        Ok(port.retire(&self.token))
    }
    fn write_terminal(&self, cx: &Context<'_>) -> Poll<io::Result<()>> {
        if !self.write.active() {
            return Poll::Ready(Ok(()));
        }
        match self.write.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => {
                let result = self
                    .write
                    .consume()
                    .and_then(super::native_io::Completion::result);
                match result {
                    Ok(count) if count == self.accepted.load(Ordering::Acquire) => {
                        Poll::Ready(Ok(()))
                    }
                    Ok(_) => {
                        self.abort();
                        Poll::Ready(Err(failure("short native pipe write")))
                    }
                    Err(error) => {
                        self.abort();
                        Poll::Ready(Err(error))
                    }
                }
            }
        }
    }
}

impl PhysicalOwner for Listening {
    fn abort(&self) {
        self.abort();
    }
    fn retire_ready(&self) -> io::Result<bool> {
        Ok(true)
    }
}
impl PhysicalOwner for Connection {
    fn abort(&self) {
        self.abort();
    }
    fn retire_ready(&self) -> io::Result<bool> {
        if self.initializing.load(Ordering::Acquire) {
            return Ok(false);
        }
        match self.drain.get() {
            Some(Ok(drain)) => drain.join_ready(),
            None | Some(Err(_)) => Ok(true),
        }
    }
    fn retired(&self) {
        let old = {
            let mut charges = self.charges.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *charges)
        };
        drop(old);
    }
}

struct Logical {
    connection: Arc<Connection>,
}
impl Drop for Logical {
    fn drop(&mut self) {
        // Cancellation/wake only. The arena, not this logical Arc, owns native
        // buffers, helper JoinHandle, and eventual physical retirement.
        self.connection.token.abort();
        if let Some(Ok(drain)) = self.connection.drain.get() {
            drain.abort();
        }
    }
}
pub(super) struct Pipe {
    logical: Arc<Logical>,
}
impl Pipe {
    pub(super) fn new(connection: Arc<Connection>) -> io::Result<Self> {
        // One writable stream per physical connection. Tokio split serializes
        // access to that stream; cloning a writer would let it race shutdown
        // between the closing check and native submission.
        connection
            .logical_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| failure("native connection already has a logical stream"))?;
        Ok(Self {
            logical: Arc::new(Logical { connection }),
        })
    }
}
impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let connection = &self.get_mut().logical.connection;
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if !connection.read.active()
            && let Err(error) = connection.read.submit(&[])
        {
            return Poll::Ready(Err(error));
        }
        match connection.read.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => {
                let count = buffer.remaining().min(connection.chunk);
                let output = buffer.initialize_unfilled_to(count);
                match connection.read.copy_read(output) {
                    Ok(count) => {
                        buffer.advance(count);
                        Poll::Ready(Ok(()))
                    }
                    Err(error) if matches!(error.raw_os_error(), Some(109 | 233 | 232)) => {
                        Poll::Ready(Ok(()))
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                }
            }
        }
    }
}
impl AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let connection = &self.get_mut().logical.connection;
        if connection.closing.load(Ordering::Acquire) {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let count = bytes.len().min(connection.chunk);
        connection.accepted.store(count, Ordering::Release);
        connection.dirty.store(true, Ordering::Release);
        if let Err(error) = connection.write.submit(&bytes[..count]) {
            connection.abort();
            return Poll::Ready(Err(error));
        }
        // Logical acceptance is explicit; fixed owned storage survives pending
        // native write and future Drop. flush proves native completion;
        // shutdown separately proves peer drain for a successful server close.
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let connection = &self.get_mut().logical.connection;
        connection.write_terminal(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let connection = &self.get_mut().logical.connection;
        connection.closing.store(true, Ordering::Release);
        if connection.closed.load(Ordering::Acquire) {
            return Poll::Ready(Ok(()));
        }
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        // Named pipes have no socket-style half-close. Clients need native
        // write completion only; a final server reply must additionally be read
        // by its peer before this physical instance can be safely closed.
        if !connection.server || !connection.dirty.load(Ordering::Acquire) {
            connection.closed.store(true, Ordering::Release);
            return Poll::Ready(Ok(()));
        }
        let drain = match connection.drain() {
            Ok(drain) => drain,
            Err(error) => return Poll::Ready(Err(error)),
        };
        match drain.poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                connection.dirty.store(false, Ordering::Release);
                connection.closed.store(true, Ordering::Release);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                connection.abort();
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn final_server_drain_is_lazy_delivers_complete_frame_and_closes_writes() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        super::super::root::run_on(&runtime, &policy, |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 1, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 4096, 1024 * 1024)?;
            let mut writer = Pipe::new(server.clone())?;
            assert!(
                Pipe::new(server.clone()).is_err(),
                "one logical writer per connection"
            );
            let mut reader = Pipe::new(client.clone())?;
            let mut frame = vec![b'x'; 4096];
            frame.push(b'\n');
            let (prefix_sent, prefix_read) = tokio::sync::oneshot::channel();
            let (release_tail, read_tail) = tokio::sync::oneshot::channel();
            let sending = async {
                writer.write_all(&frame).await?;
                writer.flush().await?;
                assert!(server.drain.get().is_none(), "native write needs no helper");
                prefix_read.await.map_err(io::Error::other)?;
                let mut closing = Box::pin(writer.shutdown());
                assert!(
                    std::future::poll_fn(|cx| std::task::Poll::Ready(closing.as_mut().poll(cx)))
                        .await
                        .is_pending(),
                    "native write completion cannot stand in for peer drain"
                );
                release_tail
                    .send(())
                    .map_err(|()| failure("tail reader lost"))?;
                closing.await?;
                assert!(
                    server.drain.get().is_some(),
                    "final server close drains peer"
                );
                writer.shutdown().await?;
                assert_eq!(
                    writer.write(b"late").await.unwrap_err().kind(),
                    io::ErrorKind::BrokenPipe
                );
                Ok::<_, io::Error>(())
            };
            let reading = async {
                let mut received = vec![0; frame.len()];
                // One-byte native read regions make read-ahead impossible,
                // including short ReadFile completions. The final newline
                // stays in the kernel pipe until this explicit release.
                let tail = frame.len() - 1;
                reader.read_exact(&mut received[..tail]).await?;
                prefix_sent
                    .send(())
                    .map_err(|()| failure("close observer lost"))?;
                read_tail.await.map_err(io::Error::other)?;
                reader.read_exact(&mut received[tail..]).await?;
                assert_eq!(received, frame);
                reader.shutdown().await?;
                assert!(
                    client.drain.get().is_none(),
                    "client never waits for peer drain"
                );
                Ok::<_, io::Error>(())
            };
            tokio::try_join!(sending, reading)?;
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn pre_entry_drain_cancellation_not_found_keeps_native_owner_until_thread_exit() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let admitted = slots.clone().try_acquire_owned().unwrap();
        let request_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let request = request_slots.clone().try_acquire_owned().unwrap();
        let inspect_slots = slots.clone();
        let inspect_requests = request_slots.clone();
        let weak = super::super::root::run_on(&runtime, &policy, |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 4096, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 4096, 1024 * 1024)?;
            server.retain(false, Arc::new(admitted))?;
            server.retain(true, Arc::new(request))?;
            let mut writer = Pipe::new(server.clone())?;
            writer.write_all(b"tail\n").await?;
            writer.flush().await?;
            let drain = server.drain()?;
            let (entered, observing) = tokio::sync::oneshot::channel();
            let (release, proceed) = std::sync::mpsc::channel();
            drain.pause_before_native(entered, proceed);
            let mut closing = Box::pin(writer.shutdown());
            tokio::select! {
                result = observing => result.map_err(io::Error::other)?,
                result = &mut closing => panic!("drain returned before native entry: {result:?}"),
            }
            // The helper is deliberately BETWEEN its abort check and Win32
            // entry. NOT_FOUND is an actual CancelSynchronousIo observation,
            // never evidence of thread exit or permission to release capacity.
            server.abort();
            assert_eq!(
                drain.cancel_status(),
                windows_sys::Win32::Foundation::ERROR_NOT_FOUND
            );
            assert!(!drain.join_ready()?);
            assert_eq!(
                inspect_slots.available_permits(),
                0,
                "original connection charge is still live"
            );
            assert_eq!(
                inspect_requests.available_permits(),
                0,
                "original request charge is still live"
            );
            assert_eq!(service.reap()?, 2);
            assert!(
                matches!(service.reserve(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
            );
            release.send(()).map_err(io::Error::other)?;
            assert!(closing.await.is_err());
            drop(client);
            Ok((Arc::downgrade(&server), server))
        })
        .unwrap();
        let (weak, logical_owner) = weak;
        assert_eq!(
            slots.available_permits(),
            1,
            "physical retirement releases the original connection charge"
        );
        assert_eq!(
            request_slots.available_permits(),
            1,
            "physical retirement releases the original request charge"
        );
        assert!(
            weak.upgrade().is_some(),
            "logical references may outlive retirement"
        );
        drop(logical_owner);
        assert!(
            weak.upgrade().is_none(),
            "outer owner observed actual thread exit/join"
        );
    }
    #[test]
    fn owned_facade_clone_admission_and_timeout_updates_do_not_wait_for_a_read() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run(&policy, move |service| {
            let transport = super::super::transport::Transport::new(service, options);
            let listener = transport.bind_blocking(&endpoint)?;
            let _client = transport.connect_blocking(&endpoint)?;
            let mut server = listener.accept()?;
            server.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
            let mut first_reader = server.try_clone()?;
            std::thread::scope(|scope| -> io::Result<()> {
                let first = scope.spawn(move || std::io::Read::read(&mut first_reader, &mut [0]));
                let watchdog = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while !server.test_read_active() && std::time::Instant::now() < watchdog {
                    std::thread::yield_now();
                }
                assert!(
                    server.test_read_active(),
                    "first native read entered before cloned admission"
                );
                // This setter must not wait for the pending read. The second
                // clone's original deadline includes direction-lock admission.
                server.set_read_timeout(Some(std::time::Duration::from_millis(1)))?;
                let second = std::io::Read::read(&mut server, &mut [0]).unwrap_err();
                assert_eq!(second.kind(), io::ErrorKind::TimedOut);
                assert!(
                    first.join().unwrap().is_err(),
                    "timed-out exchange cancels the first native read too"
                );
                Ok(())
            })
        })
        .unwrap();
    }

    #[test]
    fn owned_facade_blocking_timeout_aborts_without_waiting_for_peer() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run(&policy, move |service| {
            let transport = super::super::transport::Transport::new(service, options);
            let listener = transport.bind_blocking(&endpoint)?;
            listener.set_nonblocking(true)?;
            assert_eq!(
                listener.accept().err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            let mut client = transport.connect_blocking(&endpoint)?;
            listener.set_nonblocking(false)?;
            let _server = listener.accept()?;
            client.set_read_timeout(Some(std::time::Duration::from_millis(1)))?;
            let error = std::io::Read::read(&mut client, &mut [0]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn owned_facade_blocking_split_preserves_final_frame() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run(&policy, move |service| {
            let transport = super::super::transport::Transport::new(service, options);
            let listener = transport.bind_blocking(&endpoint)?;
            let mut client = transport.connect_blocking(&endpoint)?;
            let server = listener.accept()?;
            let mut reader = server.try_clone()?;
            let mut writer = server;
            let expected = vec![b'x'; 16_384];
            std::thread::scope(|scope| -> io::Result<()> {
                let bytes = &expected;
                let serving = scope.spawn(move || -> io::Result<()> {
                    let mut request = [0; 1];
                    std::io::Read::read_exact(&mut reader, &mut request)?;
                    assert_eq!(request, [b'?']);
                    std::io::Write::write_all(&mut writer, bytes)?;
                    std::io::Write::flush(&mut writer)?;
                    writer.shutdown()
                });
                std::io::Write::write_all(&mut client, b"?")?;
                std::io::Write::flush(&mut client)?;
                let mut actual = vec![0; expected.len()];
                std::io::Read::read_exact(&mut client, &mut actual)?;
                assert_eq!(actual, expected);
                serving.join().unwrap()
            })
        })
        .unwrap();
    }

    #[test]
    fn owned_facade_waits_for_physical_capacity_before_next_accept() {
        use std::future::Future;
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run_on(&runtime, &policy, move |service| async move {
            let transport = super::super::transport::Transport::new(service.clone(), options);
            let listener = transport.bind(&endpoint)?;
            let client = transport.connect_async(&endpoint).await?;
            let server = listener.accept().await?;
            let mut next = std::pin::pin!(listener.accept());
            std::future::poll_fn(|cx| {
                assert!(
                    next.as_mut().poll(cx).is_pending(),
                    "full arena waits, never fatal accept"
                );
                std::task::Poll::Ready(())
            })
            .await;
            drop(server);
            drop(client);
            // A registered capacity waiter must wake when actual retirement
            // frees the old two physical records, without another application RPC.
            let connecting = async {
                loop {
                    match transport.connect_async(&endpoint).await {
                        Ok(client) => break Ok::<_, io::Error>(client),
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock
                                    | io::ErrorKind::NotFound
                                    | io::ErrorKind::TimedOut
                            ) =>
                        {
                            tokio::task::yield_now().await;
                        }
                        Err(error) => break Err(error),
                    }
                }
            };
            let (server, client) =
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::try_join!(&mut next, connecting)
                })
                .await
                .map_err(io::Error::other)??;
            drop(server);
            drop(client);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn repeated_cancelled_reads_keep_bounded_capacity_and_control_progress() {
        let directory = tempfile::tempdir().unwrap();
        let control = crate::platform::daemon_endpoint(directory.path());
        let churn = crate::platform::daemon_endpoint(&directory.path().join("churn"));
        let path = super::super::pipe_path(&control).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 4,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run_on(&runtime, &policy, move |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 4096, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 4096, 1024 * 1024)?;
            let mut writer = Pipe::new(server.clone())?;
            let mut reader = Pipe::new(client.clone())?;
            let transport = super::super::transport::Transport::new(service.clone(), options);
            let mut high_water = 0;
            for _ in 0..32 {
                let listener = transport.bind(&churn)?;
                let peer = transport.connect_async(&churn).await?;
                let mut dirty = listener.accept().await?;
                let mut byte = [0];
                {
                    let pending = dirty.read_exact(&mut byte);
                    tokio::pin!(pending);
                    std::future::poll_fn(|cx| {
                        use std::future::Future;
                        assert!(pending.as_mut().poll(cx).is_pending());
                        std::task::Poll::Ready(())
                    }).await;
                }
                high_water = high_water.max(service.reap()?);
                drop((dirty, peer, listener));
                // Control traffic progresses while the cancelled native read retires.
                let mut received = [0; 5];
                tokio::try_join!(async {
                    writer.write_all(b"tick\n").await?;
                    writer.flush().await
                }, reader.read_exact(&mut received))?;
                assert_eq!(&received, b"tick\n");
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    loop {
                        if service.reap()? == 2 {
                            break Ok::<_, io::Error>(());
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.map_err(io::Error::other)??;
            }
            assert_eq!(high_water, 4, "active plus retiring physical cap");
            assert!(server.drain.get().is_none() && client.drain.get().is_none());
            eprintln!("native cancellation churn: cycles=32 physical_high_water=4 remaining_control=2 control_replies=32 helpers=0");
            Ok(())
        }).unwrap();
    }

    #[test]
    fn persistent_exchange_completes_native_writes_without_a_drain_worker() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 2,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let (before, after) = super::super::root::run_on(&runtime, &policy, |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 4096, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 4096, 1024 * 1024)?;
            let before = (server.drain.get().is_some(), client.drain.get().is_some());
            let mut writer = Pipe::new(server.clone())?;
            let mut reader = Pipe::new(client.clone())?;
            let mut samples = Vec::with_capacity(128);
            for _ in 0..128 {
                let began = std::time::Instant::now();
                let mut received = [0; 5];
                let sending = async {
                    writer.write_all(b"pong\n").await?;
                    writer.flush().await
                };
                let reading = reader.read_exact(&mut received);
                tokio::try_join!(sending, reading)?;
                assert_eq!(&received, b"pong\n");
                assert!(!server.write.active(), "last native write was consumed");
                samples.push(began.elapsed().as_micros());
            }
            samples.sort_unstable();
            eprintln!(
                "native persistent 5-byte write/read: n={} p50_us={} p95_us={} max_us={} helper_creations=0",
                samples.len(), samples[64], samples[121], samples[127]
            );
            Ok((
                before,
                (server.drain.get().is_some(), client.drain.get().is_some()),
            ))
        })
        .unwrap();
        assert_eq!(before, (false, false), "no eager helper at connect/accept");
        assert_eq!(after, (false, false), "ordinary replies need no peer drain");
    }
    #[test]
    fn dropped_dirty_pipe_keeps_registered_owner_until_actual_native_retirement() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let mut port = Port::new(2, 1024 * 1024).unwrap();
        let service = port.service();
        let listening = Listening::new(&port, &path, 4096, true).unwrap();
        let client = Connection::client(&port, &path, 65_536, 1024 * 1024).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let token = runtime
            .block_on(std::future::poll_fn(|cx| listening.poll_accept(cx)))
            .unwrap();
        let server = Connection::accepted(token, 65_536, 1024 * 1024).unwrap();
        drop(listening);
        let weak = Arc::downgrade(&server);
        let mut pipe = Pipe::new(server.clone()).unwrap();
        let bytes = vec![b'x'; 65_536];
        assert_eq!(runtime.block_on(pipe.write(&bytes)).unwrap(), bytes.len());
        // Actual native return status, not a pre-entry marker or poll-count
        // guess: the unread large WriteFile is pending beyond logical write.
        assert!(server.write.submitted_pending());
        drop(server);
        drop(pipe);
        assert!(
            weak.upgrade().is_some(),
            "the registry owns the dirty envelope"
        );
        assert!(
            matches!(service.reserve(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let retained = weak.upgrade().unwrap();
        retained.abort();
        client.abort();
        assert!(retained.drain.get().is_none());
        assert!(client.drain.get().is_none());
        runtime
            .block_on(std::future::poll_fn(|cx| retained.write.poll(cx)))
            .unwrap();
        drop(retained);
        drop(client);
        assert_eq!(service.reap().unwrap(), 0);
        assert!(weak.upgrade().is_none(), "final physical owner is retired");
        service.quiesce();
        assert!(
            service.reserve().is_err(),
            "escaped capability stays closed"
        );
        match port.finish() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                port.wait_progress_signal().unwrap();
                port.finish().unwrap();
            }
            Err(error) => panic!("native progress retirement: {error}"),
        }
    }
    #[test]
    fn helper_join_remains_terminal_before_last_native_cancel_caller_returns() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let mut port = Port::new(2, 1024 * 1024).unwrap();
        let listening = Listening::new(&port, &path, 4096, true).unwrap();
        let client = Connection::client(&port, &path, 4096, 1024 * 1024).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let token = runtime
            .block_on(std::future::poll_fn(|cx| listening.poll_accept(cx)))
            .unwrap();
        let server = Connection::accepted(token, 4096, 1024 * 1024).unwrap();
        // This test explicitly creates the optional final-drain helper. Its
        // joined state must stay terminal while another native user is held.
        server.drain().unwrap();
        server.read.submit(&[]).unwrap();
        let (entered, observing) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let read = server.read.clone();
        let caller = std::thread::spawn(move || {
            read.controlled_cancel(|handle, overlapped| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                // SAFETY: cancel_using retains this actual HANDLE/OVERLAPPED
                // until this caller returns, even after another cancellation.
                unsafe {
                    windows_sys::Win32::System::IO::CancelIoEx(handle, overlapped);
                }
            });
        });
        observing
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        server.abort();
        server.drain().unwrap().wait_signal().unwrap();
        assert!(server.drain().unwrap().join_ready().unwrap());
        // The dedicated helper is joined, while the deliberately held actual
        // native cancel caller independently keeps this connection physical.
        assert!(!port.retire(&server.token));
        assert!(server.drain().unwrap().join_ready().unwrap());
        assert!(!server.retire(&port).unwrap());
        release.send(()).unwrap();
        caller.join().unwrap();
        assert!(server.drain().unwrap().join_ready().unwrap());
        // Let the completion port publish the cancelled read, independently
        // from the cancellation caller's return.
        runtime
            .block_on(std::future::poll_fn(|cx| server.read.poll(cx)))
            .unwrap();
        assert!(server.retire(&port).unwrap());
        client.abort();
        assert!(client.drain.get().is_none());
        assert!(client.retire(&port).unwrap());
        match port.finish() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                port.wait_progress_signal().unwrap();
                port.finish().unwrap();
            }
            Err(error) => panic!("native progress retirement: {error}"),
        }
    }
    #[test]
    fn owned_pipe_transfers_handle_key_and_drains_while_next_listener_exists() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let mut port = Port::new(3, 1024 * 1024).unwrap();
        let listening = Listening::new(&port, &path, 4096, true).unwrap();
        // Deliberately connect before ConnectNamedPipe: exercise local
        // ERROR_PIPE_CONNECTED without manufacturing a synthetic packet.
        let client = Connection::client(&port, &path, 4096, 1024 * 1024).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (server, next) = runtime.block_on(async {
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx))
                .await
                .unwrap();
            assert!(Arc::ptr_eq(&token, &listening.token));
            let server = Connection::accepted(token, 4096, 1024 * 1024).unwrap();
            let next = Listening::new(&port, &path, 4096, false).unwrap();
            let frame = b"{\"owned\":\"adapter\"}\n";
            let mut writer = Pipe::new(server.clone()).unwrap();
            let mut reader = Pipe::new(client.clone()).unwrap();
            let mut received = [0; 20];
            assert_eq!(frame.len(), received.len());
            let sending = async {
                writer.write_all(frame).await.unwrap();
                writer.shutdown().await.unwrap();
            };
            let reading = async {
                reader.read_exact(&mut received).await.unwrap();
            };
            tokio::join!(sending, reading);
            assert_eq!(&received, frame);
            (server, next)
        });
        // Workers have returned from flush, but their lifetime charges stay
        // until the actual dedicated native threads signal and outer join ends.
        server.abort();
        client.abort();
        next.abort();
        assert!(!port.retire(&server.token));
        assert!(client.drain.get().is_none());
        server.drain().unwrap().wait_signal().unwrap();
        assert!(client.drain.get().is_none());
        assert!(server.retire(&port).unwrap());
        assert!(client.retire(&port).unwrap());
        assert!(port.retire(&next.token));
        match port.finish() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                port.wait_progress_signal().unwrap();
                port.finish().unwrap();
            }
            Err(error) => panic!("native progress retirement: {error}"),
        }
    }
}
