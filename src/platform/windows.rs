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

use interprocess::local_socket::tokio::{Listener as TokioListener, Stream as TokioStream};
use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::{
    ConnectOptions, GenericNamespaced, Listener as SyncListener, ListenerNonblockingMode,
    ListenerOptions, Name, Stream as SyncStream, ToNsName as _,
};
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

/// Liveness probe: a pipe has no persistent name, so probing means connecting
/// once and closing (`None`-byte EOF on the server side, ADR-10).
#[must_use]
pub fn connect_probe(endpoint: &Endpoint) -> bool {
    connect_blocking(endpoint).is_ok()
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

/// Opens (creating if needed) an append-only diagnostics log (ADR-10); file
/// privacy relies on the containing `%LOCALAPPDATA%` ACLs.
///
/// # Errors
/// Returns the raw IO error when the file cannot be opened.
pub fn open_private_log(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}
