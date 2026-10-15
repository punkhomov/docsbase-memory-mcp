//! Coverage and backfill through the job helpers with an explicit fake
//! embedder (FR-15, SC-1, SC-6). Ungated: no network, runs in the default
//! build. The provider-backed plan-embed path is exercised by the T5 live
//! smoke, not here.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::run_project_index;
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::HybridCfg;
use docsbase_memory::hybrid::jobs::{coverage_with, embed_project_chunks_with, embed_texts};
use docsbase_memory::store::Db;
use docsbase_memory::vector_cache::Embedder;
use tempfile::TempDir;

struct FakeEmbedder {
    calls: AtomicUsize,
}

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "fake/model@d3@l2"
    }

    #[allow(clippy::cast_precision_loss)]
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|text| vec![text.len() as f32, 0.5, 0.25])
            .collect())
    }
}

fn indexed_project() -> (TempDir, Db, i64) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for (name, body) in [
        ("a.md", "# A\n\nwidgets and sprockets.\n"),
        ("b.md", "# B\n\ncogs and gears.\n"),
        ("c.md", "# C\n\nmore widgets here.\n"),
        // Two byte-identical documents: their chunks share one content hash.
        ("dup-1.md", "identical duplicate body text.\n"),
        ("dup-2.md", "identical duplicate body text.\n"),
    ] {
        fs::write(root.path().join(name), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let project = registry::ensure_project(&mut db, &canonical).expect("project");
    run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
    (cache, db, project.id)
}

#[test]
fn coverage_and_backfill_with_a_fake_embedder() {
    let (_cache, db, project_id) = indexed_project();
    let embedder = FakeEmbedder {
        calls: AtomicUsize::new(0),
    };

    let before = coverage_with(&db, &embedder, project_id).expect("coverage");
    assert!(before.total > 0, "the project has chunks");
    assert_eq!(before.cached, 0, "nothing is embedded before the backfill");

    let backfilled = embed_project_chunks_with(&db, &embedder, project_id).expect("backfill");
    assert!(backfilled >= before.total, "every chunk text is processed");
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);

    let after = coverage_with(&db, &embedder, project_id).expect("coverage");
    assert_eq!(
        (after.cached, after.total),
        (before.total, before.total),
        "backfill reaches full coverage (SC-6)"
    );

    let again = embed_project_chunks_with(&db, &embedder, project_id).expect("second pass");
    assert!(again >= before.total);
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        1,
        "a fully covered second pass never touches the embedder (SC-1)"
    );
}

#[test]
fn disabled_hybrid_is_a_zero_no_op() {
    let (cache, db, project_id) = indexed_project();
    let embedded = embed_project_chunks_with(
        &db,
        &FakeEmbedder {
            calls: AtomicUsize::new(0),
        },
        project_id,
    )
    .expect("fake embedder");
    assert!(embedded > 0);

    let cfg = HybridCfg::default();
    let count = embed_texts(cache.path(), &cfg, &["x".to_owned()]).expect("disabled");
    assert_eq!(count, 0, "disabled stages must not embed");
}

#[cfg(not(feature = "hybrid-remote"))]
#[test]
fn enabled_hybrid_in_the_offline_build_is_an_admission_error() {
    let (cache, _db, _project_id) = indexed_project();
    let error = embed_texts(
        cache.path(),
        &HybridCfg::enabled_with_defaults(),
        &["x".to_owned()],
    )
    .expect_err("offline build must refuse enabled hybrid (FR-14)");
    assert!(
        matches!(error, docsbase_memory::error::Error::Admission { .. }),
        "{error}"
    );
}

#[test]
fn duplicate_chunk_texts_do_not_block_full_coverage() {
    let (_cache, db, project_id) = indexed_project();
    let embedder = FakeEmbedder {
        calls: AtomicUsize::new(0),
    };

    let before = coverage_with(&db, &embedder, project_id).expect("coverage");
    assert!(before.total > 0);

    let backfilled = embed_project_chunks_with(&db, &embedder, project_id).expect("backfill");
    assert!(backfilled >= before.total);

    let after = coverage_with(&db, &embedder, project_id).expect("coverage");
    assert_eq!(
        (after.cached, after.total),
        (before.total, before.total),
        "content-addressed coverage counts distinct chunk hashes and converges (FR-15)"
    );

    let calls = embedder.calls.load(Ordering::SeqCst);
    let again = embed_project_chunks_with(&db, &embedder, project_id).expect("second pass");
    assert!(again >= before.total);
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        calls,
        "converged coverage stays a no-op"
    );
}
