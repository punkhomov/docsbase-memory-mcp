//! Unix implementation of the platform transport seam (ADR-9, v1).
//!
//! Everything here is a thin delegation to `std`/`tokio` Unix primitives;
//! behavior (framing, timeouts, backlog) is unchanged from the pre-seam code.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

/// Binds the async listener for `endpoint`.
///
/// Must be called from within an IO-enabled Tokio runtime (production binds
/// inside `run_daemon`'s runtime).
///
/// # Errors
/// Returns the raw bind error (caller adds path context).
pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    tokio::net::UnixListener::bind(endpoint.as_path()).map(Listener)
}

/// Binds the blocking listener for fakes and tests.
///
/// # Errors
/// Returns the raw bind error.
pub fn bind_blocking(endpoint: &Endpoint) -> io::Result<BlockingListener> {
    UnixListener::bind(endpoint.as_path()).map(BlockingListener)
}

/// Connects a blocking client stream to `endpoint`.
///
/// # Errors
/// Returns the raw connect error.
pub fn connect_blocking(endpoint: &Endpoint) -> io::Result<BlockingStream> {
    UnixStream::connect(endpoint.as_path()).map(BlockingStream)
}

/// Liveness probe: `true` when something accepts on `endpoint`.
#[must_use]
pub fn connect_probe(endpoint: &Endpoint) -> bool {
    UnixStream::connect(endpoint.as_path()).is_ok()
}
