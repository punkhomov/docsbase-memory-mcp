//! Coverage and backfill through the job helpers with an explicit fake
//! embedder (FR-15, SC-1, SC-6). Ungated: no network, runs in the default
//! build. The provider-backed plan-embed path is exercised by the T5 live
//! smoke, not here.

#[path = "support/vector_leases.rs"]
mod vector_leases;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::run_project_index;
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::HybridCfg;
use docsbase_memory::hybrid::jobs::{coverage_with, embed_project_chunks_with, embed_texts};
use docsbase_memory::store::Db;
use docsbase_memory::vector_cache::Embedder;
use docsbase_memory::vector_cache::VectorCache;
use tempfile::TempDir;
use vector_leases::lease_count;

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
    let count = embed_texts(
        cache.path(),
        &cfg,
        &docsbase_memory::hybrid::config::ProvidersCfg::default(),
        &["x".to_owned()],
    )
    .expect("disabled");
    assert_eq!(count, 0, "disabled stages must not embed");
}

#[cfg(not(feature = "hybrid-remote"))]
#[test]
fn enabled_hybrid_in_the_offline_build_is_an_admission_error() {
    let (cache, _db, _project_id) = indexed_project();
    let error = embed_texts(
        cache.path(),
        &HybridCfg::enabled_with_defaults(),
        &docsbase_memory::hybrid::config::ProvidersCfg::default(),
        &["x".to_owned()],
    )
    .expect_err("offline build must refuse enabled hybrid (FR-14)");
    assert!(
        matches!(error, docsbase_memory::error::Error::Admission { .. }),
        "{error}"
    );
}

/// Embedder that records how many leases the cache held while it ran: the
/// backfill pins *before* embedding, so this observes the admission decision.
struct LeaseProbe {
    root: PathBuf,
    seen: AtomicU64,
}

impl LeaseProbe {
    fn new(root: &std::path::Path) -> Self {
        Self {
            root: root.to_path_buf(),
            seen: AtomicU64::new(u64::MAX),
        }
    }

    fn seen(&self) -> u64 {
        self.seen.load(Ordering::SeqCst)
    }
}

impl Embedder for LeaseProbe {
    fn model_id(&self) -> &'static str {
        "fake/model@d3@l2"
    }

    #[allow(clippy::cast_precision_loss)]
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.seen
            .store(lease_count(&self.root, self.model_id())?, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|text| vec![text.len() as f32, 0.5, 0.25])
            .collect())
    }
}

#[test]
fn backfill_leases_only_when_the_build_can_be_admitted() {
    let (cache, db, project_id) = indexed_project();
    // Dim 3 costs 1048 bytes per row (graph 1036 + exact 12): 4 distinct hashes
    // fit 4500 bytes, 5 chunk rows do not. The lease must follow the row count
    // (what `resolve_rows` admits), not the deduped hash count.
    let small = docsbase_memory::hybrid::dense::DenseCacheLimits {
        max_bytes: 4_500,
        min_vectors: 1,
        ..docsbase_memory::hybrid::dense::DenseCacheLimits::default()
    };
    let probe = LeaseProbe::new(cache.path());
    docsbase_memory::hybrid::jobs::embed_project_chunks_with_dense_policy(
        &db,
        &probe,
        project_id,
        docsbase_memory::vector_cache::CacheLimits::default(),
        true,
        &small,
    )
    .expect("backfill");
    assert_eq!(
        probe.seen(),
        0,
        "a corpus the build cannot admit must not take a backfill lease"
    );

    // Control: a fresh cache forces embedding for the same admitted corpus.
    let (cache, db, project_id) = indexed_project();
    let admitted = docsbase_memory::hybrid::dense::DenseCacheLimits {
        min_vectors: 1,
        ..docsbase_memory::hybrid::dense::DenseCacheLimits::default()
    };
    let probe = LeaseProbe::new(cache.path());
    docsbase_memory::hybrid::jobs::embed_project_chunks_with_dense_policy(
        &db,
        &probe,
        project_id,
        docsbase_memory::vector_cache::CacheLimits::default(),
        true,
        &admitted,
    )
    .expect("backfill");
    assert_eq!(
        probe.seen(),
        4,
        "an admitted corpus must pin its vectors before embedding"
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

#[test]
fn below_threshold_backfill_does_not_suspend_the_vector_budget() {
    let (cache, db, project_id) = indexed_project();
    let probe = LeaseProbe::new(cache.path());
    embed_project_chunks_with(&db, &probe, project_id).expect("backfill");
    assert_eq!(
        probe.seen(),
        0,
        "a corpus below the ANN threshold needs no ANN pin"
    );
}

#[test]
fn vector_lease_observer_does_not_create_a_missing_database() {
    let root = TempDir::new().expect("cache root");
    let path = root
        .path()
        .join(docsbase_memory::vector_cache::VECTOR_DB_FILE);
    let error = lease_count(root.path(), "missing-model").expect_err("missing database");
    assert!(matches!(
        &error,
        docsbase_memory::error::Error::Internal { .. }
    ));
    assert!(std::error::Error::source(&error).is_some());
    assert!(error.to_string().contains("open vector lease database"));
    assert!(error.to_string().contains(&path.display().to_string()));
    assert!(!path.exists());
    assert_eq!(
        fs::read_dir(root.path()).expect("cache contents").count(),
        0
    );
}

#[test]
fn vector_lease_observer_counts_model_rows_without_changing_them() {
    let root = TempDir::new().expect("cache root");
    let model = "quoted' OR 1=1 --";
    // Real cache writes leave committed leases in the active WAL while the
    // independent observer reads. Expire one row only after pin's purge step.
    let mut cache = VectorCache::open(root.path()).expect("seed cache");
    cache
        .pin(
            1,
            model,
            &["expired".into(), "active".into()],
            std::time::Duration::from_secs(3600),
        )
        .expect("seed model leases");
    cache
        .pin(
            2,
            "other-model",
            &["other".into()],
            std::time::Duration::from_secs(3600),
        )
        .expect("seed independent model");
    let conn = rusqlite::Connection::open(
        root.path()
            .join(docsbase_memory::vector_cache::VECTOR_DB_FILE),
    )
    .expect("fixture writer");
    conn.execute(
        "UPDATE vector_cache_leases SET expires_at = 0 WHERE model_id = ?1 AND chunk_hash = ?2",
        rusqlite::params![model, "expired"],
    )
    .expect("expire fixture row");
    let rows = || {
        let mut statement = conn
            .prepare(
                "SELECT lease_id, model_id, chunk_hash, expires_at FROM vector_cache_leases
                 ORDER BY lease_id, model_id, chunk_hash",
            )
            .expect("lease snapshot");
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .expect("read snapshot")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("snapshot rows")
    };
    let before = rows();
    let version: i64 = conn
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .expect("data version");
    assert_eq!(lease_count(root.path(), model).expect("quoted model"), 2);
    assert_eq!(
        lease_count(root.path(), "other-model").expect("independent model"),
        1
    );
    assert_eq!(lease_count(root.path(), "absent").expect("absent model"), 0);
    assert_eq!(rows(), before, "observation must preserve expired rows too");
    assert_eq!(
        conn.pragma_query_value::<i64, _>(None, "data_version", |row| row.get(0))
            .expect("data version after reads"),
        version,
        "the observer must not commit any writes"
    );
}
