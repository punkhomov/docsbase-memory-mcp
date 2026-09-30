//! Content-addressed cache throughput (T53): hash lookups must stay cheap
//! enough to run before every embed batch.

use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use docsbase_memory::error::Result;
use docsbase_memory::vector_cache::{Embedder, VectorCache, chunk_hash};
use tempfile::TempDir;

const CHUNKS: usize = 1_000;

struct FakeEmbedder {
    calls: AtomicUsize,
}

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "bench"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|text| {
                let len = u16::try_from(text.len()).map_or(f32::MAX, f32::from);
                vec![len, 1.0, 2.0]
            })
            .collect())
    }
}

fn corpus() -> Vec<String> {
    (0..CHUNKS)
        .map(|index| format!("chunk number {index} with some prose payload"))
        .collect()
}

fn warm_cache(dir: &TempDir) -> (VectorCache, Vec<String>, Vec<String>) {
    let texts = corpus();
    let mut cache = VectorCache::open(dir.path()).expect("cache");
    let embedder = FakeEmbedder {
        calls: AtomicUsize::new(0),
    };
    cache.embed_missing(&embedder, &texts).expect("warm");
    let hashes = texts.iter().map(|text| chunk_hash(text)).collect();
    (cache, texts, hashes)
}

fn cache_benchmarks(c: &mut Criterion) {
    c.bench_function("vector_cache_embed_all_hits_1000", |b| {
        let dir = TempDir::new().expect("dir");
        let (mut cache, texts, _hashes) = warm_cache(&dir);
        let embedder = FakeEmbedder {
            calls: AtomicUsize::new(0),
        };
        b.iter_batched(
            || cache.embed_missing(&embedder, &texts).expect("batch"),
            |vectors| {
                assert_eq!(vectors.len(), CHUNKS);
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("vector_cache_lookup_1000", |b| {
        let dir = TempDir::new().expect("dir");
        let (mut cache, _texts, hashes) = warm_cache(&dir);
        b.iter(|| {
            for hash in &hashes {
                assert!(cache.get("bench", hash).expect("get").is_some());
            }
        });
    });
}

criterion_group!(benches, cache_benchmarks);
criterion_main!(benches);
