//! Unix implementation of the platform transport seam (ADR-9, v1).
//!
//! Everything here is a thin delegation to `std`/`tokio` Unix primitives;
//! behavior (framing, timeouts, backlog) is unchanged from the pre-seam code.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::signal::unix::{Signal, SignalKind};

use super::Endpoint;

/// Async listener bound to a local endpoint.
pub struct Listener(tokio::net::UnixListener);

impl Listener {
    /// Accepts the next incoming connection.
    ///
    /// # Errors
    /// Returns the raw accept error (the peer address is not exposed —
    /// callers do not use it).
    pub async fn accept(&self) -> io::Result<Stream> {
        self.0
            .accept()
            .await
            .map(|(stream, _address)| Stream(stream))
    }
}

/// Async connection stream (`AsyncRead + AsyncWrite`).
pub struct Stream(tokio::net::UnixStream);

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

/// Blocking connection stream for CLI clients, probes and stop requests.
pub struct BlockingStream(UnixStream);

impl BlockingStream {
    /// Adjusts the read timeout of one exchange (long tools need longer).
    ///
    /// # Errors
    /// Returns the raw IO error when the socket refuses the option.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    /// Adjusts the write timeout of one exchange.
    ///
    /// # Errors
    /// Returns the raw IO error when the socket refuses the option.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    /// Clones the underlying handle for a reader/writer split.
    ///
    /// # Errors
    /// Returns the raw IO error when the handle cannot be cloned.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
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

/// Blocking listener used by fakes and tests (parity with the async facade).
pub struct BlockingListener(UnixListener);

impl BlockingListener {
    /// Accepts the next incoming connection.
    ///
    /// # Errors
    /// Returns the raw accept error.
    pub fn accept(&self) -> io::Result<BlockingStream> {
        self.0
            .accept()
            .map(|(stream, _address)| BlockingStream(stream))
    }

    /// Switches the listener between blocking and polling accept.
    ///
    /// # Errors
    /// Returns the raw IO error.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
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

/// Longest `sun_path` for this platform, including the trailing NUL byte
/// (macOS 104, Linux 108; FR-6).
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;

/// Rejects endpoint paths that cannot fit into `sockaddr_un` with a clear
/// error instead of a raw EINVAL/ENAMETOOLONG (FR-6).
fn checked_path(endpoint: &Endpoint) -> io::Result<&Path> {
    let path = endpoint.as_path();
    let len = path.as_os_str().len();
    if len >= SUN_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path is {len} bytes; the limit is {} — shorten the cache directory (e.g. DOCSBASE_CACHE_DIR)",
                SUN_PATH_MAX - 1
            ),
        ));
    }
    Ok(path)
}

/// Binds the async listener for `endpoint`.
///
/// Must be called from within an IO-enabled Tokio runtime (production binds
/// inside `run_daemon`'s runtime).
///
/// # Errors
/// Returns [`io::ErrorKind::InvalidInput`] for over-long paths and the raw
/// bind error otherwise (caller adds path context).
pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    tokio::net::UnixListener::bind(checked_path(endpoint)?).map(Listener)
}

/// Binds the blocking listener for fakes and tests.
///
/// # Errors
/// Returns [`io::ErrorKind::InvalidInput`] for over-long paths and the raw
/// bind error otherwise.
pub fn bind_blocking(endpoint: &Endpoint) -> io::Result<BlockingListener> {
    UnixListener::bind(checked_path(endpoint)?).map(BlockingListener)
}

/// Connects a blocking client stream to `endpoint`.
///
/// # Errors
/// Returns [`io::ErrorKind::InvalidInput`] for over-long paths and the raw
/// connect error otherwise.
pub fn connect_blocking(endpoint: &Endpoint) -> io::Result<BlockingStream> {
    UnixStream::connect(checked_path(endpoint)?).map(BlockingStream)
}

/// Liveness probe: `true` when something accepts on `endpoint`.
#[must_use]
pub fn connect_probe(endpoint: &Endpoint) -> bool {
    checked_path(endpoint).and_then(UnixStream::connect).is_ok()
}

/// Endpoint of the daemon serving `cache` (design §3).
#[must_use]
pub fn daemon_endpoint(cache: &std::path::Path) -> Endpoint {
    Endpoint::Unix(cache.join("state").join("daemon.sock"))
}

/// True when the endpoint file exists.
#[must_use]
pub fn exists(endpoint: &Endpoint) -> bool {
    endpoint.as_path().exists()
}

