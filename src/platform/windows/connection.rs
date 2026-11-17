//! Raw owned pipe endpoints; IOCP keeps write buffers alive after logical acceptance.
#![allow(unsafe_code)] // Only constructors transfer fresh Win32 pipe HANDLE ownership.
use super::flush::Flush;
use super::native_io::{HandleToken, Kind, Operation, Port};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::pin::Pin;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
}
impl Listening {
    pub(super) fn new(port: &Port, path: &str, chunk: usize, first: bool) -> io::Result<Self> {
        let reservation = port.reserve()?;
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
        Ok(Self { token, connect })
    }
    pub(super) fn poll_accept(&self, cx: &Context<'_>) -> Poll<io::Result<Arc<HandleToken>>> {
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
                    .map(|_| self.token.clone()),
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
    drain: Flush,
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
        Self::new(token, chunk, stack, true)
    }
    pub(super) fn client(
        port: &Port,
        path: &str,
        chunk: usize,
        stack: usize,
    ) -> io::Result<Arc<Self>> {
        let reservation = port.reserve()?;
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
        Self::new(token, chunk, stack, false)
    }
    fn new(
        token: Arc<HandleToken>,
        chunk: usize,
        stack: usize,
        server: bool,
    ) -> io::Result<Arc<Self>> {
        let read = Operation::new(token.clone(), Kind::Read, chunk)?;
        let write = Operation::new(token.clone(), Kind::Write, chunk)?;
        // Successful helper creation precedes the first possible dirty write.
        let drain = Flush::new(token.clone(), stack)?;
        Ok(Arc::new(Self {
            token,
            read,
            write,
            drain,
            chunk,
            accepted: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            server,
            disconnected: AtomicBool::new(false),
        }))
    }
    pub(super) fn abort(&self) {
        self.drain.abort();
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
        if !self.drain.join_ready()? {
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

#[derive(Clone)]
pub(super) struct Pipe(pub Arc<Connection>);
impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let connection = &self.get_mut().0;
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
        let connection = &self.get_mut().0;
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        match connection.drain.poll_ready(cx) {
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
        let connection = &self.get_mut().0;
        match connection.write_terminal(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if !connection.dirty.load(Ordering::Acquire) {
            return Poll::Ready(Ok(()));
        }
        match connection.drain.poll_flush(cx) {
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
            let mut writer = Pipe(server.clone());
            let mut reader = Pipe(client.clone());
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
        server.drain.wait_signal().unwrap();
        client.drain.wait_signal().unwrap();
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
