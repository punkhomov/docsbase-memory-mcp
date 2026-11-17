//! Outer synchronous owner: a cancelled work future never owns native retirement.
use super::native_io::{Port, Service};
#[cfg(test)]
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;
#[cfg(test)]
use tokio::runtime::Runtime;
use tokio::runtime::{Handle, RuntimeFlavor};

pub(super) use crate::platform::transport::Options;

/// The caller retains `Runtime`, `JobRoot`, signal receiver and admission lease.
/// Only this outer function owns the progress `JoinHandle`. Its native owners
/// remain registered during work errors, cancellation, and unwinding.
#[cfg(test)]
pub(super) fn run_on<T: Send, F>(
    runtime: &Runtime,
    options: &Options,
    work: impl FnOnce(Service) -> F + Send,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    run(options, move |service| runtime.block_on(work(service)))
}

/// One outer scope for synchronous or asynchronous application work. Native
/// retirement progresses even when that work cannot cooperatively yield.
pub(super) fn run<T: Send>(
    options: &Options,
    work: impl FnOnce(Service) -> io::Result<T> + Send,
) -> io::Result<T> {
    let run = || run_outside(options, work);
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

fn run_outside<T: Send>(
    options: &Options,
    work: impl FnOnce(Service) -> io::Result<T> + Send,
) -> io::Result<T> {
    options.validate()?;
    let mut port = Port::new(options.handles, options.helper_stack_bytes)?;
    let service = port.service();
    // Only application work moves to this scoped thread. Port, the progress
    // JoinHandle and physical owners stay on this synchronous outer thread.
    // FnOnce includes future/runtime construction, not just later polling.
    let result = catch_unwind(AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            // If outer housekeeping unwinds, request cancellation BEFORE
            // thread::scope waits for application work to return.
            struct AbortOnReturn<'a>(&'a Service);
            impl Drop for AbortOnReturn<'_> {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }
            let _abort = AbortOnReturn(&service);
            let submission = service.clone();
            let application = std::thread::Builder::new()
                .name("docsbase-transport-work".into())
                .stack_size(options.work_stack_bytes)
                .spawn_scoped(scope, move || work(submission))?;
            while !application.is_finished() {
                if service.reap().is_err() {
                    // Fault means closed admission/quarantine, never detaching
                    // physical owners or abandoning the application thread.
                    service.abort();
                }
                std::thread::sleep(options.drain_poll);
            }
            match application.join() {
                Ok(result) => result,
                Err(panic) => resume_unwind(panic),
            }
        })
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
        // This owner does not require a live/polling application runtime.
        // Observation is never a native timeout or permission to detach.
        std::thread::sleep(options.drain_poll);
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
    fn synchronous_work_does_not_block_outer_native_retirement() {
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
            work_stack_bytes: 2 * 1024 * 1024,
            drain_poll: Duration::from_millis(1),
            drain_observation: Duration::from_secs(30),
        };
        run_on(&runtime, &options, |service| async move {
            let listening = Listening::new_in(&service, &path, 4096, true)?;
            let client = Connection::client_in(&service, &path, 65_536, 1024 * 1024)?;
            let token = std::future::poll_fn(|cx| listening.poll_accept(cx)).await?;
            let server = Connection::accepted(token, 65_536, 1024 * 1024)?;
            let weak = Arc::downgrade(&server);
            let mut writer = Pipe::new(server.clone())?;
            writer.write_all(&vec![b'x'; 65_536]).await?;
            drop(writer);
            drop(server);
            drop(listening);
            client.abort();
            drop(client);
            // Synchronous clients cannot yield the enclosing async future to
            // drive retirement. This is only a liveness watchdog: actual owner
            // destruction, not elapsed time or cancel return, is the evidence.
            let watchdog = Instant::now() + Duration::from_secs(30);
            while weak.upgrade().is_some() && Instant::now() < watchdog {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                weak.upgrade().is_none(),
                "physical owner must retire while synchronous work is running"
            );
            Ok(())
        })
        .unwrap();
    }

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
                work_stack_bytes: 2 * 1024 * 1024,
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
                    assert!(!unwind, "controlled work unwind after native submission");
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
