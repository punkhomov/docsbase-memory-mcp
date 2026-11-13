//! Actual dense policy and ANN backfill leases (`CF6b`), without provider I/O.

#[path = "support/vector_leases.rs"]
mod vector_leases;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::schema::SettingsSpec;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
use docsbase_memory::hybrid::dense_settings::{DenseSettings, DenseSpec, standalone};
use docsbase_memory::hybrid::jobs::{
    embed_project_chunks_with, embed_project_chunks_with_dense_policy,
};
use docsbase_memory::store::Db;
use docsbase_memory::vector_cache::{CacheLimits, Embedder};
use vector_leases::lease_count;

fn policy() -> DenseSettings {
    standalone().unwrap().into_settings()
}

fn indexed(root: &Path) -> (tempfile::TempDir, Db, i64) {
    let project_root = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("a.md", "alpha unique body"),
        ("b.md", "beta unique body"),
        ("c.md", "gamma unique body"),
        ("d.md", "identical duplicate body"),
        ("e.md", "identical duplicate body"),
    ] {
        std::fs::write(project_root.path().join(name), text).unwrap();
    }
    let mut db = Db::open(root).unwrap();
    let project =
        registry::ensure_project(&mut db, &project_root.path().canonicalize().unwrap()).unwrap();
    tools::run_project_index(&mut db, root, &project, &Config::default()).unwrap();
    (project_root, db, project.id)
}

struct LeaseProbe {
    root: PathBuf,
    during: AtomicU64,
    gate: Option<std::sync::Arc<std::sync::Barrier>>,
}
impl LeaseProbe {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            during: AtomicU64::new(u64::MAX),
            gate: None,
        }
    }
}
impl Embedder for LeaseProbe {
    fn model_id(&self) -> &'static str {
        "settings-probe@d3@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        assert!(docsbase_memory::hybrid::blocking::in_context());
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        self.during
            .store(lease_count(&self.root, self.model_id())?, Ordering::SeqCst);
        Ok(texts.iter().map(|_| vec![1., 0., 0.]).collect())
    }
}

