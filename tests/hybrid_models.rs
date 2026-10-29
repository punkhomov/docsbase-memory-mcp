#![cfg(feature = "hybrid-local")]
//! Integration coverage for `src/hybrid/models.rs`: the public API and the
//! `DOCSBASE_CACHE_DIR` environment wiring. Behavior-level tests (locks,
//! hashes, caps) live in the module's unit tests; this file exercises the
//! real entry points once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use docsbase_memory::error::Error;
use docsbase_memory::hybrid::models::{
    FileSpec, ModelEntry, ModelSource, Pooling, REGISTRY, Task, cache_dir, cached, canonical_id,
    entry, expected_bytes, fetch,
};
use tempfile::TempDir;

/// Serializes tests that mutate the process-wide `DOCSBASE_CACHE_DIR`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Deterministic fake source matching `fake_entry` sizes/hashes.
struct FakeSource {
    data: HashMap<String, Vec<u8>>,
    calls: Arc<AtomicUsize>,
}

impl FakeSource {
    fn for_entry(entry: &ModelEntry<'_>) -> Self {
        let data = entry
            .files
            .iter()
            .map(|spec| (spec.path.to_owned(), fake_content(spec)))
            .collect();
        Self {
            data,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

fn fake_content(spec: &FileSpec) -> Vec<u8> {
    let size = usize::try_from(spec.size).unwrap_or(usize::MAX);
    let block = spec.path.as_bytes();
    let mut content = Vec::with_capacity(size);
    while content.len() < size {
        let rest = size - content.len();
        content.extend_from_slice(&block[..block.len().min(rest)]);
    }
    content
}

impl FakeSource {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl ModelSource for FakeSource {
    fn fetch(
        &self,
        _repo: &str,
        _revision: &str,
        path: &str,
    ) -> docsbase_memory::error::Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.data
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Admission {
                message: format!("fake source has no file {path}"),
            })
    }
}

fn fake_entry() -> ModelEntry<'static> {
    let files: &[FileSpec] = &[
        FileSpec {
            path: "onnx/model.onnx",
            size: 4096,
            sha256: "db743c223182c9ef5ecefd77fbd39a0bb571068bae9d254c728d09d226db5902",
        },
        FileSpec {
            path: "tokenizer.json",
            size: 512,
            sha256: "81c41ed08f50596b10289d11454c080a74710904e632a2697449d3a7e7a8f2de",
        },
    ];
    ModelEntry {
        name: "fake-model",
        task: Task::Embedding,
        repo: "fake/repo",
        revision: "0123456789abcdef0123456789abcdef01234567",
        dim: Some(16),
        pooling: Some(Pooling::Mean),
        quantization: None,
        output_key: None,
        license: "MIT",
        query_prefix: "query: ",
        passage_prefix: "passage: ",
        prompt_version: 2,
        files,
    }
}

#[test]
fn registry_lists_real_rows_via_entry() {
    let names: Vec<&str> = REGISTRY.iter().map(|entry| entry.name).collect();
    assert!(names.contains(&"bge-small-en-v1.5"));
    assert!(names.contains(&"multilingual-e5-small"));
    assert!(names.contains(&"jina-reranker-v1-turbo-en"));
    assert!(names.contains(&"bge-reranker-base"));
    let model = entry("multilingual-e5-small").expect("known entry");
    assert_eq!(model.dim, Some(384));
    assert_eq!(model.revision.len(), 40, "full commit sha");
    let err = entry("no-such-model").expect_err("unknown entry");
    assert!(matches!(err, Error::Admission { .. }));
}

#[test]
#[allow(unsafe_code)] // test-only env mutation, serialized by ENV_LOCK
fn fetch_and_cached_pipeline_with_env() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = TempDir::new().expect("tmp");
    // SAFETY: test-only, serialized by ENV_LOCK; the value is restored at the
    // end of the test.
    unsafe {
        std::env::set_var("DOCSBASE_CACHE_DIR", dir.path());
    }
    let entry = fake_entry();
    let source = FakeSource::for_entry(&entry);
    assert!(!cached(&entry).expect("not cached yet"));

    let (installed, installed_flag) = fetch(&entry, &source, false).expect("fetch");
    assert_eq!(installed, cache_dir(entry.name, entry.revision));
    assert!(
        installed_flag,
        "fresh install reports installed (round-4 C5)"
    );
    assert!(installed.join(".complete").exists());
    assert!(cached(&entry).expect("cached after fetch"));

    // Second fetch is a no-op and downloads nothing.
    let source2 = FakeSource::for_entry(&entry);
    let (_, fast_flag) = fetch(&entry, &source2, false).expect("second fetch");
    assert!(
        !fast_flag,
        "verified-complete fast path reports not installed"
    );
    assert_eq!(source2.calls(), 0, "complete fetch downloads nothing");

    // Tampering is detected by `cached`.
    std::fs::write(installed.join("tokenizer.json"), b"tampered").expect("tamper");
    assert!(!cached(&entry).expect("tampered not cached"));

    // SAFETY: restore the previous value (unset).
    unsafe {
        std::env::remove_var("DOCSBASE_CACHE_DIR");
    }
}

#[test]
fn canonical_id_matches_spec_format() {
    let entry = fake_entry();
    let id = canonical_id(&entry, 384, true);
    assert_eq!(id, "fastembed/fake-model@d384@l2@r01234567@p2");
    let raw = canonical_id(&entry, 16, false);
    assert!(raw.ends_with("@d16@raw@r01234567@p2"), "{raw}");
}

#[test]
fn expected_bytes_is_positive() {
    for entry in REGISTRY {
        assert!(
            expected_bytes(entry) > 0,
            "{} has positive expected bytes",
            entry.name
        );
    }
}
