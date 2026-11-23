//! Explicit capability over the application-owned native arena.
use super::connection::{Connection, Listening, Pipe};
use super::native_io::Service;
use super::root::Options;
use crate::platform::Endpoint;
use std::future::{Future, poll_fn};
use std::io::{self, Read, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf, ReadHalf, WriteHalf};
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

#[allow(unsafe_code)] // Borrowed name and finite caller-owned wait; no handle or worker.
pub(super) fn wait_named_pipe(endpoint: &Endpoint, remaining: Duration) -> io::Result<()> {
    if remaining.is_zero() {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    let path = super::pipe_path(endpoint)?;
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // Round to Win32 milliseconds without selecting either special value:
    // zero asks for the server default, DWORD::MAX waits forever.
    let millis = remaining
        .as_nanos()
        .div_ceil(1_000_000)
        .clamp(1, u128::from(u32::MAX - 1));
    let millis = u32::try_from(millis).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: the validated NUL-terminated name stays live for this synchronous
    // call. The timeout is finite; IOCP progress stays on its independent owner.
    if unsafe { WaitNamedPipeW(wide.as_ptr(), millis) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_SEM_TIMEOUT.cast_signed()) {
        Err(io::Error::new(io::ErrorKind::TimedOut, error))
    } else {
        Err(error)
    }
}

pub(in crate::platform) fn run<T: Send>(
    options: &Options,
    work: impl FnOnce(Transport) -> io::Result<T> + Send,
) -> io::Result<T> {
    super::root::run(options, |service| {
        work(Transport::new(service, options.clone()))
    })
}

/// Submission capability. Its enclosing synchronous root owns physical retirement.
#[derive(Clone)]
pub struct Transport {
    service: Service,
    options: Options,
}
impl Transport {
    #[cfg(test)]
    pub(crate) fn test_quiesced(&self) -> bool {
        self.service.test_quiesced()
    }

    pub(super) fn new(service: Service, options: Options) -> Self {
        Self { service, options }
    }
    /// Bind an endpoint inside this scope.
    /// # Errors
    /// Invalid endpoint, exhausted physical capacity, or native bind failure.
    pub fn bind(&self, endpoint: &Endpoint) -> io::Result<Listener> {
        let path = super::pipe_path(endpoint)?;
        let pending = Listening::new_in(&self.service, &path, self.options.chunk_bytes, true)?;
        Ok(Listener {
            transport: self.clone(),
            path,
            pending: Mutex::new(Some(pending)),
            accepting: tokio::sync::Mutex::new(()),
        })
    }
    /// Bind a blocking listener in the same physical arena.
    /// # Errors
    /// Returns the native bind error.
    pub fn bind_blocking(&self, endpoint: &Endpoint) -> io::Result<BlockingListener> {
        Ok(BlockingListener {
            listener: self.bind(endpoint)?,
            nonblocking: AtomicBool::new(false),
        })
    }
    fn connection_raw(&self, endpoint: &Endpoint) -> io::Result<Arc<Connection>> {
        Connection::client_in(
            &self.service,
            &super::pipe_path(endpoint)?,
            self.options.chunk_bytes,
            self.options.helper_stack_bytes,
        )
    }
    fn connection(&self, endpoint: &Endpoint) -> io::Result<Arc<Connection>> {
        self.connection_raw(endpoint).map_err(Self::connect_error)
    }
    fn connect_error(error: io::Error) -> io::Error {
        if error.raw_os_error() == Some(ERROR_PIPE_BUSY.cast_signed()) {
            io::Error::new(io::ErrorKind::TimedOut, error)
        } else {
            error
        }
    }
    /// One nonblocking native connection attempt; no helper or retry.
    /// # Errors
    /// Returns the native error, or `TimedOut` when the endpoint is busy.
    pub fn connect_async(
        &self,
        endpoint: &Endpoint,
    ) -> impl Future<Output = io::Result<Stream>> + Send {
        poll_fn(move |_| Poll::Ready(Stream::new(self.connection(endpoint)?)))
    }
    /// Wait for a local physical slot under the caller's deadline, then make one native open.
    pub(crate) async fn connect_async_with_capacity(
        &self,
        endpoint: &Endpoint,
    ) -> io::Result<Stream> {
        let path = super::pipe_path(endpoint)?;
        let reservation = loop {
            let changed = self.service.capacity_changed();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.service.reserve() {
                Ok(reservation) => break reservation,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => changed.await,
                Err(error) => return Err(error),
            }
        };
        let connection = Connection::client_reserved(
            &self.service,
            reservation,
            &path,
            self.options.chunk_bytes,
            self.options.helper_stack_bytes,
        )
        .map_err(Self::connect_error)?;
        Stream::new(connection)
    }
    /// One native connection attempt, using the same owned I/O as async clients.
    /// # Errors
    /// Returns the native connection error.
    pub fn connect_blocking(&self, endpoint: &Endpoint) -> io::Result<BlockingStream> {
        BlockingStream::new(self.connection(endpoint)?)
    }

    /// Wait for a busy Windows pipe on this caller's thread, using one deadline.
    /// No worker is created and no request is replayed after connection.
    /// # Errors
    /// Deadline expiry, exhausted local capacity or a non-busy native error.
    pub fn connect_blocking_until(
        &self,
        endpoint: &Endpoint,
        deadline: Instant,
    ) -> io::Result<BlockingStream> {
        BlockingStream::new(self.connection_until(endpoint, deadline, |remaining| {
            wait_named_pipe(endpoint, remaining)
        })?)
    }

    pub(super) fn connection_until(
        &self,
        endpoint: &Endpoint,
        deadline: Instant,
        mut wait: impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<Arc<Connection>> {
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            match self.connection_raw(endpoint) {
                Ok(connection) => {
                    if Instant::now() >= deadline {
                        connection.abort();
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    return Ok(connection);
                }
                Err(error) if error.raw_os_error() == Some(231) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    match wait(remaining) {
                        Ok(()) => {}
                        // A finite DWORD wait can end before a very long
                        // caller deadline; its original Instant still governs.
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Serialized accept preserves the pending physical instance across cancellation.
pub struct Listener {
    transport: Transport,
    path: String,
    pending: Mutex<Option<Arc<Listening>>>,
    accepting: tokio::sync::Mutex<()>,
}
impl Listener {
    /// Accept without releasing the arena charge of the listening instance.
    /// # Errors
    /// Returns native accept errors or closure of the enclosing root.
    pub async fn accept(&self) -> io::Result<Stream> {
        let _accept = self.accepting.lock().await;
        loop {
            let pending = loop {
                let changed = self.transport.service.capacity_changed();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if let Some(pending) = self
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                {
                    break pending;
                }
                match Listening::new_in(
                    &self.transport.service,
                    &self.path,
                    self.transport.options.chunk_bytes,
                    false,
                ) {
                    Ok(pending) => {
                        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(pending.clone());
                        break pending;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => changed.await,
                    Err(error) => return Err(error),
                }
            };
            let token = match poll_fn(|cx| pending.poll_accept(cx)).await {
                Ok(token) => token,
                Err(error) if error.raw_os_error() == Some(232) => {
                    // The peer connected and closed before ConnectNamedPipe. This
                    // instance is not reusable; retire its original physical owner.
                    // Reserve/rearm before closing the last named instance when
                    // capacity permits. Otherwise even a healthy new client can
                    // observe FILE_NOT_FOUND during our cooperative yield.
                    let replacement = match Listening::new_in(
                        &self.transport.service,
                        &self.path,
                        self.transport.options.chunk_bytes,
                        false,
                    ) {
                        Ok(replacement) => Some(replacement),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                        Err(error) => {
                            self.pending
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take();
                            pending.abort();
                            return Err(error);
                        }
                    };
                    *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = replacement;
                    pending.abort();
                    drop(pending);
                    // Connection churn must not monopolize the executor or prevent
                    // cancellation while we prepare the next bounded instance.
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            // No await after transfer: the same token becomes the admitted owner
            // before a cancelled accept future could abandon the transition.
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            return Stream::new(Connection::accepted(
                token,
                self.transport.options.chunk_bytes,
                self.transport.options.helper_stack_bytes,
            )?);
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(pending) = self
            .pending
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            pending.abort();
        }
    }
}

/// Native asynchronous byte stream with one serialized logical writer.
pub struct Stream {
    pipe: Pipe,
    connection: Arc<Connection>,
}
impl Stream {
    fn new(connection: Arc<Connection>) -> io::Result<Self> {
        Ok(Self {
            pipe: Pipe::new(connection.clone())?,
            connection,
        })
    }
    pub(crate) fn ownership(&self) -> Ownership {
        Ownership(self.connection.clone())
    }
}
/// Native-only tail handle; retaining this does not keep a logical stream open.
#[derive(Clone)]
pub(crate) struct Ownership(Arc<Connection>);
impl Ownership {
    #[cfg(test)]
    pub(crate) fn test_hold_write_cancellation(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        self.0.test_hold_write_cancellation(&entered, &release);
        drop((entered, release));
    }
    pub(crate) fn retain_connection(&self, charge: Arc<dyn Send + Sync>) -> io::Result<()> {
        self.0.retain(false, charge)
    }
    pub(crate) fn retain_request(&self, charge: Arc<dyn Send + Sync>) -> io::Result<()> {
        self.0.retain(true, charge)
    }
    pub(crate) fn request_completed(&self) {
        self.0.request_completed();
    }
    pub(crate) fn abort(&self) {
        self.0.abort();
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().pipe).poll_read(cx, buffer)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().pipe).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().pipe).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().pipe).poll_shutdown(cx)
    }
}

#[derive(Default)]
struct WakeState {
    signalled: Mutex<bool>,
    changed: Condvar,
}
impl Wake for WakeState {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        *self
            .signalled
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_one();
    }
}
/// Poll owned operations without a Tokio runtime or an unbounded helper.
fn blocking<T>(
    future: impl Future<Output = io::Result<T>>,
    timeout: Option<Duration>,
    nonblocking: bool,
) -> io::Result<T> {
    let deadline = timeout
        .map(|duration| {
            Instant::now()
                .checked_add(duration)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
        })
        .transpose()?;
    let signal = Arc::new(WakeState::default());
    let waker = Waker::from(signal.clone());
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        *signal
            .signalled
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
        if nonblocking {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let mut signalled = signal
            .signalled
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !*signalled {
            signalled = match deadline {
                None => signal
                    .changed
                    .wait(signalled)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
                    let (guard, _) = signal
                        .changed
                        .wait_timeout(signalled, remaining)
                        .unwrap_or_else(PoisonError::into_inner);
                    guard
                }
            };
        }
    }
}
struct BlockingState {
    read: tokio::sync::Mutex<ReadHalf<Stream>>,
    write: tokio::sync::Mutex<WriteHalf<Stream>>,
    ownership: Ownership,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
}
/// Clones share one serialized writer and independent read/write directions.
pub struct BlockingStream {
    state: Arc<BlockingState>,
}
impl BlockingStream {
    fn new(connection: Arc<Connection>) -> io::Result<Self> {
        let stream = Stream::new(connection)?;
        let ownership = stream.ownership();
        let (read, write) = tokio::io::split(stream);
        Ok(Self {
            state: Arc::new(BlockingState {
                read: tokio::sync::Mutex::new(read),
                write: tokio::sync::Mutex::new(write),
                ownership,
                read_timeout: Mutex::new(None),
                write_timeout: Mutex::new(None),
            }),
        })
    }
    #[cfg(test)]
    pub(super) fn test_read_active(&self) -> bool {
        self.state.ownership.0.test_read_active()
    }
    /// Set a real native-read deadline; zero is invalid.
    /// # Errors
    /// Zero timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        Self::timeout(&self.state.read_timeout, timeout)
    }
    /// Set a real native-write deadline; zero is invalid.
    /// # Errors
    /// Zero timeout.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        Self::timeout(&self.state.write_timeout, timeout)
    }
    fn timeout(slot: &Mutex<Option<Duration>>, timeout: Option<Duration>) -> io::Result<()> {
        if timeout.is_some_and(|duration| duration.is_zero()) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = timeout;
        Ok(())
    }
    /// Share the same serialized stream for a reader/writer split.
    /// # Errors
    /// Retains the Unix-compatible fallible signature; does not duplicate native handles.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            state: self.state.clone(),
        })
    }
    fn result<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            self.state.ownership.abort();
        }
        result
    }
    /// Explicit successful final close; waits for the peer to read server bytes.
    /// # Errors
    /// Native failure or the configured write timeout. Failure aborts the exchange.
    pub fn shutdown(&mut self) -> io::Result<()> {
        let timeout = *self
            .state
            .write_timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.result(blocking(
            async { self.state.write.lock().await.shutdown().await },
            timeout,
            false,
        ))
    }
}
impl Read for BlockingStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let timeout = *self
            .state
            .read_timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.result(blocking(
            async {
                tokio::io::AsyncReadExt::read(&mut *self.state.read.lock().await, bytes).await
            },
            timeout,
            false,
        ))
    }
}
impl Write for BlockingStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let timeout = *self
            .state
            .write_timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.result(blocking(
            async { self.state.write.lock().await.write(bytes).await },
            timeout,
            false,
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        let timeout = *self
            .state
            .write_timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.result(blocking(
            async { self.state.write.lock().await.flush().await },
            timeout,
            false,
        ))
    }
}
/// Blocking accept is an adapter over the same cancellation-safe listener.
pub struct BlockingListener {
    listener: Listener,
    nonblocking: AtomicBool,
}
impl BlockingListener {
    /// Accept a stream without allocating a helper.
    /// # Errors
    /// Native failure, or `WouldBlock` in polling mode.
    pub fn accept(&self) -> io::Result<BlockingStream> {
        let stream = blocking(
            self.listener.accept(),
            None,
            self.nonblocking.load(Ordering::Acquire),
        )?;
        let ownership = stream.ownership();
        let (read, write) = tokio::io::split(stream);
        Ok(BlockingStream {
            state: Arc::new(BlockingState {
                read: tokio::sync::Mutex::new(read),
                write: tokio::sync::Mutex::new(write),
                ownership,
                read_timeout: Mutex::new(None),
                write_timeout: Mutex::new(None),
            }),
        })
    }
    /// Switch accept between blocking and polling.
    /// # Errors
    /// Retains the Unix-compatible fallible signature.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }
    /// Iterate accepted streams until the first error.
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming {
            listener: self,
            done: false,
        }
    }
}
/// Incoming blocking streams.
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
        let result = self.listener.accept();
        self.done = result.is_err();
        Some(result)
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn wake_after_blocking_deadline_cannot_repoll_operation() {
        let timeout = Duration::from_millis(100);
        let mut polls = 0;
        let result = blocking(
            poll_fn(|cx| {
                polls += 1;
                if polls == 1 {
                    let late = Instant::now() + timeout;
                    while Instant::now() <= late {
                        std::thread::park_timeout(late.saturating_duration_since(Instant::now()));
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            }),
            Some(timeout),
            false,
        );
        assert!(
            matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut),
            "wake after the absolute deadline cannot admit another poll"
        );
        assert!(polls <= 1);
    }
}
