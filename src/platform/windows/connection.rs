//! Application facade over dependency-owned Windows named-pipe I/O.
#![allow(unsafe_code)] // Checked Win32 constructors/cancellation; dependency owns all OVERLAPPED storage.
use super::flush::Flush;
#[cfg(test)]
use super::native_io::Port;
use super::native_io::{HandleToken, PhysicalOwner, RetirementGuard, Service};
use std::io;
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES,
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
        return Err(failure("native pipe name exceeds Windows limit"));
    }
    wide.push(0);
    Ok(wide)
}

enum Endpoint {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}
/// Strong references fence every raw-handle caller, including drain/cancel.
/// Last destruction delegates read/write/connect retirement to patched Mio.
pub(super) struct NativePipe {
    io: Mutex<Endpoint>,
    raw: usize,
}
impl NativePipe {
    pub(super) fn new(
        handle: OwnedHandle,
        server: bool,
        capacity: usize,
        token: Arc<HandleToken>,
    ) -> io::Result<Self> {
        let raw = handle.into_raw_handle();
        let guard = Box::new(RetirementGuard(token));
        // SAFETY: sole ownership of a fresh overlapped byte-mode HANDLE moves
        // together with its accounting guard BEFORE registration/first read.
        // These constructors never disable completion-on-success notifications.
        let io = unsafe {
            if server {
                Endpoint::Server(NamedPipeServer::from_raw_handle_with_retirement_guard(
                    raw, guard, capacity,
                )?)
            } else {
                Endpoint::Client(NamedPipeClient::from_raw_handle_with_retirement_guard(
                    raw, guard, capacity,
                )?)
            }
        };
        Ok(Self {
            io: Mutex::new(io),
            raw: raw as usize,
        })
    }
    pub(super) fn raw(&self) -> std::os::windows::io::RawHandle {
        self.raw as _
    }
    pub(super) fn cancel(&self) {
        // SAFETY: caller's strong NativePipe reference retains this exact HANDLE
        // until CancelIoEx returns. Return (including NOT_FOUND) is not retirement.
        unsafe {
            CancelIoEx(self.raw(), ptr::null());
        }
    }
    pub(super) fn disconnect(&self) {
        // SAFETY: strong reference fences the HANDLE; only failed server
        // exchanges invoke this discard path. No delivery claim follows it.
        unsafe {
            DisconnectNamedPipe(self.raw());
        }
    }
    fn poll_connect(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        match &*io {
            Endpoint::Server(pipe) => pipe.poll_connect(cx),
            Endpoint::Client(_) => Poll::Ready(Err(failure("client cannot accept"))),
        }
    }
    fn poll_read(&self, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *io {
            Endpoint::Server(pipe) => Pin::new(pipe).poll_read(cx, buffer),
            Endpoint::Client(pipe) => Pin::new(pipe).poll_read(cx, buffer),
        }
    }
    fn poll_write(&self, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *io {
            Endpoint::Server(pipe) => Pin::new(pipe).poll_write(cx, bytes),
            Endpoint::Client(pipe) => Pin::new(pipe).poll_write(cx, bytes),
        }
    }
    fn poll_flush(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *io {
            Endpoint::Server(pipe) => Pin::new(pipe).poll_flush(cx),
            Endpoint::Client(pipe) => Pin::new(pipe).poll_flush(cx),
        }
    }
}

