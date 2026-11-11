//! Windows x64 backend of the platform seam (ADR-10, T39–T41).
//!
//! Transport is built on the `interprocess` crate's local sockets (named
//! pipes); signals, process liveness and permissions complete the backend
//! (ADR-10). Behavior mirrors the Unix backend: byte-stream NDJSON, blocking
//! client, async server.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll};
use std::time::Duration;

use interprocess::ConnectWaitMode;
use interprocess::local_socket::tokio::{Listener as TokioListener, Stream as TokioStream};
use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::{
    ConnectOptions, GenericNamespaced, Listener as SyncListener, ListenerNonblockingMode,
    ListenerOptions, Name, Stream as SyncStream, ToNsName as _,
};
use interprocess::os::windows::named_pipe::tokio::DuplexPipeStream as AsyncPipeStream;
use interprocess::os::windows::named_pipe::{DuplexPipeStream, pipe_mode::Bytes};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::signal::windows::{CtrlC, CtrlClose, ctrl_c, ctrl_close};
use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS, GetExitCodeProcess, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::Endpoint;

/// Endpoint of the daemon serving `cache`: a per-cache named pipe (ADR-10).
#[must_use]
pub fn daemon_endpoint(cache: &Path) -> Endpoint {
    Endpoint::Pipe(super::pipe_name(cache))
}

fn ns_name(endpoint: &Endpoint) -> io::Result<Name<'_>> {
    match endpoint {
        Endpoint::Pipe(name) => name.as_str().to_ns_name::<GenericNamespaced>(),
        Endpoint::Unix(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unix endpoint submitted to the Windows backend",
        )),
    }
}

/// Async listener on a named pipe.
pub struct Listener(TokioListener);

impl Listener {
    /// Accepts the next incoming connection.
    ///
    /// # Errors
    /// Returns the raw accept error.
    pub async fn accept(&self) -> io::Result<Stream> {
        self.0.accept().await.map(Stream)
    }
}

/// Async connection stream (`AsyncRead + AsyncWrite`).
pub struct Stream(TokioStream);

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// Blocking connection stream for CLI clients and fake daemons.
pub struct BlockingStream(SyncStream);

impl BlockingStream {
    /// Named pipes do not support I/O timeouts (ADR-10): best-effort no-op so
    /// callers keep their Unix semantics without failing to connect.
    ///
    /// # Errors
    /// Never fails today; the signature mirrors the Unix backend.
    pub fn set_read_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    /// See [`Self::set_read_timeout`].
    ///
    /// # Errors
    /// Never fails today; the signature mirrors the Unix backend.
    pub fn set_write_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    /// Clones the underlying handle for a reader/writer split.
    ///
    /// # Errors
    /// Returns the raw IO error when the handle cannot be cloned.
    pub fn try_clone(&self) -> io::Result<Self> {
        interprocess::TryClone::try_clone(&self.0).map(Self)
    }
}

impl Read for BlockingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for BlockingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Blocking listener used by fakes and tests.
pub struct BlockingListener(SyncListener);

impl BlockingListener {
    /// Accepts the next incoming connection.
    ///
    /// # Errors
    /// Returns the raw accept error.
    pub fn accept(&self) -> io::Result<BlockingStream> {
        self.0.accept().map(BlockingStream)
    }

    /// Switches accept between blocking and polling.
    ///
    /// # Errors
    /// Returns the raw IO error.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mode = if nonblocking {
            ListenerNonblockingMode::Accept
        } else {
            ListenerNonblockingMode::Neither
        };
        self.0.set_nonblocking(mode)
    }

    /// Iterator over incoming connections; stops after the first error.
    #[must_use]
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming {
            listener: self,
            done: false,
        }
    }
}

/// Iterator returned by [`BlockingListener::incoming`].
pub struct Incoming<'a> {
    listener: &'a BlockingListener,
    done: bool,
}

impl Iterator for Incoming<'_> {
    type Item = io::Result<BlockingStream>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.listener.accept() {
            Ok(stream) => Some(Ok(stream)),
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

/// Binds the async listener for `endpoint`.
///
/// Must be called from within an IO-enabled Tokio runtime.
///
/// # Errors
/// Returns the raw bind error.
pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    ListenerOptions::new()
        .name(ns_name(endpoint)?)
        .create_tokio()
        .map(Listener)
}

/// Binds the blocking listener for fakes and tests.
///
/// # Errors
/// Returns the raw bind error.
pub fn bind_blocking(endpoint: &Endpoint) -> io::Result<BlockingListener> {
    ListenerOptions::new()
        .name(ns_name(endpoint)?)
        .create_sync()
        .map(BlockingListener)
}

/// Connects a blocking client stream to `endpoint`.
///
/// # Errors
/// Returns the raw connect error.
pub fn connect_blocking(endpoint: &Endpoint) -> io::Result<BlockingStream> {
    ConnectOptions::new()
        .name(ns_name(endpoint)?)
        .connect_sync()
        .map(BlockingStream)
}

