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
pub mod jobs;
pub mod rrf;

#[cfg(feature = "hybrid-local")]
pub mod models;
#[cfg(feature = "hybrid-remote")]
pub mod openrouter;
#[cfg(feature = "hybrid-remote")]
pub mod wire;

use std::cell::Cell;

use crate::error::Result;

pub use config::{EmbeddingCfg, HybridCfg, HybridStages, Ranking, RerankerCfg, from_config};

/// Cross-encoder style reranking stage (FR-2, FR-11).
///
/// Not cached (no `model_id`): rerank scores are per-query and never
/// persisted, so an identity would have no consumer (YAGNI).
pub trait Reranker: Send + Sync {
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
            let outer = flag.replace(true);
            let result = f();
            flag.set(outer);
            result
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
