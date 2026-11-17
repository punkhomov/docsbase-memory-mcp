//! Outer synchronous owner: a cancelled work future never owns native retirement.
use super::native_io::{Port, Service};
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::{Duration, Instant};
use tokio::runtime::{Handle, Runtime, RuntimeFlavor};

/// Complete resolved composition. No native consumer supplies fallback policy.
pub(super) struct Options {
    pub handles: usize,
    pub chunk_bytes: usize,
    pub helper_stack_bytes: usize,
    pub drain_poll: Duration,
    pub drain_observation: Duration,
}
impl Options {
    fn validate(&self) -> io::Result<()> {
        if self.handles == 0
            || self.chunk_bytes == 0
            || u32::try_from(self.chunk_bytes).is_err()
            || self.helper_stack_bytes == 0
            || self.drain_poll.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid complete native policy",
            ));
        }
        // Reserve the entire retained-native/helper envelope before any HANDLE
        // or helper creation. Frame/staging owners add their separate checked
        // reservations at the application composition boundary.
        self.chunk_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(self.helper_stack_bytes))
            .and_then(|bytes| bytes.checked_mul(self.handles))
            .and_then(|bytes| bytes.checked_add(self.helper_stack_bytes))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "native capacity overflow")
            })?;
        Ok(())
    }
}

/// The caller retains Runtime, JobRoot, signal receiver and admission lease.
/// Only this outer function owns the progress JoinHandle. Its native owners
/// remain registered during work errors, cancellation, and unwinding.
pub(super) fn run_on<T, F>(
    runtime: &Runtime,
    options: &Options,
    work: impl FnOnce(Service) -> F,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    let run = || run_outside(runtime, options, work);
    match Handle::try_current() {
        Err(_) => run(),
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(run)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "native synchronous owner cannot nest in a current-thread runtime",
        )),
    }
}

fn run_outside<T, F>(
    runtime: &Runtime,
    options: &Options,
    work: impl FnOnce(Service) -> F,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    options.validate()?;
    let mut port = Port::new(options.handles, options.helper_stack_bytes)?;
    let service = port.service();
    // Future construction is also inside the unwind boundary, while the root
    // and its thread/helper ownership remain outside it.
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut future = Box::pin(work(service.clone()));
        loop {
            service.reap()?;
            let completed = runtime.block_on(async {
                tokio::select! {
                    biased;
                    result = &mut future => Some(result),
                    () = tokio::time::sleep(options.drain_poll) => None,
                }
            });
            if let Some(result) = completed {
                return result;
            }
        }
    }));
    // The work future is gone, but its physical envelopes stay in the arena.
    // A successful work return means no healthy exchange remains admitted;
    // daemon quiescence keeps serving admitted frames inside that work scope.
    service.abort();
    let began = Instant::now();
    let mut reported = false;
    loop {
        let status = match service.reap() {
            Ok(0) => match port.finish() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                Err(error) => Some(error.to_string()),
            },
            Ok(retained) => Some(format!("{retained} physical envelopes retained")),
            Err(error) => Some(error.to_string()),
        };
        if !reported && began.elapsed() >= options.drain_observation {
            eprintln!(
                "native transport still draining/quarantined: {}",
                status
                    .as_deref()
                    .unwrap_or("progress thread is not yet terminal")
            );
            reported = true;
        }
        // Existing owner poll policy also rediscovers permanent state if a
        // runtime wake failed. This is observation, never a native completion
        // timeout or permission to detach/close unresolved native storage.
        runtime.block_on(async { tokio::time::sleep(options.drain_poll).await });
    }
    match result {
        Ok(result) => result,
        Err(panic) => resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::connection::{Connection, Listening, Pipe};
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn outer_scope_retires_dirty_registered_endpoints_before_error_or_unwind_returns() {
        for unwind in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let endpoint = crate::platform::daemon_endpoint(directory.path());
            let path = crate::platform::windows::pipe_path(&endpoint).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let options = Options {
                handles: 2,
                chunk_bytes: 65_536,
                helper_stack_bytes: 1024 * 1024,
                drain_poll: Duration::from_millis(1),
                drain_observation: Duration::from_secs(30),
            };
            let captured = Arc::new(Mutex::new(None));
            let observation = captured.clone();
            let chunk_bytes = options.chunk_bytes;
            let stack_bytes = options.helper_stack_bytes;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                run_on::<(), _>(&runtime, &options, move |service| async move {
                    let listening = Listening::new_in(&service, &path, 4096, true)?;
                    let client = Connection::client_in(&service, &path, chunk_bytes, stack_bytes)?;
                    let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
                    let server = Connection::accepted(token, chunk_bytes, stack_bytes)?;
                    *observation.lock().unwrap() = Some((Arc::downgrade(&server), service));
                    let mut pipe = Pipe::new(server)?;
                    pipe.write_all(&vec![b'x'; chunk_bytes]).await?;
                    drop(client);
                    if unwind {
                        panic!("controlled work unwind after native submission");
                    }
                    Err(io::Error::other(
                        "controlled work failure after native submission",
                    ))
                })
            }));
            if unwind {
                assert!(outcome.is_err());
            } else {
                assert_eq!(
                    outcome.unwrap().unwrap_err().to_string(),
                    "controlled work failure after native submission"
                );
            }
            let state = captured.lock().unwrap();
            let (owner, escaped_service) = state.as_ref().unwrap();
            assert!(
                owner.upgrade().is_none(),
                "physical envelope is retired before return"
            );
            assert!(
                escaped_service.reserve().is_err(),
                "escaped capability is permanently closed"
            );
        }
    }
}
