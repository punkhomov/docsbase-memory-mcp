//! Cooperative cancellation state; it does not interrupt a running provider.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::error::{Error, Result};

/// One operation's cancellation state, independent of configuration/time budgets.
#[derive(Clone, Default)]
pub struct Cancellation {
    state: Arc<SharedState>,
}

#[derive(Default)]
struct SharedState {
    data: Mutex<State>,
    notify: tokio::sync::Notify,
}

#[derive(Default)]
struct State {
    cancelled: bool,
    listeners: Vec<Weak<Wake>>,
}

impl Cancellation {
    /// Creates an active operation. Cancellation is permanent for this token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels this operation and wakes its current subscribers.
    pub fn cancel(&self) {
        let listeners = {
            let mut state = self
                .state
                .data
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.cancelled {
                return;
            }
            state.cancelled = true;
            std::mem::take(&mut state.listeners)
        };
        self.state.notify.notify_waiters();
        for wake in listeners.into_iter().filter_map(|wake| wake.upgrade()) {
            wake.notify();
        }
    }

    /// Waits on this token's permanent state without occupying a blocking worker.
    /// Dropping this future unregisters its Notify waiter.
    pub async fn cancelled(&self) {
        let notified = self.state.notify.notified();
        tokio::pin!(notified);
        // Register before checking the bit: cancel between these operations
        // either sets the bit we read or wakes this already enabled waiter.
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }

    /// Whether cancellation was explicitly requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }

    /// Checks a cooperative boundary before starting another operation phase.
    ///
    /// # Errors
    /// Admission failure when the caller cancelled this operation.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Admission {
                message: "shared work request cancelled".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn listen(&self) -> Listener {
        // One pending wake is a coalescing protocol invariant, not a work cap.
        // All terminal state is published before the nonblocking notification.
        let (sender, receiver) = mpsc::sync_channel(1);
        let wake = Arc::new(Wake {
            sender,
            cancellation: self.clone(),
        });
        let weak = Arc::downgrade(&wake);
        let cancelled = {
            let mut state = self
                .state
                .data
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !state.cancelled {
                state.listeners.push(weak.clone());
            }
            state.cancelled
        };
        if cancelled {
            wake.notify();
        }
        Listener {
            wake,
            receiver,
            _subscription: Subscription {
                cancellation: self.clone(),
                wake: weak,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.state
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .len()
    }
}

pub(crate) struct Wake {
    sender: SyncSender<()>,
    cancellation: Cancellation,
}

impl Wake {
    pub(crate) fn notify(&self) {
        // Full means a wake is already pending; disconnected means it left.
        let _ = self.sender.try_send(());
    }

    pub(crate) fn active(&self) -> bool {
        !self.cancellation.is_cancelled()
    }
}

pub(crate) struct Listener {
    pub(crate) wake: Arc<Wake>,
    receiver: Receiver<()>,
    _subscription: Subscription,
}

impl Listener {
    pub(crate) fn wait(&self) -> Result<()> {
        self.receiver
            .recv()
            .map_err(|err| Error::internal_with_source("shared work notification channel", err))
    }
}

struct Subscription {
    cancellation: Cancellation,
    wake: Weak<Wake>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.cancellation
            .state
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .retain(|wake| !Weak::ptr_eq(wake, &self.wake));
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::*;

    #[derive(Default)]
    struct Wakes(AtomicUsize);

    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn async_drop_unsubscribes_without_a_blocking_listener() {
        let token = Cancellation::new();
        let listener = token.listen();
        assert_eq!(token.subscriber_count(), 1);
        let wake_count = Arc::new(Wakes::default());
        let waker = Waker::from(Arc::clone(&wake_count));
        let mut context = Context::from_waker(&waker);
        let mut waiting = Box::pin(token.cancelled());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        assert_eq!(token.subscriber_count(), 1);
        assert_eq!(Arc::strong_count(&wake_count), 3);
        drop(waiting);
        assert_eq!(Arc::strong_count(&wake_count), 2);
        token.cancel();
        listener.wait().unwrap();
        assert_eq!(wake_count.0.load(Ordering::SeqCst), 0);
        assert_eq!(token.subscriber_count(), 0);
        token.cancelled().await;
    }

    #[test]
    fn cancellation_after_construction_before_registration_is_ready() {
        let token = Cancellation::new();
        let mut waiting = Box::pin(token.cancelled());
        token.cancel();
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(waiting.as_mut().poll(&mut context), Poll::Ready(()));
    }
}