/// Removes the endpoint file; a missing endpoint is already clean.
///
/// # Errors
/// Returns the raw IO error when the file exists but cannot be removed.
pub fn remove(endpoint: &Endpoint) -> std::io::Result<()> {
    match std::fs::remove_file(endpoint.as_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Terminates the daemon's accept loop on SIGTERM or SIGINT.
pub struct ShutdownSignal {
    sigterm: Signal,
    sigint: Signal,
}

impl ShutdownSignal {
    /// Installs both handlers (must be called inside a Tokio runtime).
    ///
    /// # Errors
    /// Returns the raw IO error when a handler cannot be installed.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            sigterm: tokio::signal::unix::signal(SignalKind::terminate())?,
            sigint: tokio::signal::unix::signal(SignalKind::interrupt())?,
        })
    }

    /// Resolves when SIGTERM or SIGINT arrives.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.sigterm.recv() => {}
            _ = self.sigint.recv() => {}
        }
    }
}

/// True while `pid` exists and is not a zombie.
///
/// Linux reads `/proc` (without `/proc` the process is assumed alive and
/// other liveness checks decide). macOS uses `proc_pidinfo` BSD info; a
/// zombie (killed but not reaped) counts as dead, matching the Linux
/// `state != "Z"` semantics (FR-2).
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_process_alive(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if !Path::new("/proc").is_dir() {
            return true;
        }
        fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z")
        })
    }
}

/// macOS liveness via `proc_pidinfo(PROC_PIDTBSDINFO)` (ADR-M1).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // libproc FFI: SAFETY comments on each block
fn macos_process_alive(pid: u32) -> bool {
    /// Darwin `SZOMB` (`sys/proc.h`); libc does not export it for Apple.
    const SZOMB: u32 = 5;
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return false;
    };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).unwrap_or(i32::MAX);
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `proc_pidinfo` writes at most `size` bytes into `info`, which is
    // exactly the struct's size; `pid`/flavor are plain values. A zero or
    // short result means the pid is gone.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return false;
    }
    // SAFETY: the call reported a full struct written into `info`.
    let info = unsafe { info.assume_init() };
    info.pbi_status != SZOMB
}

/// Open file descriptors of this process.
///
/// Linux reads `/proc/self/fd`; macOS uses `proc_pidinfo(PROC_PIDLISTFDS)`;
/// 0 elsewhere (FR-3).
#[must_use]
pub fn fd_count() -> u64 {
    #[cfg(target_os = "macos")]
    {
        macos_fd_count()
    }
    #[cfg(not(target_os = "macos"))]
    {
        fs::read_dir("/proc/self/fd").map_or(0, |entries| {
            u64::try_from(entries.count()).unwrap_or(u64::MAX)
        })
    }
}

/// macOS descriptor count via `PROC_PIDLISTFDS` (buffer doubles until it
/// fits, bounded) (ADR-M1).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // libproc FFI: SAFETY comments on each block
fn macos_fd_count() -> u64 {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    let Ok(pid) = libc::c_int::try_from(std::process::id()) else {
        return 0;
    };
    let mut capacity = 64_usize;
    loop {
        let bytes = capacity.saturating_mul(entry);
        let Ok(size) = i32::try_from(bytes) else {
            return 0;
        };
        let mut buffer = vec![0_u8; bytes];
        // SAFETY: the buffer is `bytes` long and `proc_pidinfo` writes at
        // most `size` bytes into it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
        if written <= 0 {
            return 0;
        }
        let count = usize::try_from(written).unwrap_or(0) / entry;
        if written < size || capacity >= 65_536 {
            // Fits, or the bounded cap is reached (count may under-report).
            return u64::try_from(count).unwrap_or(u64::MAX);
        }
        capacity = capacity.saturating_mul(2);
    }
}

/// OS threads of this process (Linux `/proc/self/task`; 0 elsewhere — macOS
/// degrades like Windows, `task_threads` is out of scope) (FR-3).
#[must_use]
pub fn thread_count() -> u64 {
    #[cfg(target_os = "macos")]
    {
        0
    }
    #[cfg(not(target_os = "macos"))]
    {
        fs::read_dir("/proc/self/task").map_or(0, |entries| {
            u64::try_from(entries.count()).unwrap_or(u64::MAX)
        })
    }
}

/// Puts the child in its own process group so signals to the daemon's group
/// do not hit unrelated processes (FR-2).
pub fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Creates `dir` (and parents) with owner-only `0700`, idempotently.
///
/// # Errors
/// Returns the raw IO error when creation or chmod fails.
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        // Create with the final mode so there is no world-readable window
        // (umask may still mask bits; the chmod below corrects it).
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir)?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// Restricts an existing path to `0600` (sockets, manifests, logs).
///
/// # Errors
/// Returns the raw IO error when chmod fails.
pub fn secure_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Marks an existing path executable as `0755` (installed binary).
///
/// # Errors
/// Returns the raw IO error when chmod fails.
pub fn secure_executable(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// Opens (creating if needed) a `0600` append-only log file; pre-existing
/// files are tightened too (`mode` only applies at creation).
///
/// # Errors
/// Returns the raw IO error when the file cannot be opened or chmod'ed.
pub fn open_private_log(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}
