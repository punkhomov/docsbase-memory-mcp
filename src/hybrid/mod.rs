//! Hybrid retrieval stages (iteration 1: `OpenRouter` embeddings + rerank).
//!
//! The module skeleton is ungated: configuration resolution, the feature-off
//! admission path and the blocking-context contract compile and test in the
//! default offline build (FR-14). Only the wire/client implementation sits
//! behind the `hybrid-remote` feature (SC-4).
//!
//! Every embed/rerank call must run inside [`blocking::enter`]: the daemon
//! routes tool calls through `spawn_blocking` (`daemon::server`), the watcher
//! consumer owns its thread and the direct CLI mode runs on the main thread,
//! so a sanctioned blocking context is always available without blocking an
//! async worker (ADR-7, I3).

pub mod ann;
pub mod ann_store;
pub mod config;
pub mod dense;
pub mod dense_metadata;
pub mod dense_settings;
mod flight;
pub mod jobs;
pub mod meta;
pub mod provider;
#[cfg(feature = "hybrid-remote")]
pub mod remote;
pub mod remote_settings;
pub mod rrf;
pub mod work;

#[cfg(feature = "hybrid-local")]
pub mod local;
#[cfg(feature = "hybrid-local")]
pub mod models;
#[cfg(feature = "hybrid-remote")]
pub mod openrouter;
#[cfg(feature = "hybrid-remote")]
mod rig_codec;
#[cfg(feature = "hybrid-local")]
pub mod runtime;
#[cfg(feature = "hybrid-remote")]
pub mod wire;

use std::cell::Cell;

use crate::error::Result;

pub use config::{
    EmbeddingCfg, FastembedProvider, HybridCfg, HybridStages, OpenrouterProvider,
    PROVIDER_FASTEMBED, ProviderState, ProviderStatus, ProvidersCfg, Ranking, RerankerCfg,
    embedding_model_id, from_config, provider_status, resolve_embedding, resolve_reranker,
};
pub use provider::{ModelAddress, Provider, Task, provider, providers};

/// Cross-encoder style reranking stage (FR-2, FR-11).
///
/// Not cached (no `model_id`): rerank scores are per-query and never
/// persisted, so an identity would have no consumer (YAGNI).
pub trait Reranker: Send + Sync {
    /// Explicit remote capability; None keeps bounded local execution.
    #[cfg(feature = "hybrid-remote")]
    fn async_remote(&self) -> Option<&dyn remote::AsyncReranker> {
        None
    }
    /// Scores `docs` against `query`; returns `(original_index, score)` pairs,
    /// best first. `top_n` caps the number of returned pairs.
    ///
    /// # Errors
    /// Provider or transport failures (never a silent fallback, NFR-2).
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>>;
}

thread_local! {
    static IN_BLOCKING: Cell<bool> = const { Cell::new(false) };
}

/// The sanctioned execution context for hybrid network calls (I3).
pub mod blocking {
    use std::cell::Cell;

    use crate::error::Result;

    use super::IN_BLOCKING;

    thread_local! {
        static SYNC_BRIDGE: std::cell::RefCell<Option<std::sync::Arc<crate::daemon::remote::SyncBridge>>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn bridge() -> Option<std::sync::Arc<crate::daemon::remote::SyncBridge>> {
        SYNC_BRIDGE.with(|current| current.borrow().clone())
    }

    // Compatibility construction scope only. It never encloses an await.
    pub(crate) fn with_bridge<T>(
        bridge: Option<std::sync::Arc<crate::daemon::remote::SyncBridge>>,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        struct Restore(Option<std::sync::Arc<crate::daemon::remote::SyncBridge>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SYNC_BRIDGE.with(|current| {
                    current.replace(self.0.take());
                });
            }
        }
        SYNC_BRIDGE.with(|current| {
            let _restore = Restore(current.replace(bridge));
            enter(work)
        })
    }

    struct ContextReset<'a> {
        flag: &'a Cell<bool>,
        previous: bool,
    }

    impl Drop for ContextReset<'_> {
        fn drop(&mut self) {
            self.flag.set(self.previous);
        }
    }

    /// Runs `f` marked as a blocking context.
    ///
    /// Call this from a plain thread or from inside `spawn_blocking`; never
    /// from an async task. The daemon routes every tool call through
    /// `spawn_blocking` (`daemon::server::route`), so search and index paths
    /// satisfy this by construction.
    ///
    /// # Errors
    /// Propagates `f`'s error unchanged.
    pub fn enter<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        IN_BLOCKING.with(|flag| {
            let _reset = ContextReset {
                flag,
                previous: flag.replace(true),
            };
            f()
        })
    }

    /// Whether the current thread is inside [`enter`].
    ///
    /// The transport guard uses this to reject unsanctioned calls with an
    /// explicit error instead of panicking inside `Handle::block_on`.
    #[must_use]
    pub fn in_context() -> bool {
        IN_BLOCKING.with(Cell::get)
    }
}

#[cfg(test)]
mod blocking_tests {
    use super::blocking;
    use crate::error::Result;

    #[test]
    fn blocking_context_is_restored_after_provider_unwind() {
        // Isolate the old leaked thread-local flag from the test harness.
        std::thread::spawn(|| {
            assert!(!blocking::in_context());
            let failed = std::panic::catch_unwind(|| {
                let _: Result<()> = blocking::enter(|| panic!("provider unwind"));
            });
            assert!(failed.is_err());
            assert!(
                !blocking::in_context(),
                "provider unwind must restore context"
            );
        })
        .join()
        .expect("context test");
    }

    #[test]
    fn nested_provider_unwind_restores_outer_context() {
        blocking::enter(|| {
            assert!(blocking::in_context());
            let failed = std::panic::catch_unwind(|| {
                let _: Result<()> = blocking::enter(|| panic!("nested provider unwind"));
            });
            assert!(failed.is_err());
            assert!(blocking::in_context());
            Ok(())
        })
        .expect("outer context");
        assert!(!blocking::in_context());
    }
}