#[test]
fn dense_defaults_project_the_existing_owner_and_preserve_zero_policies() {
    let settings = policy();
    assert_eq!(
        settings,
        DenseSpec::defaults(&DefaultsContext::standalone())
    );
    assert_eq!(
        DenseCacheLimits::from(&settings),
        DenseCacheLimits::default()
    );
    let root = tempfile::tempdir().unwrap();
    let zero = DenseSettings {
        max_vectors: 0,
        max_bytes: 0,
        max_projects: 0,
        min_vectors: 0,
        build_backoff: std::time::Duration::ZERO,
        max_ann_disk_bytes: 0,
        ..settings
    };
    let dense = DenseCache::with_settings(AnnPolicy::Disabled, root.path(), &zero).unwrap();
    assert_eq!(dense.limits(), DenseCacheLimits::from(&zero));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn supplied_vector_and_byte_caps_choose_actual_streaming_fallback() {
    let root = tempfile::tempdir().unwrap();
    let (_project, db, id) = indexed(root.path());
    embed_project_chunks_with(&db, &LeaseProbe::new(root.path()), id).unwrap();
    for settings in [
        DenseSettings {
            max_vectors: 4,
            ..policy()
        },
        DenseSettings {
            max_bytes: 59,
            ..policy()
        },
    ] {
        let dense = DenseCache::with_settings(AnnPolicy::Disabled, root.path(), &settings).unwrap();
        assert!(matches!(
            dense.get(&db, id, "settings-probe@d3@l2").unwrap(),
            DenseRows::TooLarge(_)
        ));
    }
}

#[test]
fn explicit_daemon_policy_reaches_dense_and_query_resources() {
    let root = tempfile::tempdir().unwrap();
    let (_project, db, id) = indexed(root.path());
    let settings = DenseSettings {
        max_vectors: 4,
        min_vectors: 1,
        query_cache_max_entries: 1,
        ann_lifetimes: docsbase_memory::hybrid::dense_settings::AnnLifetimes::new(
            std::time::Duration::from_secs(120),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
        ..policy()
    };
    let shared = docsbase_memory::daemon::server::Shared::open_with_dense_policy(
        root.path(),
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        &settings,
    )
    .unwrap();
    assert_eq!(shared.dense.limits().ann_lifetimes, settings.ann_lifetimes);
    assert!(matches!(
        shared.dense.get(&db, id, "settings-probe@d3@l2").unwrap(),
        DenseRows::TooLarge(_)
    ));
    let probe = LeaseProbe::new(root.path());
    embed_project_chunks_with_dense_policy(
        &db,
        &probe,
        id,
        CacheLimits::default(),
        true,
        &shared.dense.limits(),
    )
    .unwrap();
    assert_eq!(probe.during.load(Ordering::SeqCst), 0);
    shared.dense.store_query_vector("model", "a", &[1.]);
    shared.dense.store_query_vector("model", "b", &[2.]);
    assert!(shared.dense.query_vector("model", "a").is_none());
    assert!(shared.dense.query_vector("model", "b").is_some());
}

#[test]
fn static_ann_ineligibility_never_takes_pins_during_real_embedding() {
    for limits in [
        DenseCacheLimits {
            max_bytes: 4_500,
            min_vectors: 1,
            ..DenseCacheLimits::default()
        },
        DenseCacheLimits {
            max_vectors: 4,
            min_vectors: 1,
            ..DenseCacheLimits::default()
        },
        DenseCacheLimits {
            min_vectors: 6,
            ..DenseCacheLimits::default()
        },
        DenseCacheLimits {
            max_projects: 0,
            min_vectors: 1,
            ..DenseCacheLimits::default()
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let (_project, db, id) = indexed(root.path());
        let probe = LeaseProbe::new(root.path());
        embed_project_chunks_with_dense_policy(
            &db,
            &probe,
            id,
            CacheLimits::default(),
            true,
            &limits,
        )
        .unwrap();
        assert_eq!(probe.during.load(Ordering::SeqCst), 0, "{limits:?}");
        assert_eq!(lease_count(root.path(), probe.model_id()).unwrap(), 0);
    }
}

#[test]
fn eligible_duplicate_chunk_rows_keep_their_pins_after_embedding() {
    let root = tempfile::tempdir().unwrap();
    let (_project, db, id) = indexed(root.path());
    let probe = LeaseProbe::new(root.path());
    let limits = DenseCacheLimits {
        max_bytes: 6_000,
        max_vectors: 5,
        min_vectors: 5,
        ..DenseCacheLimits::default()
    };
    embed_project_chunks_with_dense_policy(&db, &probe, id, CacheLimits::default(), true, &limits)
        .unwrap();
    assert_eq!(
        probe.during.load(Ordering::SeqCst),
        4,
        "five rows include four distinct hashes"
    );
    assert_eq!(
        lease_count(root.path(), probe.model_id()).unwrap(),
        4,
        "renew by chunk rows, not distinct hashes"
    );
}

#[test]
fn supplied_project_capacity_evicts_the_actual_resident_view() {
    let root = tempfile::tempdir().unwrap();
    let (_first, db, first) = indexed(root.path());
    let (_second, _db2, second) = indexed(root.path());
    let probe = LeaseProbe::new(root.path());
    embed_project_chunks_with(&db, &probe, first).unwrap();
    embed_project_chunks_with(&db, &probe, second).unwrap();
    let settings = DenseSettings {
        max_projects: 1,
        ..policy()
    };
    let dense = DenseCache::with_settings(AnnPolicy::Disabled, root.path(), &settings).unwrap();
    let ready = |id| match dense.get(&db, id, probe.model_id()).unwrap() {
        DenseRows::Ready(index) => index,
        other => panic!("expected resident exact view: {other:?}"),
    };
    let first_view = ready(first);
    assert!(std::sync::Arc::ptr_eq(&first_view, &ready(first)));
    let _second_view = ready(second);
    assert!(!std::sync::Arc::ptr_eq(&first_view, &ready(first)));
}

#[test]
fn parallel_backfills_use_their_own_supplied_policy() {
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [4_500, 6_000]
        .into_iter()
        .map(|max_bytes| {
            let gate = std::sync::Arc::clone(&gate);
            std::thread::spawn(move || {
                let root = tempfile::tempdir().unwrap();
                let (_project, db, id) = indexed(root.path());
                let mut probe = LeaseProbe::new(root.path());
                probe.gate = Some(gate);
                let limits = DenseCacheLimits {
                    max_bytes,
                    min_vectors: 1,
                    ..DenseCacheLimits::default()
                };
                embed_project_chunks_with_dense_policy(
                    &db,
                    &probe,
                    id,
                    CacheLimits::default(),
                    true,
                    &limits,
                )
                .unwrap();
                probe.during.load(Ordering::SeqCst)
            })
        })
        .collect();
    let seen: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(seen, [0, 4]);
}

#[test]
fn invalid_complete_policy_is_rejected_before_resource_creation() {
    let root = tempfile::tempdir().unwrap();
    let absent = root.path().join("absent");
    let invalid = DenseSettings {
        query_cache_max_bytes: 0,
        ..policy()
    };
    let db = docsbase_memory::store::settings::standalone().unwrap();
    let readers = docsbase_memory::store::settings::standalone_read_pool().unwrap();
    assert!(DenseCache::with_settings(AnnPolicy::BuildOrLoad, &absent, &invalid).is_err());
    assert!(
        docsbase_memory::daemon::server::Shared::open_with_dense_policy(
            &absent, &db, &readers, &invalid
        )
        .is_err()
    );
    assert!(
        docsbase_memory::daemon::lifecycle::run_daemon_with_dense_policy(
            &absent,
            std::time::Duration::ZERO,
            &db,
            &readers,
            &invalid
        )
        .is_err()
    );
    assert!(!absent.exists());
}

#[test]
fn ann_eligibility_rejects_unrepresentable_peak_even_with_maximum_byte_cap() {
    let limits = DenseCacheLimits {
        max_vectors: usize::MAX,
        max_bytes: u64::MAX,
        min_vectors: 1,
        ..DenseCacheLimits::default()
    };
    let width_boundary = (usize::MAX - 1023) / 4;
    assert!(!docsbase_memory::hybrid::dense::ann_build_admitted(
        1,
        width_boundary,
        u64::MAX
    ));
    assert!(!limits.ann_build_eligible(1, width_boundary));
    assert!(!limits.ann_build_eligible_for_model(1, &format!("huge@d{width_boundary}")));
    assert!(!limits.ann_build_eligible(usize::MAX, 3));
    assert!(!limits.ann_build_eligible_for_model(usize::MAX, "huge@d3"));
    assert!(!limits.ann_build_eligible_for_model(5, "missing-width"));
    assert!(!limits.ann_build_eligible(5, 0));
}
