//! Construction-owned ANN lifetimes against real metadata, leases and status.

#[path = "support/vector_leases.rs"]
mod vector_leases;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use docsbase_memory::config::Config;
use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::Resolution;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::daemon::{registry, session::Stats, tools};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache};
use docsbase_memory::hybrid::dense_settings::{AnnLifetimes, DensePatch, DenseSpec, standalone};
use docsbase_memory::hybrid::jobs::embed_project_chunks_with_dense_policy;
use docsbase_memory::hybrid::meta::AnnState;
use docsbase_memory::store::Db;
use docsbase_memory::vector_cache::{CacheLimits, Embedder, VectorCache};
use vector_leases::lease_count;

#[test]
fn lifetimes_reject_unrepresentable_or_immediately_expired_leases() {
    for (pin, outcome) in [
        (Duration::ZERO, Duration::from_secs(1)),
        (Duration::from_nanos(1), Duration::from_secs(1)),
        (Duration::from_millis(1500), Duration::from_secs(1)),
        (Duration::from_secs(u64::MAX), Duration::from_secs(1)),
        (Duration::from_secs(1), Duration::ZERO),
    ] {
        assert!(matches!(
            AnnLifetimes::new(pin, outcome),
            Err(Error::Admission { .. })
        ));
    }
    let valid = AnnLifetimes::new(Duration::from_secs(1), Duration::from_nanos(1)).unwrap();
    assert_eq!(valid.backfill_pin_ttl(), Duration::from_secs(1));
    assert_eq!(valid.outcome_ttl(), Duration::from_nanos(1));
}

#[test]
fn lifetime_overlay_preserves_origins_scope_and_last_valid_policy() {
    let context = DefaultsContext::standalone();
    let mut resolution = Resolution::<DenseSpec>::new(&context);
    let global = SourceId::Global("global.toml".into());
    resolution
        .apply(
            &global,
            DensePatch {
                backfill_pin_ttl: Some(Duration::from_secs(600)),
                ..DensePatch::default()
            },
        )
        .unwrap();
    resolution
        .apply(
            &SourceId::Cli,
            DensePatch {
                ann_outcome_ttl: Some(Duration::from_secs(30)),
                ..DensePatch::default()
            },
        )
        .unwrap();
    for (source, pin) in [
        (SourceId::Cli, Duration::ZERO),
        (
            SourceId::Project("project.toml".into()),
            Duration::from_secs(120),
        ),
    ] {
        assert!(
            resolution
                .apply(
                    &source,
                    DensePatch {
                        max_bytes: Some(1),
                        backfill_pin_ttl: Some(pin),
                        ..DensePatch::default()
                    },
                )
                .is_err()
        );
    }
    let resolved = resolution.finish().unwrap();
    assert_eq!(resolved.settings().max_bytes, 1536 * 1024 * 1024);
    assert_eq!(
        resolved.settings().ann_lifetimes.backfill_pin_ttl(),
        Duration::from_secs(600)
    );
    assert_eq!(
        resolved.settings().ann_lifetimes.outcome_ttl(),
        Duration::from_secs(30)
    );
    assert_eq!(
        resolved
            .provenance()
            .get("dense.backfill_pin_ttl")
            .unwrap()
            .source,
        global
    );
    assert_eq!(
        resolved
            .provenance()
            .get("dense.ann_outcome_ttl")
            .unwrap()
            .source,
        SourceId::Cli
    );
}

fn indexed(cache: &Path) -> (tempfile::TempDir, Db, i64) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("one.md"), "alpha lease body").unwrap();
    let mut db = Db::open(cache).unwrap();
    let project = registry::ensure_project(&mut db, &root.path().canonicalize().unwrap()).unwrap();
    tools::run_project_index(&mut db, cache, &project, &Config::default()).unwrap();
    (root, db, project.id)
}

fn seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

