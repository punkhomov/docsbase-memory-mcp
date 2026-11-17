//! Raw owned pipe endpoints; IOCP keeps write buffers alive after logical acceptance.
#![allow(unsafe_code)] // Only constructors transfer fresh Win32 pipe HANDLE ownership.
use super::flush::Flush;
use super::native_io::{HandleToken, Kind, Operation, PhysicalOwner, Port, Service};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
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
        if !self.connect.active() {
            if let Err(error) = self.connect.submit(&[]) {
                return Poll::Ready(Err(error));
            }
        }
        match self.connect.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => Poll::Ready(
                self.connect
                    .consume()
                    .and_then(|completion| completion.result())
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
    drain: OnceLock<Flush>,
    initializing: AtomicBool,
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
        let read = Operation::new(token.clone(), Kind::Read, chunk)?;
        let write = Operation::new(token.clone(), Kind::Write, chunk)?;
        // Register the complete envelope before starting its helper. If helper
        // construction fails or the caller disappears after this return, the
        // SAME bounded registry still owns all physical cleanup. Reaping cannot
        // retire an envelope while this synchronous construction is in flight.
        let owner = Arc::new(Self {
            token,
            read,
            write,
            drain: OnceLock::new(),
            initializing: AtomicBool::new(true),
            chunk,
            accepted: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            server,
            disconnected: AtomicBool::new(false),
        });
        service.attach(&owner.token, owner.clone())?;
        match Flush::new(owner.token.clone(), stack) {
            Ok(drain) => {
                // This private synchronous constructor is the sole writer;
                // the helper cannot be exposed to a logical stream beforehand.
                owner.drain.get_or_init(|| drain);
                owner.initializing.store(false, Ordering::Release);
                Ok(owner)
            }
            Err(error) => {
                owner.abort();
                owner.initializing.store(false, Ordering::Release);
                Err(error)
            }
        }
    }
    fn drain(&self) -> io::Result<&Flush> {
        self.drain
            .get()
            .ok_or_else(|| failure("native helper is not initialized"))
    }
    pub(super) fn abort(&self) {
        self.token.abort();
        if let Some(drain) = self.drain.get() {
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
                    .and_then(|completion| completion.result());
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
        self.drain.get().map_or(Ok(true), Flush::join_ready)
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
        if let Some(drain) = self.connection.drain.get() {
            drain.abort();
        }
    }
}
#[derive(Clone)]
pub(super) struct Pipe {
    logical: Arc<Logical>,
}
impl Pipe {
    pub(super) fn new(connection: Arc<Connection>) -> Self {
        Self {
            logical: Arc::new(Logical { connection }),
        }
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
        if !connection.read.active() {
            if let Err(error) = connection.read.submit(&[]) {
                return Poll::Ready(Err(error));
            }
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
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let drain = match connection.drain() {
            Ok(drain) => drain,
            Err(error) => return Poll::Ready(Err(error)),
        };
        match drain.poll_ready(cx) {
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
        // native write and future Drop. Flush below proves actual write/drain.
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let connection = &self.get_mut().logical.connection;
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if !connection.dirty.load(Ordering::Acquire) {
            return Poll::Ready(Ok(()));
        }
        let drain = match connection.drain() {
            Ok(drain) => drain,
            Err(error) => return Poll::Ready(Err(error)),
        };
        match drain.poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                connection.dirty.store(false, Ordering::Release);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                connection.abort();
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn dropped_dirty_pipe_keeps_registered_owner_until_actual_helper_retirement() {
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
        let mut pipe = Pipe::new(server.clone());
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
        retained.drain().unwrap().wait_signal().unwrap();
        client.drain().unwrap().wait_signal().unwrap();
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
        client.drain().unwrap().wait_signal().unwrap();
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
            let mut writer = Pipe::new(server.clone());
            let mut reader = Pipe::new(client.clone());
            let mut received = [0; 20];
            assert_eq!(frame.len(), received.len());
            let sending = async {
                writer.write_all(frame).await.unwrap();
                writer.flush().await.unwrap();
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
        assert!(!port.retire(&client.token));
        server.drain().unwrap().wait_signal().unwrap();
        client.drain().unwrap().wait_signal().unwrap();
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