/// Makes one nonblocking connection attempt through the IO runtime.
///
/// # Errors
/// Returns invalid endpoint and raw connect errors, or `TimedOut` when busy.
pub async fn connect_async(endpoint: &Endpoint) -> io::Result<Stream> {
    let path = pipe_path(endpoint)?;
    // interprocess 2.4.4 local-socket adapters ignore the wait mode. Their
    // default busy-pipe path spawns an unbounded blocking worker, which can
    // survive future cancellation and delay runtime Drop. ZERO selects the
    // raw API's single OS attempt; retry/budget ownership stays with callers.
    AsyncPipeStream::<Bytes>::connect_by_path_with_wait_mode(
        path.as_str(),
        ConnectWaitMode::Timeout(Duration::ZERO),
    )
    .await
    .map(|pipe| Stream(TokioStream::NamedPipe(pipe.into())))
}

// Match GenericNamespaced's local \\.\pipe\ mapping used by our listeners.
fn pipe_path(endpoint: &Endpoint) -> io::Result<String> {
    match endpoint {
        Endpoint::Pipe(name) => Ok(format!(r"\\.\pipe\{name}")),
        Endpoint::Unix(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unix endpoint submitted to the Windows backend",
        )),
    }
}

/// Probes once without waiting for a free instance. A busy pipe is present;
/// an available pipe is connected and closed (byte EOF on the server, ADR-10).
#[must_use]
pub fn connect_probe(endpoint: &Endpoint) -> bool {
    let Ok(path) = pipe_path(endpoint) else {
        return false;
    };
    match DuplexPipeStream::<Bytes>::connect_by_path_with_wait_mode(
        path.as_str(),
        ConnectWaitMode::Timeout(Duration::ZERO),
    ) {
        Ok(_) => true,
        Err(error) => error.kind() == io::ErrorKind::TimedOut,
    }
}

/// Same as [`connect_probe`]: named pipes have no file to check.
#[must_use]
pub fn exists(endpoint: &Endpoint) -> bool {
    connect_probe(endpoint)
}

/// Named pipes disappear with their last handle; nothing to remove.
///
/// # Errors
/// Never fails today; the signature mirrors the Unix backend.
pub fn remove(_endpoint: &Endpoint) -> io::Result<()> {
    Ok(())
}

/// Terminates the daemon's accept loop on `CTRL_C` or a console close event
/// (equivalent of the Unix SIGTERM/SIGINT pair; ADR-10).
pub struct ShutdownSignal {
    ctrl_c: CtrlC,
    ctrl_close: CtrlClose,
}

impl ShutdownSignal {
    /// Installs both handlers (must be called inside a Tokio runtime).
    ///
    /// # Errors
    /// Returns the raw IO error when a handler cannot be installed.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            ctrl_c: ctrl_c()?,
            ctrl_close: ctrl_close()?,
        })
    }

    /// Resolves when `CTRL_C` or a console close event arrives.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_c.recv() => {}
            _ = self.ctrl_close.recv() => {}
        }
    }
}

/// True while `pid` exists and has not exited (ADR-10).
#[allow(unsafe_code)] // windows-sys FFI: SAFETY comments on each block
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: OpenProcess only queries the kernel; an unknown pid yields NULL.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code = 0_u32;
    // SAFETY: `handle` is a valid process handle returned by OpenProcess.
    let ok = unsafe { GetExitCodeProcess(handle, std::ptr::from_mut(&mut code)) };
    // SAFETY: `handle` is still valid and unused after this call.
    unsafe { CloseHandle(handle) };
    ok != 0 && code == STILL_ACTIVE as u32
}

/// Open file descriptors are not exposed as a cheap counter on Windows
/// (ADR-10): reported as 0 like the kernel-independent Unix fallback.
#[must_use]
pub fn fd_count() -> u64 {
    0
}

/// OS thread count is not exposed as a cheap counter on Windows (ADR-10):
/// reported as 0 like the kernel-independent Unix fallback.
#[must_use]
pub fn thread_count() -> u64 {
    0
}

/// Detaches the child into its own process group without a console window
/// (ADR-10), mirroring `process_group(0)` on Unix.
pub fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// Creates `dir` (and parents); privacy comes from `%LOCALAPPDATA%` ACLs
/// (ADR-10), so no chmod-style step is attempted.
///
/// # Errors
/// Returns the raw IO error when creation fails.
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// No chmod equivalent on Windows (ADR-10): best-effort no-op.
///
/// # Errors
/// Never fails today; the signature mirrors the Unix backend.
pub fn secure_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// No chmod equivalent on Windows (ADR-10): best-effort no-op.
///
/// # Errors
/// Never fails today; the signature mirrors the Unix backend.
pub fn secure_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Free bytes available on the filesystem holding `path` (Win32
/// `GetDiskFreeSpaceExW`); used by the fetch/connect free-space preflight
/// (FR-13).
///
/// # Errors
/// Returns the raw IO error when the call fails.
#[allow(unsafe_code)] // windows-sys FFI: SAFETY comments on each block
pub fn available_space(path: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide = path_to_wide(path)?;
    let mut free_bytes: u64 = 0;
    // SAFETY: `free_bytes` points at a valid u64; the other two out-params
    // are optional (NULL). The path buffer is NUL-terminated.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut free_bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free_bytes)
}

/// Converts a path to a NUL-terminated UTF-16 buffer for Win32 calls.
fn path_to_wide(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    if wide.len() > u32::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path too long"));
    }
    Ok(wide)
}

