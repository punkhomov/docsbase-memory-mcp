//! Daemon watcher coordination; provider waits use the owning runtime, never a worker.
use super::{
    Arc, BatchEvent, BatchRetryPolicy, Duration, Error, HashSet, Mutex, PathBuf, PoisonError,
    Project, Result, WatchContext, Watchers, Weak, lifecycle, transient_batch_error,
    transient_delay,
};
use crate::daemon::server::Shared;
use crate::search::budget::{PayloadBudget, PayloadPermit};
use crate::search::operation::SearchOperation;

/// Raw retry inputs and their single shared allowance. Data dies before charge.
pub(crate) struct WatchPayload {
    pub(crate) paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    raw_charge: u64,
    pub(crate) account: Arc<PayloadPermit>,
}
pub(crate) struct WatchPrepared {
    pub(crate) payload: Result<Option<Arc<WatchPayload>>>,
    pub(crate) rx: tokio::sync::mpsc::Receiver<Vec<PathBuf>>,
    pub(crate) _retention: Arc<crate::daemon::request::Activity>,
}

/// Called only on the existing admitted background CPU lane.
pub(crate) fn merge_pending(
    budget: &PayloadBudget,
    previous: Option<Arc<WatchPayload>>,
    first: Option<Vec<PathBuf>>,
    rx: &mut tokio::sync::mpsc::Receiver<Vec<PathBuf>>,
    operation: &SearchOperation,
    watchers: &Watchers,
) -> Result<Arc<WatchPayload>> {
    #[cfg(not(test))]
    let _ = watchers;
    let mut pending = match previous {
        Some(previous) => Arc::try_unwrap(previous)
            .map_err(|_| Error::internal("watcher raw input is still retained"))?,
        None => WatchPayload {
            paths: Vec::new(),
            seen: HashSet::new(),
            raw_charge: 0,
            account: Arc::new(budget.reserve(0)?),
        },
    };
    // A failed attempt settled its old native/codec owners before this phase.
    pending.account.shrink_shared(pending.raw_charge);
    let mut append = |paths: Vec<PathBuf>| -> Result<()> {
        for path in paths {
            operation.check()?;
            if pending.seen.contains(&path) {
                continue;
            }
            let charge = (path.as_os_str().len() as u64)
                .checked_mul(16)
                .and_then(|bytes| bytes.checked_add(512))
                .and_then(|bytes| pending.raw_charge.checked_add(bytes))
                .ok_or_else(|| Error::Admission {
                    message: "watcher pending capture overflow".into(),
                })?;
            pending.account.grow_shared(charge)?;
            #[cfg(test)]
            watchers.pending_copy_entered(&pending.account);
            pending.seen.insert(path.clone());
            pending.paths.push(path);
            pending.raw_charge = charge;
        }
        Ok(())
    };
    if let Some(first) = first {
        append(first)?;
    }
    while let Ok(extra) = rx.try_recv() {
        append(extra)?;
    }
    Ok(Arc::new(pending))
}

