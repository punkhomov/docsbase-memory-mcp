//! Owner of the native Tantivy writer and stale directory recovery policy.

use std::time::Duration;

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
const DEFAULT_STAGING_DISK_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REMOVE_ATTEMPTS: u32 = 1000;
const MAX_REMOVE_WAIT_MS: u64 = 10_000;

/// Validated settings for a single lexical writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexWriterSettings {
    heap_bytes: usize,
    worker_threads: usize,
    merge_threads: usize,
    staging_disk_bytes: u64,
    remove_attempts: u32,
    remove_retry_delay_ms: u64,
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
            ..Self::owner_defaults()
        })
    }

    pub(crate) const fn owner_defaults() -> Self {
        Self {
            heap_bytes: 20_000_000,
            // The former 20 MB automatic budget always selected one worker.
            worker_threads: 1,
            merge_threads: 4,
            staging_disk_bytes: DEFAULT_STAGING_DISK_BYTES,
            remove_attempts: 20,
            remove_retry_delay_ms: 100,
        }
    }

    pub(crate) const fn staging_disk_bytes(self) -> u64 {
        self.staging_disk_bytes
    }

    pub(super) const fn removal(self) -> (u32, Duration) {
        (
            self.remove_attempts,
            Duration::from_millis(self.remove_retry_delay_ms),
        )
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
        view.push(
            "index.writer.staging_disk_bytes",
            self.staging_disk_bytes.into(),
            None,
        )?;
        view.push(
            "index.writer.remove_attempts",
            self.remove_attempts.into(),
            None,
        )?;
        view.push(
            "index.writer.remove_retry_delay_ms",
            self.remove_retry_delay_ms.into(),
            None,
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterPatch {
    heap_bytes: Option<usize>,
    worker_threads: Option<usize>,
    merge_threads: Option<usize>,
    staging_disk_bytes: Option<u64>,
    remove_attempts: Option<u32>,
    remove_retry_delay_ms: Option<u64>,
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
        FieldSpec::new(
            "index.writer.staging_disk_bytes",
            Scope::Daemon,
            Unit::Bytes,
        ),
        FieldSpec::new("index.writer.remove_attempts", Scope::Daemon, Unit::Count),
        FieldSpec::new(
            "index.writer.remove_retry_delay_ms",
            Scope::Daemon,
            Unit::Milliseconds,
        ),
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
        let mut candidate = IndexWriterSettings::new(
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
        candidate.staging_disk_bytes = patch
            .staging_disk_bytes
            .unwrap_or(settings.staging_disk_bytes);
        if patch.staging_disk_bytes.is_some() {
            origins.set("index.writer.staging_disk_bytes", source)?;
        }
        candidate.remove_attempts = patch.remove_attempts.unwrap_or(settings.remove_attempts);
        candidate.remove_retry_delay_ms = patch
            .remove_retry_delay_ms
            .unwrap_or(settings.remove_retry_delay_ms);
        if patch.remove_attempts.is_some() {
            origins.set("index.writer.remove_attempts", source)?;
        }
        if patch.remove_retry_delay_ms.is_some() {
            origins.set("index.writer.remove_retry_delay_ms", source)?;
        }
        *settings = candidate;
        Ok(())
    }

    fn validate(settings: &IndexWriterSettings, _: &Capabilities) -> Result<()> {
        if settings.staging_disk_bytes <= 2 * blake3::OUT_LEN as u64
            || i64::try_from(settings.staging_disk_bytes).is_err()
        {
            return Err(Error::Admission {
                message: "index.writer.staging_disk_bytes must cover both ownership records and fit a SQLite integer".into(),
            });
        }
        if !(1..=MAX_REMOVE_ATTEMPTS).contains(&settings.remove_attempts) {
            return Err(Error::Admission {
                message: format!(
                    "index.writer.remove_attempts must be between 1 and {MAX_REMOVE_ATTEMPTS}"
                ),
            });
        }
        let waits = u64::from(settings.remove_attempts - 1);
        if settings.remove_retry_delay_ms.saturating_mul(waits) > MAX_REMOVE_WAIT_MS {
            return Err(Error::Admission {
                message: format!(
                    "index.writer.remove_retry_delay_ms must keep the total wait within {MAX_REMOVE_WAIT_MS} ms"
                ),
            });
        }
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
