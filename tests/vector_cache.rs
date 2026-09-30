//! Content-addressed embedding-cache contract: branch switches, worktree
//! duplication and model changes must not re-embed unchanged bytes (T53).

use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use docsbase_memory::error::{Error, Result};
use docsbase_memory::vector_cache::{CacheLimits, Embedder, VectorCache, chunk_hash};

struct FakeEmbedder {
    model: String,
    calls: AtomicUsize,
}

impl FakeEmbedder {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let len = u16::try_from(text.len()).map_or(f32::MAX, f32::from);
                let index = u16::try_from(index).map_or(f32::MAX, f32::from);
                vec![len, index, 1.0]
            })
            .collect())
    }
}

struct BadEmbedder;

impl Embedder for BadEmbedder {
    fn model_id(&self) -> &'static str {
        "bad"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .enumerate()
            .map(|(index, _)| {
                if index == 0 {
                    vec![1.0]
                } else {
                    vec![1.0, 2.0]
                }
            })
            .collect())
    }
}

fn texts(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}

fn cache() -> (TempDir, VectorCache) {
    let dir = TempDir::new().expect("dir");
    let cache = VectorCache::open(dir.path()).expect("cache");
    (dir, cache)
}

#[test]
fn unchanged_chunks_are_never_re_embedded() {
    let (_dir, mut cache) = cache();
    let embedder = FakeEmbedder::new("m1");
    let batch = texts(&["alpha doc", "beta doc", "gamma doc"]);

    cache.embed_missing(&embedder, &batch).expect("first");
    assert_eq!(embedder.calls(), 3);
    cache.embed_missing(&embedder, &batch).expect("second");
    assert_eq!(embedder.calls(), 3, "all hits must skip the embedder");
}

#[test]
fn editing_one_chunk_embeds_only_it() {
    let (_dir, mut cache) = cache();
    let embedder = FakeEmbedder::new("m1");
    cache
        .embed_missing(&embedder, &texts(&["alpha", "beta"]))
        .expect("first");
    cache
        .embed_missing(&embedder, &texts(&["alpha", "beta edited"]))
        .expect("second");
    assert_eq!(embedder.calls(), 3, "only the edited chunk is new");
}

#[test]
fn branch_switch_back_is_free() {
    let (_dir, mut cache) = cache();
    let embedder = FakeEmbedder::new("m1");
    // Branch A: a, b, c. Branch B: a, b, d (c removed). Back to A.
    cache
        .embed_missing(&embedder, &texts(&["a", "b", "c"]))
        .expect("A");
    cache
        .embed_missing(&embedder, &texts(&["a", "b", "d"]))
        .expect("B");
    assert_eq!(embedder.calls(), 4, "only `d` is new on the other branch");
    cache
        .embed_missing(&embedder, &texts(&["a", "b", "c"]))
        .expect("A again");
    assert_eq!(embedder.calls(), 4, "switching back must be free");
}

#[test]
fn worktrees_share_the_content_addressed_cache() {
    let (_dir, mut cache) = cache();
    let embedder = FakeEmbedder::new("m1");
    // Two worktrees with overlapping files: the overlap embeds once.
    // The gate is structural: the cache is keyed by a user-level cache root,
    // never by project/worktree, so both trees hit the same store.
    cache
        .embed_missing(&embedder, &texts(&["shared", "main only"]))
        .expect("main worktree");
    cache
        .embed_missing(&embedder, &texts(&["shared", "worktree only"]))
        .expect("linked worktree");
    assert_eq!(embedder.calls(), 3);
}

#[test]
fn duplicate_chunks_in_one_batch_embed_once() {
    let (_dir, mut cache) = cache();
    let embedder = FakeEmbedder::new("m1");
    let batch = texts(&["same", "other", "same"]);
    let vectors = cache.embed_missing(&embedder, &batch).expect("batch");

    assert_eq!(embedder.calls(), 2, "duplicates embed once per batch");
    assert_eq!(vectors[0], vectors[2], "duplicates share the vector");
    assert_eq!(cache.len().expect("len"), 2);
}

#[test]
fn model_change_invalidates_everything() {
    let (_dir, mut cache) = cache();
    let batch = texts(&["alpha", "beta"]);
    let first = FakeEmbedder::new("m1");
    cache.embed_missing(&first, &batch).expect("m1");
    assert_eq!(first.calls(), 2);

    let second = FakeEmbedder::new("m2");
    cache.embed_missing(&second, &batch).expect("m2");
    assert_eq!(second.calls(), 2, "new model must re-embed every chunk");

    let third = FakeEmbedder::new("m1");
    cache.embed_missing(&third, &batch).expect("m1 again");
    assert_eq!(third.calls(), 0, "the old model's vectors are still valid");
}

#[test]
fn inconsistent_embedder_dimensions_are_an_error() {
    let (_dir, mut cache) = cache();
    let err = cache
        .embed_missing(&BadEmbedder, &texts(&["alpha", "beta"]))
        .expect_err("inconsistent dims must fail");
    assert!(matches!(err, Error::Internal { .. }), "unexpected: {err}");
    assert_eq!(cache.len().expect("len"), 0, "nothing may be cached");
}

#[test]
fn direct_entries_match_chunk_hashes() {
    let (_dir, mut cache) = cache();
    let hash = chunk_hash("payload");
    assert!(cache.get("m1", &hash).expect("get").is_none());
    cache.put("m1", &hash, &[1.5, 2.5]).expect("put");
    let vector = cache.get("m1", &hash).expect("get").expect("hit");
    assert_eq!(vector, vec![1.5, 2.5]);
    assert!(
        cache.get("m2", &hash).expect("get other model").is_none(),
        "model id is part of the key"
    );
}

#[test]
fn byte_limits_are_enforced() {
    let dir = TempDir::new().expect("dir");
    let limits = CacheLimits {
        max_entries: 1_000,
        max_bytes: 8,
    };
    let mut cache = VectorCache::open_with_limits(dir.path(), limits).expect("cache");
    cache.put("m", "h1", &[1.0, 2.0]).expect("put h1");
    cache.put("m", "h2", &[3.0, 4.0]).expect("put h2");
    assert_eq!(cache.len().expect("len"), 1, "byte cap enforced");
    assert!(cache.get("m", "h2").expect("get").is_some(), "newest kept");
}

#[test]
fn corrupt_rows_are_rejected() {
    let dir = TempDir::new().expect("dir");
    let mut cache = VectorCache::open(dir.path()).expect("cache");
    let conn = rusqlite::Connection::open(dir.path().join("vectors.db")).expect("raw db");
    conn.execute(
        "INSERT INTO vector_cache (model_id, chunk_hash, dim, vector, used_at)
         VALUES ('m', 'bad', 3, X'0000', 1)",
        [],
    )
    .expect("insert corrupt row");
    drop(conn);

    let err = cache
        .get("m", "bad")
        .expect_err("corrupt row must be an error");
    assert!(matches!(err, Error::Internal { .. }), "unexpected: {err}");
}

#[test]
fn limits_are_enforced() {
    let dir = TempDir::new().expect("dir");
    let limits = CacheLimits {
        max_entries: 2,
        max_bytes: u64::MAX,
    };
    let mut cache = VectorCache::open_with_limits(dir.path(), limits).expect("cache");
    for hash in ["h1", "h2", "h3"] {
        cache.put("m", hash, &[1.0, 2.0]).expect("put");
    }
    assert_eq!(cache.len().expect("len"), 2, "entries cap enforced");
}