struct LeaseProbe {
    root: PathBuf,
    project_id: i64,
    before: i64,
    expected_ttl: i64,
    fail: bool,
}
impl Embedder for LeaseProbe {
    fn model_id(&self) -> &'static str {
        "lifetime-probe@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let conn = rusqlite::Connection::open(self.root.join("vectors.db")).unwrap();
        let expires: i64 = conn
            .query_row(
                "SELECT expires_at FROM vector_cache_leases WHERE lease_id=?1 AND model_id=?2",
                rusqlite::params![self.project_id, self.model_id()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            expires >= self.before + self.expected_ttl && expires <= seconds() + self.expected_ttl
        );
        // Simulate time consumed by embedding without sleeping or replacing the
        // production clock. Success must renew this shortened owned lease.
        conn.execute(
            "UPDATE vector_cache_leases SET expires_at=?3 WHERE lease_id=?1 AND model_id=?2",
            rusqlite::params![self.project_id, self.model_id(), seconds() + 120],
        )
        .unwrap();
        if self.fail {
            return Err(Error::internal("controlled embedding failure"));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

fn backfill(fail: bool, pin_ttl: Option<Duration>) {
    let cache = tempfile::tempdir().unwrap();
    let (_root, db, id) = indexed(cache.path());
    let mut settings = standalone().unwrap().into_settings();
    settings.min_vectors = 1;
    if let Some(pin_ttl) = pin_ttl {
        settings.ann_lifetimes = AnnLifetimes::new(pin_ttl, Duration::from_secs(30)).unwrap();
    }
    let expected_ttl = pin_ttl.map_or(3600, |ttl| i64::try_from(ttl.as_secs()).unwrap());
    let dense = DenseCache::with_settings(AnnPolicy::BuildOrLoad, cache.path(), &settings).unwrap();
    let mut vectors = VectorCache::open(cache.path()).unwrap();
    vectors
        .pin(
            id,
            "unrelated",
            &["other-hash".into()],
            Duration::from_secs(3600),
        )
        .unwrap();
    vectors
        .pin(
            999,
            "lifetime-probe@d2@l2",
            &["other-project-hash".into()],
            Duration::from_secs(3600),
        )
        .unwrap();
    let before = seconds();
    let probe = LeaseProbe {
        root: cache.path().into(),
        project_id: id,
        before,
        expected_ttl,
        fail,
    };
    let result = embed_project_chunks_with_dense_policy(
        &db,
        &probe,
        id,
        CacheLimits::default(),
        true,
        &dense.limits(),
    );
    assert_eq!(result.is_err(), fail);
    let conn = rusqlite::Connection::open(cache.path().join("vectors.db")).unwrap();
    let expires: Option<i64> = conn
        .query_row(
            "SELECT MAX(expires_at) FROM vector_cache_leases WHERE lease_id=?1 AND model_id=?2",
            rusqlite::params![id, probe.model_id()],
            |row| row.get(0),
        )
        .unwrap();
    if fail {
        assert_eq!(expires, None);
    } else {
        let expires = expires.unwrap();
        assert!(expires >= before + expected_ttl && expires <= seconds() + expected_ttl);
    }
    assert_eq!(
        lease_count(cache.path(), probe.model_id()).unwrap(),
        if fail { 1 } else { 2 }
    );
    assert_eq!(lease_count(cache.path(), "unrelated").unwrap(), 1);
}

#[test]
fn backfill_acquires_and_renews_the_supplied_lease_duration() {
    backfill(false, Some(Duration::from_secs(600)));
}

#[test]
fn failed_backfill_keeps_unrelated_owners_leases() {
    backfill(true, Some(Duration::from_secs(600)));
}

#[test]
fn omitted_lifetimes_preserve_hourly_backfill_leases() {
    backfill(false, None);
}

#[test]
fn actual_status_uses_cache_lifetime_without_changing_default_control() {
    let cache = tempfile::tempdir().unwrap();
    let (_root, db, id) = indexed(cache.path());
    let mut settings = standalone().unwrap().into_settings();
    settings.ann_lifetimes =
        AnnLifetimes::new(Duration::from_secs(600), Duration::from_nanos(1)).unwrap();
    let dense = DenseCache::with_settings(AnnPolicy::Disabled, cache.path(), &settings).unwrap();
    let control = DenseCache::with_policy(AnnPolicy::Disabled, cache.path());
    dense.record_outcome(id, AnnState::Ready, None);
    control.record_outcome(id, AnnState::Ready, None);
    let at = dense.outcome(id).unwrap().at;
    while at.elapsed() <= Duration::from_nanos(1) {
        std::thread::yield_now();
    }
    for (cache, expected) in [(&dense, "stale"), (&control, "ready")] {
        let status = tools::status(
            &db,
            &Stats::default(),
            0,
            None,
            &|_| false,
            &|_| None,
            Some(cache),
        )
        .unwrap();
        assert_eq!(status["projects"][0]["ann"]["state"], expected);
        if expected == "stale" {
            assert_eq!(
                status["projects"][0]["ann"]["reason"],
                "last search is older than configured TTL"
            );
        }
    }
}
