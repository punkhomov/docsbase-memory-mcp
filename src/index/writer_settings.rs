//! Owner of the native Tantivy writer's arena and thread policy.

use tantivy::indexer::IndexWriterOptions;

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use crate::config::view::ViewBuilder;
use crate::error::{Error, Result};

// Pinned to Tantivy 0.26.2's private IndexWriterOptions validation.
const MIN_ARENA_BYTES: usize = 15_000_000;
const MAX_ARENA_BYTES_EXCLUSIVE: usize = u32::MAX as usize - 1_000_000;
const MAX_THREADS: usize = 8;

/// Validated settings for a single lexical writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexWriterSettings {
    heap_bytes: usize,
    worker_threads: usize,
    merge_threads: usize,
}

impl IndexWriterSettings {
    /// Construct the worker and merge policy for a writer.
    /// # Errors
    /// The thread counts are outside the supported range or the heap split is
    /// outside Tantivy's per-worker arena domain.
    pub fn new(heap_bytes: usize, worker_threads: usize, merge_threads: usize) -> Result<Self> {
        if !(1..=MAX_THREADS).contains(&worker_threads)
            || !(1..=MAX_THREADS).contains(&merge_threads)
        {
            return Err(Error::Admission {
                message: format!("index.writer thread counts must be between 1 and {MAX_THREADS}"),
            });
        }
        if !(MIN_ARENA_BYTES..MAX_ARENA_BYTES_EXCLUSIVE).contains(&(heap_bytes / worker_threads)) {
            return Err(Error::Admission {
                message: format!(
                    "index.writer.heap_bytes must give each worker an arena in {MIN_ARENA_BYTES}..{MAX_ARENA_BYTES_EXCLUSIVE} bytes"
                ),
            });
        }
        Ok(Self {
            heap_bytes,
            worker_threads,
            merge_threads,
        })
    }

    pub(crate) const fn owner_defaults() -> Self {
        Self {
            heap_bytes: 20_000_000,
            // The former 20 MB automatic budget always selected one worker.
            worker_threads: 1,
            merge_threads: 4,
        }
    }

    pub(crate) fn options(self) -> IndexWriterOptions {
        IndexWriterOptions::builder()
            .memory_budget_per_thread(self.heap_bytes / self.worker_threads)
            .num_worker_threads(self.worker_threads)
            .num_merge_threads(self.merge_threads)
            .build()
    }

    pub(crate) fn view_fields(self, view: &mut ViewBuilder<'_>) -> Result<()> {
        for (key, value) in [
            ("index.writer.heap_bytes", self.heap_bytes),
            ("index.writer.worker_threads", self.worker_threads),
            ("index.writer.merge_threads", self.merge_threads),
        ] {
            view.push(key, value.into(), None)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterPatch {
    heap_bytes: Option<usize>,
    worker_threads: Option<usize>,
    merge_threads: Option<usize>,
}

pub(crate) struct WriterSpec;

impl SettingsSpec for WriterSpec {
    type Patch = WriterPatch;
    type Settings = IndexWriterSettings;
    type Defaults = IndexWriterSettings;
    const SCOPE: Scope = Scope::Daemon;
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec::new("index.writer.heap_bytes", Scope::Daemon, Unit::Bytes),
        FieldSpec::new("index.writer.worker_threads", Scope::Daemon, Unit::Count),
        FieldSpec::new("index.writer.merge_threads", Scope::Daemon, Unit::Count),
    ];

    fn defaults(_: &DefaultsContext) -> IndexWriterSettings {
        IndexWriterSettings::owner_defaults()
    }

    fn initial(defaults: &IndexWriterSettings) -> IndexWriterSettings {
        *defaults
    }

    fn overlay(
        settings: &mut IndexWriterSettings,
        patch: WriterPatch,
        _: &IndexWriterSettings,
        source: &SourceId,
        origins: &mut Provenance,
    ) -> Result<()> {
        let candidate = IndexWriterSettings::new(
            patch.heap_bytes.unwrap_or(settings.heap_bytes),
            patch.worker_threads.unwrap_or(settings.worker_threads),
            patch.merge_threads.unwrap_or(settings.merge_threads),
        )?;
        for (key, value) in [
            ("index.writer.heap_bytes", patch.heap_bytes),
            ("index.writer.worker_threads", patch.worker_threads),
            ("index.writer.merge_threads", patch.merge_threads),
        ] {
            if value.is_some() {
                origins.set(key, source)?;
            }
        }
        *settings = candidate;
        Ok(())
    }

    fn validate(_: &IndexWriterSettings, _: &Capabilities) -> Result<()> {
        Ok(())
    }

    fn impact(
        old: Option<&IndexWriterSettings>,
        next: &IndexWriterSettings,
        _: &ArtifactFacts,
    ) -> ComponentImpact {
        ComponentImpact::between(old, next)
    }
}
