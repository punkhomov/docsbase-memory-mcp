//! Explicit application scope for native IPC resources, independent of runtimes.
use std::io;
use std::time::Duration;

/// Complete policy resolved at the application composition boundary.
/// Resource sizes and polling policy never acquire a fallback in consumers.
#[derive(Clone)]
pub struct Options {
    /// Maximum simultaneous physical handles, including pending retirement.
    pub handles: usize,
    /// Bytes in each of the two native I/O regions per connection.
    pub chunk_bytes: usize,
    /// Reserved stack bytes for the completion thread and each optional drain worker.
    pub helper_stack_bytes: usize,
    /// Stack bytes for the single scoped application thread.
    pub work_stack_bytes: usize,
    /// Outer physical-retirement polling interval.
    pub drain_poll: Duration,
    /// Time until unresolved retirement is reported; never a detach deadline.
    pub drain_observation: Duration,
}
impl Options {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.handles == 0
            || self.chunk_bytes == 0
            || u32::try_from(self.chunk_bytes).is_err()
            || self.helper_stack_bytes == 0
            || self.work_stack_bytes == 0
            || self.drain_poll.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid complete native policy",
            ));
        }
        self.chunk_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(self.helper_stack_bytes))
            .and_then(|bytes| bytes.checked_mul(self.handles))
            .and_then(|bytes| bytes.checked_add(self.helper_stack_bytes))
            .and_then(|bytes| bytes.checked_add(self.work_stack_bytes))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "native capacity overflow")
            })?;
        Ok(())
    }
}

#[cfg(unix)]
pub use super::unix::{BlockingListener, BlockingStream, Listener, Stream};
#[cfg(windows)]
pub use super::windows::transport::{
    BlockingListener, BlockingStream, Listener, Stream, Transport,
};

/// Thin Unix capability: the OS owns synchronous socket retirement.
#[cfg(unix)]
#[derive(Clone)]
pub struct Transport {
    _private: (),
}
#[cfg(unix)]
impl Transport {
    /// Bind an asynchronous endpoint.
    /// # Errors
    /// Returns the raw bind error.
    pub fn bind(&self, endpoint: &super::Endpoint) -> io::Result<Listener> {
        super::unix::bind(endpoint)
    }
    /// Bind a blocking endpoint.
    /// # Errors
    /// Returns the raw bind error.
    pub fn bind_blocking(&self, endpoint: &super::Endpoint) -> io::Result<BlockingListener> {
        super::unix::bind_blocking(endpoint)
    }
    /// Make one asynchronous connection attempt.
    /// # Errors
    /// Returns the raw connection error.
    pub async fn connect_async(&self, endpoint: &super::Endpoint) -> io::Result<Stream> {
        super::unix::connect_async(endpoint).await
    }
    /// Make one blocking connection attempt.
    /// # Errors
    /// Returns the raw connection error.
    pub fn connect_blocking(&self, endpoint: &super::Endpoint) -> io::Result<BlockingStream> {
        super::unix::connect_blocking(endpoint)
    }
}

/// Run one application/command scope and retire physical resources before return.
/// Error/unwind requests cancellation while the native completion owner stays live.
/// # Errors
/// Invalid policy, native root construction failure, or the application's error.
pub fn run<T: Send>(
    options: &Options,
    work: impl FnOnce(Transport) -> io::Result<T> + Send,
) -> io::Result<T> {
    options.validate()?;
    #[cfg(windows)]
    {
        super::windows::transport::run(options, work)
    }
    #[cfg(unix)]
    {
        work(Transport { _private: () })
    }
}