pub(super) struct Listening {
    pub token: Arc<HandleToken>,
    pipe: Mutex<Option<Arc<NativePipe>>>,
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
        // SAFETY: reservation precedes HANDLE creation; checked owned name and
        // quota describe a local byte-mode pipe. Physical arena bounds instances.
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
        // SAFETY: fresh sole HANDLE ownership transfers once to dependency.
        let (token, pipe) =
            reservation.associate(unsafe { OwnedHandle::from_raw_handle(raw) }, true, chunk)?;
        let owner = Arc::new(Self {
            token,
            pipe: Mutex::new(Some(pipe)),
            transferred: AtomicBool::new(false),
        });
        service.attach(&owner.token, owner.clone())?;
        Ok(owner)
    }
    pub(super) fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<Arc<HandleToken>>> {
        if self.transferred.load(Ordering::Acquire) {
            return Poll::Ready(Err(failure("listening handle already transferred")));
        }
        let pipe = self
            .pipe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| failure("listener is closed"))?;
        match pipe.poll_connect(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {
                self.transferred.store(true, Ordering::Release);
                Poll::Ready(Ok(self.token.clone()))
            }
        }
    }
    pub(super) fn abort(&self) {
        self.token.abort();
        drop(
            self.pipe
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}
impl PhysicalOwner for Listening {
    fn abort(&self) {
        self.abort();
    }
    fn retire_ready(&self) -> io::Result<bool> {
        Ok(true)
    }
    fn take_native(&self) -> Option<Arc<NativePipe>> {
        self.pipe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

pub(super) struct Connection {
    pub token: Arc<HandleToken>,
    pipe: Mutex<Option<Arc<NativePipe>>>,
    drain: OnceLock<io::Result<Flush>>,
    initializing: AtomicBool,
    logical_claimed: AtomicBool,
    helper_stack_bytes: usize,
    closing: AtomicBool,
    closed: AtomicBool,
    chunk: usize,
    dirty: AtomicBool,
    server: bool,
    disconnected: AtomicBool,
    #[cfg(test)]
    read_active: AtomicBool,
}
impl Connection {
    pub(super) fn accepted(
        token: Arc<HandleToken>,
        chunk: usize,
        stack: usize,
    ) -> io::Result<Arc<Self>> {
        let service = token.service()?;
        let pipe = service.accepted(&token)?;
        Self::new(&service, token, pipe, chunk, stack, true)
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
        // SAFETY: single overlapped open, under pre-reserved physical capacity.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fresh sole ownership moves once to guarded dependency.
        let (token, pipe) =
            reservation.associate(unsafe { OwnedHandle::from_raw_handle(raw) }, false, chunk)?;
        Self::new(service, token, pipe, chunk, stack, false)
    }
    fn new(
        service: &Service,
        token: Arc<HandleToken>,
        pipe: Arc<NativePipe>,
        chunk: usize,
        stack: usize,
        server: bool,
    ) -> io::Result<Arc<Self>> {
        let owner = Arc::new(Self {
            token,
            pipe: Mutex::new(Some(pipe)),
            drain: OnceLock::new(),
            initializing: AtomicBool::new(false),
            logical_claimed: AtomicBool::new(false),
            helper_stack_bytes: stack,
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            chunk,
            dirty: AtomicBool::new(false),
            server,
            disconnected: AtomicBool::new(false),
            #[cfg(test)]
            read_active: AtomicBool::new(false),
        });
        service.attach(&owner.token, owner.clone())?;
        Ok(owner)
    }
    fn native(&self) -> io::Result<Arc<NativePipe>> {
        self.pipe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn drain(&self) -> io::Result<&Flush> {
        self.initializing.store(true, Ordering::Release);
        let result = self
            .drain
            .get_or_init(|| Flush::new(self.token.clone(), self.helper_stack_bytes));
        self.initializing.store(false, Ordering::Release);
        match result {
            Ok(drain) => Ok(drain),
            Err(error) => {
                self.abort();
                Err(io::Error::new(error.kind(), error.to_string()))
            }
        }
    }
    #[cfg(test)]
    pub(super) fn test_read_active(&self) -> bool {
        self.read_active.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn test_cancel_using(&self, work: impl FnOnce(std::os::windows::io::RawHandle)) {
        if let Ok(pipe) = self.token.native() {
            work(pipe.raw());
            drop(pipe); // The original HANDLE remains live across the whole caller.
        }
    }
    #[cfg(test)]
    pub(super) fn test_hold_write_cancellation(
        &self,
        entered: &std::sync::mpsc::Sender<()>,
        release: &std::sync::mpsc::Receiver<()>,
    ) {
        self.test_cancel_using(|handle| {
            // SAFETY: strong native owner fences the HANDLE through this gate.
            unsafe {
                CancelIoEx(handle, ptr::null());
            }
            let _ = entered.send(());
            let _ = release.recv();
        });
    }
    pub(super) fn retain(&self, request: bool, charge: Arc<dyn Send + Sync>) -> io::Result<()> {
        self.token.retain(request, charge)
    }
    pub(super) fn request_completed(&self) {
        if !self.closing.load(Ordering::Acquire) && self.token.writes_idle() {
            self.token.request_completed();
        }
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
        drop(
            self.pipe
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
    #[cfg(test)]
    pub(super) fn retire(&self, port: &Port) -> io::Result<bool> {
        self.abort();
        Ok(self.retire_ready()? && port.retire(&self.token))
    }
    fn write_terminal(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = self.native()?.poll_flush(cx);
        if let Poll::Ready(Ok(())) = result {
            self.token.write_active.store(false, Ordering::Release);
        }
        result
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
}
struct Logical {
    connection: Arc<Connection>,
}
impl Drop for Logical {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
pub(super) struct Pipe {
    logical: Arc<Logical>,
}
impl Pipe {
    pub(super) fn new(connection: Arc<Connection>) -> io::Result<Self> {
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
        let result = connection.native()?.poll_read(cx, buffer);
        #[cfg(test)]
        connection
            .read_active
            .store(result.is_pending(), Ordering::Release);
        match result {
            Poll::Ready(Err(error)) if matches!(error.raw_os_error(), Some(109 | 232 | 233)) => {
                Poll::Ready(Ok(()))
            }
            other => other,
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
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let result = connection
            .native()?
            .poll_write(cx, &bytes[..bytes.len().min(connection.chunk)]);
        if let Poll::Ready(Ok(count)) = result
            && count != 0
        {
            connection.token.write_active.store(true, Ordering::Release);
            connection.dirty.store(true, Ordering::Release);
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().logical.connection.write_terminal(cx)
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
        if !connection.server || !connection.dirty.load(Ordering::Acquire) {
            connection.closed.store(true, Ordering::Release);
            return Poll::Ready(Ok(()));
        }
        match connection.drain()?.poll_flush(cx) {
            Poll::Ready(Ok(())) => {
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
    use std::future::Future;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn impersonation_level(pipe: &NativePipe) -> io::Result<i32> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Security::{
            GetTokenInformation, RevertToSelf, TOKEN_QUERY, TokenImpersonationLevel,
        };
        use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
        // SAFETY: the borrowed server endpoint remains strongly owned after a
        // real read. The test always restores this thread before inspecting errors.
        if unsafe { ImpersonateNamedPipeClient(pipe.raw()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let observed = (|| {
            let mut raw = ptr::null_mut();
            // SAFETY: query only this thread's token into an initialized output.
            if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful OpenThreadToken transfers one owned HANDLE.
            let token = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut level = 0_i32;
            let mut returned = 0;
            // SAFETY: this information class returns one i32 in the supplied
            // exact-size output; the token remains owned through the query.
            if unsafe {
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenImpersonationLevel,
                    (&raw mut level).cast(),
                    4,
                    &raw mut returned,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(level)
        })();
        // SAFETY: restore the test thread regardless of the token-query result.
        assert_ne!(
            unsafe { RevertToSelf() },
            0,
            "restore original test identity"
        );
        observed
    }

    #[test]
    fn native_client_requests_identification_only_impersonation() {
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
        let mut writer = Pipe::new(client.clone()).unwrap();
        let mut reader = Pipe::new(server.clone()).unwrap();
        runtime.block_on(async {
            writer.write_all(b"x").await.unwrap();
            writer.flush().await.unwrap();
            reader.read_exact(&mut [0]).await.unwrap();
        });
        let observed = impersonation_level(&server.native().unwrap());
        drop((reader, writer));
        runtime.block_on(async {
            server.token.test_native_terminal().await;
            client.token.test_native_terminal().await;
        });
        assert!(server.retire(&port).unwrap());
        assert!(client.retire(&port).unwrap());
        port.finish().unwrap();
        assert_eq!(
            observed.unwrap(),
            windows_sys::Win32::Security::SecurityIdentification,
            "native client must preserve Tokio's identification-only SQOS"
        );
    }

    #[test]
    fn failed_native_opens_return_unassociated_capacity_without_a_reaper() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let mut port = Port::new(1, 1024 * 1024).unwrap();
        for _ in 0..32 {
            let error = Connection::client(&port, &path, 4096, 1024 * 1024)
                .err()
                .expect("no server exists");
            assert_eq!(
                error.raw_os_error(),
                Some(2),
                "failed CreateFile must release its unused reservation immediately"
            );
        }
        let listening = Listening::new(&port, &path, 4096, true)
            .expect("failed opens cannot exhaust the one physical slot");
        listening.abort();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(listening.token.test_native_terminal());
        assert!(port.retire(&listening.token));
        port.finish().unwrap();
    }

    #[test]
    fn accept_polled_before_client_keeps_its_native_wakeup() {
        struct AcceptWake(std::sync::mpsc::Sender<()>);
        impl std::task::Wake for AcceptWake {
            fn wake(self: Arc<Self>) {
                let _ = self.0.send(());
            }
            fn wake_by_ref(self: &Arc<Self>) {
                let _ = self.0.send(());
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let mut port = Port::new(2, 1024 * 1024).unwrap();
        let listening = Listening::new(&port, &path, 4096, true).unwrap();
        let (notifications, observed) = std::sync::mpsc::channel();
        let waker = std::task::Waker::from(Arc::new(AcceptWake(notifications)));
        assert!(
            listening
                .poll_accept(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let client = Connection::client(&port, &path, 4096, 1024 * 1024).unwrap();
        // No application runtime, timer or manual re-poll can supply this wake.
        let notified = observed
            .recv_timeout(std::time::Duration::from_secs(30))
            .is_ok();
        listening.abort();
        client.abort();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            listening.token.test_native_terminal().await;
            client.token.test_native_terminal().await;
        });
        assert!(port.retire(&listening.token));
        assert!(client.retire(&port).unwrap());
        port.finish().unwrap();
        assert!(
            notified,
            "native connect completion must wake pending accept"
        );
    }

    #[test]
    fn blocking_connection_waits_for_the_next_instance_with_one_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let policy = super::super::root::Options {
            handles: 4,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run(&policy, move |service| {
            let transport = super::super::transport::Transport::new(service.clone(), options);
            let _listener = transport.bind(&endpoint)?;
            let _occupied = transport.connect_blocking(&endpoint)?;
            let timed_out = transport.connect_blocking_until(
                &endpoint,
                std::time::Instant::now() + std::time::Duration::from_millis(1),
            );
            assert_eq!(timed_out.err().unwrap().kind(), io::ErrorKind::TimedOut);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut waits = 0;
            let mut previous_remaining = std::time::Duration::from_secs(30);
            let mut next = None;
            let connected = transport.connection_until(&endpoint, deadline, |remaining| {
                waits += 1;
                assert!(!remaining.is_zero());
                assert!(remaining <= previous_remaining);
                previous_remaining = remaining;
                if waits == 1 {
                    // Availability can be consumed by another client before
                    // CreateFile. The next busy result keeps the same deadline.
                    return Ok(());
                }
                // Publish a real pending instance only after busy selected
                // the wait path, then exercise the actual Win32 wait.
                let listening = Listening::new_in(&service, &path, 4096, false)?;
                assert!(
                    listening
                        .poll_accept(&mut Context::from_waker(std::task::Waker::noop()))
                        .is_pending()
                );
                next = Some(listening);
                super::super::transport::wait_named_pipe(&endpoint, remaining)
            })?;
            let _client = Pipe::new(connected)?;
            assert_eq!(waits, 2);
            let expired = transport.connection_until(&endpoint, std::time::Instant::now(), |_| {
                panic!("expired connection must not wait or restart its deadline")
            });
            assert_eq!(expired.err().unwrap().kind(), io::ErrorKind::TimedOut);
            drop(next);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn client_departure_before_accept_retires_instance_and_keeps_listener_alive() {
        use std::future::Future;
        let directory = tempfile::tempdir().unwrap();
        let endpoint = crate::platform::daemon_endpoint(directory.path());
        let path = super::super::pipe_path(&endpoint).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = super::super::root::Options {
            handles: 3,
            chunk_bytes: 4096,
            helper_stack_bytes: 1024 * 1024,
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: std::time::Duration::from_millis(1),
            drain_observation: std::time::Duration::from_secs(30),
        };
        let options = policy.clone();
        super::super::root::run_on(&runtime, &policy, move |service| async move {
            let transport = super::super::transport::Transport::new(service, options);
            let listener = transport.bind(&endpoint)?;
            // A plain File has no background operations: dropping it closes
            // the connected peer before ConnectNamedPipe is ever submitted.
            let departed = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)?;
            drop(departed);
            let mut accepting = std::pin::pin!(listener.accept());
            let pending =
                std::future::poll_fn(|cx| Poll::Ready(accepting.as_mut().poll(cx).is_pending()))
                    .await;
            assert!(
                pending,
                "a departed peer must not terminate accept with ERROR_NO_DATA"
            );
            let connecting = async {
                loop {
                    match transport.connect_async(&endpoint).await {
                        Ok(client) => return Ok::<_, io::Error>(client),
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                            tokio::task::yield_now().await;
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let (mut client, mut server) =
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::try_join!(connecting, accepting)
                })
                .await
                .map_err(io::Error::other)??;
            client.write_all(b"alive").await?;
            client.flush().await?;
            let mut bytes = [0; 5];
            server.read_exact(&mut bytes).await?;
            assert_eq!(&bytes, b"alive");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One causal prefix/native-pending/tail-release delivery protocol.
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
                let drain = server.drain()?;
                let mut closing = Box::pin(writer.shutdown());
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    loop {
                        let progress =
                            std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx))).await;
                        assert!(
                            progress.is_pending(),
                            "close returned before reading the kernel tail: {progress:?}"
                        );
                        if drain.test_native_io_pending()? {
                            break Ok::<_, io::Error>(());
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .map_err(io::Error::other)??;
                let peer = client.native()?;
                let mut unread = 0;
                // SAFETY: this strong endpoint owner retains the borrowed pipe;
                // PeekNamedPipe only observes kernel bytes and consumes none.
                let peeked = unsafe {
                    windows_sys::Win32::System::Pipes::PeekNamedPipe(
                        peer.raw(),
                        ptr::null_mut(),
                        0,
                        ptr::null_mut(),
                        &raw mut unread,
                        ptr::null_mut(),
                    )
                };
                assert_ne!(peeked, 0, "peek must observe the live peer");
                assert!(
                    unread > 0,
                    "read-ahead cannot consume the gated kernel tail"
                );
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
                drop(writer);
                Ok::<_, io::Error>(())
            };
            let reading = async {
                let mut received = vec![0; frame.len()];
                // Mio may prefetch one more byte after the consumed prefix.
                // Leave two bytes gated, so at least one remains in the kernel.
                let tail = frame.len() - 2;
                reader.read_exact(&mut received[..tail]).await?;
                prefix_sent
                    .send(())
                    .map_err(|()| failure("close observer lost"))?;
                read_tail.await.map_err(io::Error::other)?;
                reader.read_exact(&mut received[tail..]).await?;
                assert_eq!(received, frame);
                assert_eq!(reader.read(&mut [0]).await?, 0, "final disconnect is EOF");
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

    /// Test-only owner: every unwind/error releases and joins the actual caller.
    struct HeldReadCancellation {
        held: bool,
        release: Option<std::sync::mpsc::Sender<()>>,
        caller: Option<std::thread::JoinHandle<()>>,
    }
    impl HeldReadCancellation {
        fn new(connection: Arc<Connection>) -> Self {
            let (entered, observed) = std::sync::mpsc::channel();
            let (release, proceed) = std::sync::mpsc::channel();
            let caller = std::thread::spawn(move || {
                connection.test_cancel_using(|handle| {
                    // SAFETY: controlled_cancel owns this handle/operation until
                    // this actual cancellation caller returns, including the gate.
                    unsafe {
                        windows_sys::Win32::System::IO::CancelIoEx(handle, std::ptr::null());
                    }
                    let _ = entered.send(());
                    let _ = proceed.recv();
                });
            });
            let held = observed
                .recv_timeout(std::time::Duration::from_secs(30))
                .is_ok();
            Self {
                held,
                release: Some(release),
                caller: Some(caller),
            }
        }
    }
    impl Drop for HeldReadCancellation {
        fn drop(&mut self) {
            drop(self.release.take());
            if let Some(caller) = self.caller.take() {
                caller.join().unwrap();
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One causal cancel/retention/control/release protocol.
    fn repeated_cancelled_reads_keep_bounded_capacity_and_control_progress() {
        use std::future::Future;
        let directory = tempfile::tempdir().unwrap();
        let control = crate::platform::daemon_endpoint(directory.path());
        let churn = crate::platform::daemon_endpoint(&directory.path().join("churn"));
        let path = super::super::pipe_path(&control).unwrap();
        let churn_path = super::super::pipe_path(&churn).unwrap();
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
        super::super::root::run_on(&runtime, &policy, move |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 4096, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 4096, 1024 * 1024)?;
            let mut writer = Pipe::new(server.clone())?;
            let mut reader = Pipe::new(client.clone())?;
            let mut high_water = 0;
            for _ in 0..32 {
                let pending_listener = Listening::new_in(&service, &churn_path, 4096, true)?;
                let peer = Connection::client_in(&service, &churn_path, 4096, 1024 * 1024)?;
                let token = std::future::poll_fn(|cx| pending_listener.poll_accept(cx)).await?;
                let accepted = Connection::accepted(token, 4096, 1024 * 1024)?;
                let mut dirty = Pipe::new(accepted.clone())?;
                let mut other = Pipe::new(peer.clone())?;
                let mut byte = [0];
                let mut opposite = [0];
                {
                    let pending = dirty.read_exact(&mut byte);
                    let reverse = other.read_exact(&mut opposite);
                    tokio::pin!(pending, reverse);
                    std::future::poll_fn(|cx| {
                        assert!(pending.as_mut().poll(cx).is_pending());
                        assert!(reverse.as_mut().poll(cx).is_pending());
                        std::task::Poll::Ready(())
                    }).await;
                }
                // Actual CancelIoEx caller charges remain live on BOTH endpoints
                // across the entire control exchange, even after IOCP completion.
                let cancellation = HeldReadCancellation::new(accepted);
                let peer_cancel = HeldReadCancellation::new(peer);
                let held = cancellation.held && peer_cancel.held;
                drop((dirty, other, pending_listener));
                let retained = service.reap()?;
                let full = matches!(service.reserve(), Err(error) if error.kind() == io::ErrorKind::WouldBlock);
                let mut received = [0; 5];
                let control_reply = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::try_join!(async {
                        writer.write_all(b"tick\n").await?;
                        writer.flush().await
                    }, reader.read_exact(&mut received))
                }).await;
                let after_reply = service.reap()?;
                high_water = high_water.max(after_reply);
                drop((cancellation, peer_cancel)); // Release and join before assertions.
                assert!(held, "both actual native cancellation callers must enter");
                assert_eq!((retained, after_reply), (4, 4), "retirement remains charged through the control reply");
                assert!(full, "retiring native work must exhaust the physical arena");
                control_reply.map_err(io::Error::other)??;
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
            eprintln!("native cancellation churn: cycles=32 held_cancel_callers=2 physical_before_and_after_control=4 remaining_control=2 control_replies=32 drain_helpers=0");
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
                assert!(server.token.writes_idle(), "native write barrier completed");
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
        let client = Connection::client(&port, &path, 1, 1024 * 1024).unwrap();
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
        runtime
            .block_on(pipe.write_all(&vec![b'x'; 65_536]))
            .unwrap();
        assert!(
            Pin::new(&mut pipe)
                .poll_flush(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        let held = HeldReadCancellation::new(server.clone());
        assert!(held.held, "actual native caller must be retained");
        drop((pipe, server));
        assert!(weak.upgrade().is_some());
        assert!(matches!(service.reserve(), Err(error) if error.kind()==io::ErrorKind::WouldBlock));
        assert_eq!(
            service.reap().unwrap(),
            2,
            "held native caller preserves physical capacity"
        );
        drop(held);
        client.abort();
        runtime.block_on(async {
            client.token.test_native_terminal().await;
            weak.upgrade().unwrap().token.test_native_terminal().await;
        });
        drop(client);
        assert_eq!(service.reap().unwrap(), 0);
        assert!(weak.upgrade().is_none());
        service.quiesce();
        assert!(service.reserve().is_err());
        port.finish().unwrap();
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
        server.drain().unwrap();
        let (entered, observing) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let original = server.clone();
        let caller = std::thread::spawn(move || {
            original.test_cancel_using(|handle| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                // SAFETY: test_cancel_using retains the original HANDLE through return.
                unsafe {
                    CancelIoEx(handle, std::ptr::null());
                }
            });
        });
        observing
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        server.abort();
        server.drain().unwrap().wait_signal().unwrap();
        assert!(server.drain().unwrap().join_ready().unwrap());
        assert!(!port.retire(&server.token));
        assert!(server.drain().unwrap().join_ready().unwrap());
        assert!(!server.retire(&port).unwrap());
        release.send(()).unwrap();
        caller.join().unwrap();
        runtime.block_on(server.token.test_native_terminal());
        assert!(server.retire(&port).unwrap());
        client.abort();
        runtime.block_on(client.token.test_native_terminal());
        assert!(client.retire(&port).unwrap());
        port.finish().unwrap();
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
        assert!(server.drain().unwrap().join_ready().unwrap());
        assert!(client.drain.get().is_none());
        runtime.block_on(async {
            server.token.test_native_terminal().await;
            client.token.test_native_terminal().await;
            next.token.test_native_terminal().await;
        });
        assert!(server.retire(&port).unwrap());
        assert!(client.retire(&port).unwrap());
        assert!(port.retire(&next.token));
        port.finish().unwrap();
    }
}
