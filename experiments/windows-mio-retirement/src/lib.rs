//! Throwaway feasibility probe; no application/backend implementation.

#[cfg(all(test, windows))]
mod tests {
    use std::io;
    use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer};
    use tokio::runtime::{Builder, Runtime};
    use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    const WATCHDOG: Duration = Duration::from_secs(15);
    const LARGE: usize = 1024 * 1024;
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default)]
    struct Retirement {
        live: AtomicUsize,
        changed: Notify,
    }

    impl Retirement {
        async fn settled(&self) {
            tokio::time::timeout(WATCHDOG, async {
                while self.live.load(Ordering::SeqCst) != 0 {
                    self.changed.notified().await;
                }
            })
            .await
            .expect("actual Mio owner must retire while its runtime is alive");
        }
    }

    struct Charge {
        permit: Option<OwnedSemaphorePermit>,
        state: Arc<Retirement>,
    }

    impl Charge {
        fn reserve(budget: &Arc<Semaphore>, state: &Arc<Retirement>) -> Self {
            let permit = budget.clone().try_acquire_owned().expect("admission");
            state.live.fetch_add(1, Ordering::SeqCst);
            Self {
                permit: Some(permit),
                state: state.clone(),
            }
        }
    }

    impl Drop for Charge {
        fn drop(&mut self) {
            drop(self.permit.take());
            self.state.live.fetch_sub(1, Ordering::SeqCst);
            self.state.changed.notify_one();
        }
    }

    struct Server {
        pipe: NamedPipeServer,
        // Sensitivity mutation: retain only through logical stream destruction.
        _logical: Option<Charge>,
    }

    fn runtime() -> Runtime {
        Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn raw_server(name: &str) -> io::Result<OwnedHandle> {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: the unique NUL-terminated name and optional security pointer
        // stay valid for this call. A valid returned HANDLE is transferred once.
        let raw = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65_536,
                65_536,
                0,
                std::ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this call created the sole ownership of the valid HANDLE.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }

    async fn pair(
        budget: &Arc<Semaphore>,
        state: &Arc<Retirement>,
        logical: bool,
    ) -> (Server, NamedPipeClient) {
        let name = format!(
            r"\\.\pipe\docsbase-mio-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        // Reserve before HANDLE creation and before Mio registration/first I/O.
        let charge = Charge::reserve(budget, state);
        let handle = raw_server(&name).unwrap();
        let (pipe, logical) = if logical {
            // SAFETY: ownership moves exactly once; the control intentionally
            // keeps accounting outside Mio's native operation owner.
            let pipe = unsafe { NamedPipeServer::from_raw_handle(handle.into_raw_handle()) };
            (pipe.unwrap(), Some(charge))
        } else {
            // SAFETY: ownership and guard move together before registration.
            let pipe = unsafe {
                NamedPipeServer::from_raw_handle_with_retirement_guard(
                    handle.into_raw_handle(),
                    Box::new(charge),
                )
            };
            (pipe.unwrap(), None)
        };
        let client = ClientOptions::new().open(&name).unwrap();
        tokio::time::timeout(WATCHDOG, pipe.connect())
            .await
            .unwrap()
            .unwrap();
        (
            Server {
                pipe,
                _logical: logical,
            },
            client,
        )
    }

    fn pending_flush(pipe: &mut NamedPipeServer) -> bool {
        let mut context = Context::from_waker(Waker::noop());
        matches!(Pin::new(pipe).poll_flush(&mut context), Poll::Pending)
    }

    async fn read_exact(client: &mut NamedPipeClient, expected: &[u8]) {
        let mut received = vec![0; expected.len()];
        tokio::time::timeout(WATCHDOG, client.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, expected, "complete unchanged response bytes");
    }

    #[test]
    fn original_admission_survives_logical_drop_until_real_mio_retirement() {
        runtime().block_on(async {
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let logical = std::env::var_os("PROBE_LOGICAL_GUARD").is_some();
            let (mut server, mut client) = pair(&budget, &state, logical).await;
            let frame = vec![b'x'; LARGE];
            server.pipe.write_all(&frame).await.unwrap();
            let pending = pending_flush(&mut server.pipe);
            drop(server);
            // No await: the current-thread IOCP driver cannot dispatch between
            // the pending snapshot, logical destruction and accounting snapshot.
            let held = state.live.load(Ordering::SeqCst);
            let available = budget.available_permits();
            read_exact(&mut client, &frame).await;
            drop(client);
            state.settled().await;
            println!(
                "logical={logical} pending={pending} held={held} available={available} cleanup_live={} cleanup_slots={}",
                state.live.load(Ordering::SeqCst),
                budget.available_permits()
            );
            // Cleanup finishes before the expected RED assertion.
            assert!(pending, "large write is not natively settled at the snapshot");
            assert_eq!(held, 1, "original charge must follow physical ownership");
            assert_eq!(available, 0, "retiring write still occupies the original slot");
            assert_eq!(budget.available_permits(), 1);
        });
    }

    #[test]
    fn flush_waits_for_native_writes_on_a_persistent_connection() {
        runtime().block_on(async {
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let (mut server, mut client) = pair(&budget, &state, false).await;
            let mut pending = Vec::new();
            for byte in b'a'..=b'h' {
                let frame = vec![byte; LARGE];
                server.pipe.write_all(&frame).await.unwrap();
                pending.push(pending_flush(&mut server.pipe));
                let (flushed, ()) = tokio::join!(
                    tokio::time::timeout(WATCHDOG, server.pipe.flush()),
                    read_exact(&mut client, &frame)
                );
                flushed.unwrap().unwrap();
            }
            drop((server, client));
            state.settled().await;
            assert!(pending.into_iter().all(|value| value));
            assert_eq!(budget.available_permits(), 1);
            println!("persistent_frames=8 frame_bytes={LARGE} drain_helpers=0 retired=true");
        });
    }

    #[test]
    fn disconnect_retires_failed_writes_and_reuses_one_physical_slot() {
        runtime().block_on(async {
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let frame = vec![b'x'; LARGE];
            let mut held_snapshots = Vec::new();
            for _ in 0..16 {
                let (mut server, client) = pair(&budget, &state, false).await;
                server.pipe.write_all(&frame).await.unwrap();
                let pending = pending_flush(&mut server.pipe);
                server.pipe.disconnect().unwrap();
                drop(server);
                held_snapshots.push((pending, budget.available_permits()));
                drop(client);
                state.settled().await;
                assert_eq!(budget.available_permits(), 1);
            }
            assert!(
                held_snapshots
                    .into_iter()
                    .all(|(pending, slots)| pending && slots == 0)
            );
            println!("failed_exchange_cycles=16 cap=1 peak_live=1 final_slots=1");
        });
    }

    #[test]
    fn admitted_control_progresses_while_a_dirty_write_retains_admission() {
        runtime().block_on(async {
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let control_budget = Arc::new(Semaphore::new(1));
            let control_state = Arc::new(Retirement::default());
            let (mut dirty, mut unread) = pair(&budget, &state, false).await;
            let frame = vec![b'x'; LARGE];
            dirty.pipe.write_all(&frame).await.unwrap();
            let pending = pending_flush(&mut dirty.pipe);
            drop(dirty);
            let refused = budget.clone().try_acquire_owned().is_err();
            let (mut control, mut peer) = pair(&control_budget, &control_state, false).await;
            control.pipe.write_all(b"control\n").await.unwrap();
            let (flushed, ()) = tokio::join!(
                tokio::time::timeout(WATCHDOG, control.pipe.flush()),
                read_exact(&mut peer, b"control\n")
            );
            flushed.unwrap().unwrap();
            let held_after_control = state.live.load(Ordering::SeqCst);
            drop((control, peer));
            control_state.settled().await;
            read_exact(&mut unread, &frame).await;
            drop(unread);
            state.settled().await;
            assert!(pending && refused);
            assert_eq!(held_after_control, 1);
            assert_eq!(budget.available_permits(), 1);
            println!(
                "dirty_charge=1 admission_refused=true complete_control_reply=true cleanup_slots=1"
            );
        });
    }

    #[test]
    fn outer_runtime_retires_after_application_error_or_unwind() {
        for unwind in [false, true] {
            let runtime = runtime();
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let frame = vec![b'x'; LARGE];
            let (mut server, mut client) = runtime.block_on(pair(&budget, &state, false));
            let result = catch_unwind(AssertUnwindSafe(|| {
                runtime.block_on(async {
                    server.pipe.write_all(&frame).await.unwrap();
                    let pending = pending_flush(&mut server.pipe);
                    drop(server);
                    assert!(pending);
                    if unwind {
                        panic!("controlled application unwind; outer runtime remains owned");
                    }
                    Err::<(), _>(io::Error::other("controlled application error"))
                })
            }));
            let held = state.live.load(Ordering::SeqCst);
            runtime.block_on(async {
                read_exact(&mut client, &frame).await;
                drop(client);
                state.settled().await;
            });
            assert_eq!(held, 1);
            assert_eq!(budget.available_permits(), 1);
            assert_eq!(result.is_err(), unwind);
            println!(
                "unwind={unwind} original_charge_held=true outer_runtime_cleanup_complete=true"
            );
        }
    }

    #[test]
    fn successful_final_close_joins_drain_before_releasing_the_owner() {
        use std::os::windows::io::AsRawHandle;
        runtime().block_on(async {
            let budget = Arc::new(Semaphore::new(1));
            let state = Arc::new(Retirement::default());
            let (mut server, mut client) = pair(&budget, &state, false).await;
            let frame = vec![b'f'; 8192];
            server.pipe.write_all(&frame).await.unwrap();
            tokio::time::timeout(WATCHDOG, server.pipe.flush())
                .await
                .unwrap()
                .unwrap();
            let retained = Arc::new(server);
            let worker_pipe = retained.clone();
            let worker = std::thread::Builder::new()
                .name("mio-probe-final-drain".into())
                .stack_size(1024 * 1024)
                .spawn(move || {
                    // SAFETY: the worker owns an Arc to the pipe through this
                    // call, and the caller retains its own Arc through join.
                    if unsafe { FlushFileBuffers(worker_pipe.pipe.as_raw_handle() as HANDLE) } == 0
                    {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                })
                .unwrap();
            read_exact(&mut client, &frame).await;
            worker.join().unwrap().unwrap();
            let held_through_join = state.live.load(Ordering::SeqCst);
            retained.pipe.disconnect().unwrap();
            drop((retained, client));
            state.settled().await;
            assert_eq!(held_through_join, 1);
            assert_eq!(budget.available_permits(), 1);
            println!("final_bytes=8192 complete_delivery=true drain_joined=true final_slots=1");
        });
    }
}
