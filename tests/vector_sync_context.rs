//! Prepared sync contexts must reach both changed chunks and offline backfill.
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::dense::DenseCacheLimits;
use docsbase_memory::hybrid::{EmbeddingCfg, HybridStages, Ranking, jobs};
use docsbase_memory::store::models::{Project, ProjectStatus, SyncState};
use docsbase_memory::store::{Db, repo};
use docsbase_memory::vector_cache::{Embedder, VectorCacheSource, VectorStore};

#[derive(Default)]
struct Provider {
    calls: AtomicUsize,
    fail: AtomicBool,
}
impl Embedder for Provider {
    fn model_id(&self) -> &'static str {
        "openrouter/model@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::internal("controlled sync inference failure"));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

struct Fixture {
    db: Db,
    source: VectorCacheSource,
    provider: Arc<Provider>,
    stages: HybridStages,
    project: Project,
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
}
impl Fixture {
    fn new(offline_seed: bool) -> Self {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.md"), "alphawidget").unwrap();
        fs::write(root.path().join("b.md"), "betawidget").unwrap();
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        if offline_seed {
            tools::run_project_index(&mut db, cache.path(), &project, &Config::default()).unwrap();
        }
        let settings = docsbase_memory::vector_cache::settings::standalone().unwrap();
        let source = VectorCacheSource::Owned(
            VectorStore::acquire(cache.path(), settings.settings()).unwrap(),
        );
        let provider = Arc::new(Provider::default());
        let stages = HybridStages {
            embedder: Some(provider.clone()),
            ranking: Ranking::Rrf,
            ..HybridStages::default()
        };
        Self {
            db,
            source,
            provider,
            stages,
            project,
            root,
            cache,
        }
    }
    fn enqueue(&self) -> i64 {
        let (id, created) = tools::enqueue_sync(&self.db, &self.project).unwrap();
        assert!(created);
        id
    }
    fn sync(&mut self, id: i64) -> Result<docsbase_memory::index::job::JobStats> {
        tools::run_sync_job_in_with_vectors(
            &mut self.db,
            self.cache.path(),
            id,
            &self.project,
            &Config::default(),
            &DenseCacheLimits::default(),
            &self.stages,
            &self.source,
        )
    }
    fn coverage(&self) -> (u64, u64) {
        let result = jobs::coverage_for_key_from(
            &self.db,
            self.provider.model_id(),
            self.project.id,
            &self.source,
        )
        .unwrap();
        (result.cached, result.total)
    }
}

#[test]
fn prepared_project_index_persists_status_and_owned_vectors() {
    let mut env = Fixture::new(false);
    let stats = tools::run_project_index_with_vectors(
        &mut env.db,
        env.cache.path(),
        &env.project,
        &Config::default(),
        &env.stages,
        &env.source,
    )
    .unwrap();
    assert_eq!(stats.docs, 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(
        registry::project_by_id(&env.db, env.project.id)
            .unwrap()
            .unwrap()
            .status,
        ProjectStatus::Indexed
    );
    fs::write(env.root.path().join("a.md"), "gammawidget").unwrap();
    let stats = tools::run_project_index_with_vectors(
        &mut env.db,
        env.cache.path(),
        &env.project,
        &Config::default(),
        &env.stages,
        &env.source,
    )
    .unwrap();
    assert_eq!((stats.docs, stats.skipped), (1, 1));
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.provider.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn queued_sync_backfills_unchanged_offline_chunks_through_owner() {
    let mut env = Fixture::new(true);
    assert_eq!(env.coverage(), (0, 2));
    let id = env.enqueue();
    assert_eq!(env.sync(id).unwrap().skipped, 2);
    assert_eq!(env.coverage(), (2, 2));
    let job = repo::sync_job(env.db.connection(), id).unwrap().unwrap();
    assert_eq!(job.state, SyncState::Done);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(job.stats_json.as_deref().unwrap()).unwrap()["skipped"],
        2
    );
    let again = env.enqueue();
    env.sync(again).unwrap();
    assert_eq!(env.provider.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn child_sync_connection_keeps_prepared_context() {
    let env = Fixture::new(true);
    fs::write(env.root.path().join("a.md"), "gammawidget").unwrap();
    let id = env.enqueue();
    let project = tools::execute_sync_job_with_vectors(
        env.cache.path(),
        id,
        env.project.id,
        &Config::default(),
        env.db.settings(),
        &DenseCacheLimits::default(),
        &env.stages,
        &env.source,
    )
    .unwrap();
    assert_eq!(project.status, ProjectStatus::Indexed);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(
        repo::sync_job(env.db.connection(), id)
            .unwrap()
            .unwrap()
            .state,
        SyncState::Done
    );
    assert_eq!(
        repo::index_generation(env.db.connection(), env.project.id).unwrap(),
        2
    );
}

#[test]
fn failed_sync_records_error_and_retry_embeds_same_bytes() {
    let mut env = Fixture::new(false);
    env.provider.fail.store(true, Ordering::SeqCst);
    let id = env.enqueue();
    assert!(
        env.sync(id)
            .unwrap_err()
            .to_string()
            .contains("controlled sync inference failure")
    );
    assert_eq!(
        repo::sync_job(env.db.connection(), id)
            .unwrap()
            .unwrap()
            .state,
        SyncState::Error
    );
    assert_eq!(
        repo::index_generation(env.db.connection(), env.project.id).unwrap(),
        0
    );
    let states = repo::doc_states(env.db.connection(), env.project.id).unwrap();
    assert_eq!(
        states.len(),
        2,
        "both failed documents retain retry markers"
    );
    assert!(states.iter().all(|state| state.content_hash.is_empty()));
    env.provider.fail.store(false, Ordering::SeqCst);
    let retry = env.enqueue();
    assert_eq!(env.sync(retry).unwrap().docs, 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(
        repo::sync_job(env.db.connection(), retry)
            .unwrap()
            .unwrap()
            .state,
        SyncState::Done
    );
}

#[test]
fn wrong_source_preserves_queued_job_and_project_before_index_open() {
    let mut env = Fixture::new(false);
    let other = tempfile::tempdir().unwrap();
    let settings = docsbase_memory::vector_cache::settings::standalone().unwrap();
    let source =
        VectorCacheSource::Owned(VectorStore::acquire(other.path(), settings.settings()).unwrap());
    let id = env.enqueue();
    assert!(matches!(
        tools::run_sync_job_in_with_vectors(
            &mut env.db,
            env.cache.path(),
            id,
            &env.project,
            &Config::default(),
            &DenseCacheLimits::default(),
            &env.stages,
            &source,
        ),
        Err(Error::Admission { .. })
    ));
    assert_eq!(
        repo::sync_job(env.db.connection(), id)
            .unwrap()
            .unwrap()
            .state,
        SyncState::Queued
    );
    assert_eq!(
        registry::project_by_id(&env.db, env.project.id)
            .unwrap()
            .unwrap()
            .status,
        ProjectStatus::NotIndexed
    );
    assert!(
        !env.cache
            .path()
            .join(format!("projects/{}/tantivy", env.project.id))
            .exists()
    );
    assert_eq!(env.provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn configured_coverage_uses_owner_and_canonical_model_without_building_provider() {
    let mut env = Fixture::new(false);
    let id = env.enqueue();
    env.sync(id).unwrap();
    let mut config = Config::default();
    config.hybrid_cfg.enabled = true;
    config.hybrid_cfg.embedding = Some(EmbeddingCfg {
        model: "model".into(),
        dimensions: 2,
        ..EmbeddingCfg::default()
    });
    let coverage = jobs::coverage_from(&env.db, &config.hybrid_cfg, env.project.id, &env.source)
        .unwrap()
        .unwrap();
    assert_eq!((coverage.cached, coverage.total), (2, 2));
    assert_eq!(env.provider.calls.load(Ordering::SeqCst), 1);
    assert!(
        jobs::coverage_from(
            &env.db,
            &Config::default().hybrid_cfg,
            env.project.id,
            &env.source
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn mismatched_index_cache_and_child_root_are_rejected_without_files() {
    let mut env = Fixture::new(false);
    let other = tempfile::tempdir().unwrap();
    let missing = other.path().join("must-not-create");
    assert!(matches!(
        tools::run_project_index_with_vectors(
            &mut env.db,
            &missing,
            &env.project,
            &Config::default(),
            &env.stages,
            &env.source,
        ),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        tools::execute_sync_job_with_vectors(
            &missing,
            99,
            env.project.id,
            &Config::default(),
            env.db.settings(),
            &DenseCacheLimits::default(),
            &env.stages,
            &env.source,
        ),
        Err(Error::Admission { .. })
    ));
    assert!(!missing.exists());
    assert_eq!(
        registry::project_by_id(&env.db, env.project.id)
            .unwrap()
            .unwrap()
            .status,
        ProjectStatus::NotIndexed
    );
    assert_eq!(env.provider.calls.load(Ordering::SeqCst), 0);
}