struct Exit {
    watchers: Weak<Watchers>,
    project: i64,
    context: Arc<Mutex<WatchContext>>,
}
impl Drop for Exit {
    fn drop(&mut self) {
        if let Some(watchers) = self.watchers.upgrade() {
            watchers.retire_matching(self.project, &self.context);
        }
    }
}
pub(super) async fn consume(
    shared: Weak<Shared>,
    project: Project,
    context: Arc<Mutex<WatchContext>>,
    mut rx: tokio::sync::mpsc::Receiver<Vec<PathBuf>>,
    watchers: Weak<Watchers>,
    close: crate::hybrid::work::Cancellation,
    cancel: crate::hybrid::work::Cancellation,
) {
    let _exit = Exit {
        watchers,
        project: project.id,
        context: context.clone(),
    };
    loop {
        let batch = tokio::select! { biased; () = close.cancelled() => break, () = cancel.cancelled() => break, batch = rx.recv() => match batch { Some(batch) => batch, None => break } };
        let Some(shared) = shared.upgrade() else {
            break;
        };
        let activity = shared.watch_activity();
        let Ok(operation) = shared.watch_operation() else {
            break;
        };
        let mut retry = RetryState {
            policy: BatchRetryPolicy::default(),
            transient: 0,
            leases: 0,
        };
        let mut pending = None;
        let mut first = Some(batch);
        loop {
            let cancellation = operation.cancellation().clone();
            let prepared = tokio::select! { biased;
                () = close.cancelled() => { cancellation.cancel(); return; },
                () = cancel.cancelled() => { cancellation.cancel(); return; },
                result = shared.prepare_watch_pending(&project, pending.take(), first.take(), rx, &operation, &activity) => result,
            };
            let Ok(prepared) = prepared else { return };
            rx = prepared.rx;
            pending = match prepared.payload {
                Ok(Some(pending)) => Some(pending),
                Ok(None) => {
                    let _ = shared
                        .report_watch_event(
                            &project,
                            BatchEvent::RootMissing,
                            None,
                            &operation,
                            &activity,
                        )
                        .await;
                    return;
                }
                Err(error) => {
                    let _ = shared
                        .report_watch_event(
                            &project,
                            BatchEvent::DroppedPermanent {
                                error: error.to_string(),
                            },
                            None,
                            &operation,
                            &activity,
                        )
                        .await;
                    break;
                }
            };
            let snapshot = context
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let WatchContext::Prepared(runtime) = snapshot else {
                break;
            };
            let Some(input) = pending.as_ref() else { break };
            let cancellation = operation.cancellation().clone();
            let result = tokio::select! { biased;
                () = close.cancelled() => { cancellation.cancel(); return; },
                () = cancel.cancelled() => { cancellation.cancel(); return; },
                result = shared.run_watch_batch(project.clone(), runtime.clone(), input.clone(), operation.clone(), activity.clone()) => result,
            };
            match result {
                Ok(stats) => {
                    eprintln!(
                        "watcher: project {} refreshed {} doc(s), {} chunk(s), {} error(s)",
                        project.id, stats.docs, stats.chunks, stats.errors
                    );
                    break;
                }
                Err(error) => {
                    let error = revision_error(error, runtime, &context);
                    let (event, pause) = retry.failure(&error);
                    let _ = shared
                        .report_watch_event(&project, event, pending.clone(), &operation, &activity)
                        .await;
                    let Some(pause) = pause else { break };
                    if !delay(&operation, &close, &cancel, pause).await {
                        break;
                    }
                }
            }
        }
    }
}
async fn delay(
    operation: &SearchOperation,
    close: &crate::hybrid::work::Cancellation,
    cancel: &crate::hybrid::work::Cancellation,
    duration: Duration,
) -> bool {
    crate::hybrid::work::RemoteCall::new(operation.clone()).wait(close, async {
        tokio::select! { biased;
            () = cancel.cancelled() => Err(Error::Admission { message: "watcher stopped".into() }),
            () = tokio::time::sleep(duration) => Ok(()),
        }
    }).await.is_ok()
}

struct RetryState {
    policy: BatchRetryPolicy,
    transient: u32,
    leases: u32,
}
impl RetryState {
    fn failure(&mut self, error: &Error) -> (BatchEvent, Option<Duration>) {
        if lifecycle::is_lease_conflict(error) {
            self.leases = self.leases.saturating_add(1);
            return (
                BatchEvent::LeaseRetry {
                    attempt: self.leases,
                },
                Some(self.policy.lease_retry),
            );
        }
        if transient_batch_error(error) {
            if self.transient < self.policy.max_transient_attempts {
                self.transient += 1;
                return (
                    BatchEvent::TransientRetry {
                        attempt: self.transient,
                        error: error.to_string(),
                    },
                    Some(transient_delay(&self.policy, self.transient)),
                );
            }
            return (
                BatchEvent::DroppedTransient {
                    attempts: self.transient,
                    error: error.to_string(),
                },
                None,
            );
        }
        (
            BatchEvent::DroppedPermanent {
                error: error.to_string(),
            },
            None,
        )
    }
}

fn revision_error(
    error: Error,
    captured: Arc<super::WatchRuntime>,
    context: &Mutex<WatchContext>,
) -> Error {
    // Retry only an actually changed captured context. Every
    // attempt still performs the permanent root/schema guard
    // before effects; the same original retry clock applies.
    let current = context
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if WatchContext::Prepared(captured).same_revision(&current) {
        error
    } else {
        Error::internal("watcher config changed while preparing batch")
    }
}