/// Opens (creating if needed) an append-only diagnostics log (ADR-10); file
/// privacy relies on the containing `%LOCALAPPDATA%` ACLs.
///
/// # Errors
/// Returns the raw IO error when the file cannot be opened.
pub fn open_private_log(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// Opens an existing path read-only, opening a final reparse point itself.
/// Does not create or truncate; callers reject symlink/non-regular handles.
///
/// # Errors
/// Returns the raw IO error when the path cannot be opened without following it.
pub fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future as _;
    use std::sync::mpsc;
    use std::task::Waker;
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    #[test]
    fn busy_pipe_async_connect_is_immediate() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        // Reserve the only blocking worker. If old connect enqueues a worker,
        // it cannot enter WaitNamedPipe before the busy endpoint is removed.
        let reserved = runtime.spawn_blocking(move || {
            entered_tx.send(()).expect("entered");
            release_rx.recv().expect("release");
        });
        entered_rx.recv().expect("blocking worker reserved");
        let outcome = runtime.block_on(async {
            let cache = tempfile::TempDir::new().expect("cache");
            let endpoint = daemon_endpoint(cache.path());
            let Endpoint::Pipe(name) = &endpoint else {
                panic!("Windows endpoint");
            };
            let path = format!(r"\\.\pipe\{name}");
            let server = ServerOptions::new()
                .max_instances(1)
                .first_pipe_instance(true)
                .create(&path)
                .expect("single pipe instance");
            let client = ClientOptions::new().open(&path).expect("occupy pipe");
            server.connect().await.expect("occupied connection");
            let outcome = {
                let mut connect = std::pin::pin!(connect_async(&endpoint));
                let mut cx = Context::from_waker(Waker::noop());
                connect
                    .as_mut()
                    .poll(&mut cx)
                    .map(|result| result.err().map(|e| e.kind()))
            };
            // Cleanup precedes releasing the reserved worker and any assertion.
            drop(client);
            drop(server);
            outcome
        });
        release_tx.send(()).expect("release reserved worker");
        runtime.block_on(reserved).expect("reserved worker joined");
        drop(runtime);
        assert_eq!(outcome, Poll::Ready(Some(io::ErrorKind::TimedOut)));
    }

    #[tokio::test]
    async fn busy_pipe_probe_reports_presence() {
        let cache = tempfile::TempDir::new().expect("cache");
        let endpoint = daemon_endpoint(cache.path());
        let path = pipe_path(&endpoint).expect("pipe path");
        let server = ServerOptions::new()
            .max_instances(1)
            .first_pipe_instance(true)
            .create(&path)
            .expect("single pipe instance");
        let client = ClientOptions::new().open(&path).expect("occupy pipe");
        server.connect().await.expect("occupied connection");
        let present = connect_probe(&endpoint);
        drop(client);
        drop(server);
        assert!(present, "busy endpoint is still present");
        assert!(!connect_probe(&endpoint), "last handle removed");
    }

    #[tokio::test]
    async fn available_pipe_async_connect_preserves_byte_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let cache = tempfile::TempDir::new().expect("cache");
        let endpoint = daemon_endpoint(cache.path());
        let listener = bind(&endpoint).expect("bind");
        let (mut client, mut server) =
            tokio::try_join!(connect_async(&endpoint), listener.accept()).expect("connect/accept");
        client.write_all(b"ping\n").await.expect("client write");
        client.flush().await.expect("client flush");
        let mut bytes = [0; 5];
        server.read_exact(&mut bytes).await.expect("server read");
        assert_eq!(&bytes, b"ping\n");
        server.write_all(b"pong\n").await.expect("server write");
        server.flush().await.expect("server flush");
        client.read_exact(&mut bytes).await.expect("client read");
        assert_eq!(&bytes, b"pong\n");
    }

    #[tokio::test]
    async fn absent_pipe_async_connect_returns_not_found() {
        let cache = tempfile::TempDir::new().expect("cache");
        let endpoint = daemon_endpoint(cache.path());
        let error = connect_async(&endpoint)
            .await
            .err()
            .expect("absent endpoint");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!connect_probe(&endpoint));
    }

    #[tokio::test]
    async fn unix_endpoint_async_connect_is_invalid_on_windows() {
        let endpoint = Endpoint::Unix(std::path::PathBuf::from("invalid-unix-endpoint"));
        let error = connect_async(&endpoint)
            .await
            .err()
            .expect("invalid endpoint");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!connect_probe(&endpoint));
    }
}
