//! Explicit embedding at the real full/incremental index boundary.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::{Ranking, jobs};
use docsbase_memory::index::job::{
    IndexEmbedding, run_full_with_embedding, run_incremental_with_embedding,
};
use docsbase_memory::index::tantivy_index::{IndexHandle, REBUILD_MARKER, TOKENIZER_VERSION_FILE};
use docsbase_memory::store::models::Project;
use docsbase_memory::store::{DB_FILE, Db, repo};
use docsbase_memory::vector_cache::{Embedder, VectorCacheSource, VectorStore};

struct Probe {
    root: PathBuf,
    calls: AtomicUsize,
    fail: AtomicBool,
}
impl Embedder for Probe {
    fn model_id(&self) -> &'static str {
        "index-probe@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Both marker and vector transactions must end before inference.
        for file in [DB_FILE, "vectors.db"] {
            let connection = rusqlite::Connection::open(self.root.join(file)).unwrap();
            connection.busy_timeout(std::time::Duration::ZERO).unwrap();
            connection
                .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
                .unwrap();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::internal("controlled embedding failure"));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

struct Fixture {
    db: Db,
    index: IndexHandle,
    source: VectorCacheSource,
    probe: Probe,
    project: Project,
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for (file, body) in [("a.md", "alphawidget"), ("b.md", "betawidget")] {
            fs::write(root.path().join(file), body).unwrap();
        }
        let mut db = Db::open(cache.path()).unwrap();
        let project = registry::ensure_project(&mut db, root.path()).unwrap();
        let index = IndexHandle::open_or_create(
            &cache
                .path()
                .join(format!("projects/{}/tantivy", project.id)),
        )
        .unwrap();
        let settings = docsbase_memory::vector_cache::settings::standalone().unwrap();
        let source = VectorCacheSource::Owned(
            VectorStore::acquire(cache.path(), settings.settings()).unwrap(),
        );
        let probe = Probe {
            root: cache.path().to_owned(),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        };
        Self {
            db,
            index,
            source,
            probe,
            project,
            root,
            cache,
        }
    }
    fn full(&mut self) -> Result<docsbase_memory::index::job::JobStats> {
        let embedding = IndexEmbedding::new(&self.source, Some(&self.probe));
        run_full_with_embedding(
            &mut self.db,
            &mut self.index,
            &self.project,
            &Config::default(),
            &embedding,
        )
    }
    fn incremental(&mut self, paths: &[PathBuf]) -> Result<docsbase_memory::index::job::JobStats> {
        let embedding = IndexEmbedding::new(&self.source, Some(&self.probe));
        run_incremental_with_embedding(
            &mut self.db,
            &mut self.index,
            &self.project,
            paths,
            &Config::default(),
            &embedding,
        )
    }
    fn generation(&self) -> i64 {
        repo::index_generation(self.db.connection(), self.project.id).unwrap()
    }
    fn coverage(&self) -> (u64, u64) {
        let coverage = jobs::coverage_for_key_from(
            &self.db,
            self.probe.model_id(),
            self.project.id,
            &self.source,
        )
        .unwrap();
        (coverage.cached, coverage.total)
    }
    fn recreate(mut self) -> Self {
        let path = self
            .cache
            .path()
            .join(format!("projects/{}/tantivy", self.project.id));
        drop(self.index);
        fs::write(path.join(TOKENIZER_VERSION_FILE), b"1\n").unwrap();
        self.index = IndexHandle::open_or_create(&path).unwrap();
        assert!(self.index.was_recreated());
        assert!(path.join(REBUILD_MARKER).exists());
        self
    }
}

#[test]
fn full_and_incremental_use_one_owner_and_skip_unchanged_chunks() {
    let mut env = Fixture::new();
    assert_eq!(env.full().unwrap().docs, 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.generation(), 1);
    assert_eq!(env.full().unwrap().skipped, 2);
    assert_eq!(env.generation(), 1);
    let path = env.root.path().join("a.md");
    fs::write(&path, "gammawidget").unwrap();
    assert_eq!(
        env.incremental(std::slice::from_ref(&path)).unwrap().docs,
        1
    );
    assert_eq!(env.generation(), 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.index.search("gammawidget", 10).unwrap().len(), 1);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 0);
    assert_eq!(env.incremental(&[path]).unwrap().skipped, 1);
    assert_eq!(env.probe.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_full_embedding_preserves_markers_for_same_content_retry() {
    let mut env = Fixture::new();
    env.probe.fail.store(true, Ordering::SeqCst);
    assert!(
        env.full()
            .unwrap_err()
            .to_string()
            .contains("controlled embedding failure")
    );
    assert_eq!(env.generation(), 0);
    let states = repo::doc_states(env.db.connection(), env.project.id).unwrap();
    assert_eq!(states.len(), 2);
    assert!(states.iter().all(|state| state.content_hash.is_empty()));
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 0);
    env.probe.fail.store(false, Ordering::SeqCst);
    assert_eq!(env.full().unwrap().docs, 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.generation(), 1);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 1);
}

#[test]
fn failed_incremental_embedding_retries_without_publishing_generation() {
    let mut env = Fixture::new();
    env.full().unwrap();
    let path = env.root.path().join("a.md");
    fs::write(&path, "gammawidget").unwrap();
    env.probe.fail.store(true, Ordering::SeqCst);
    assert!(env.incremental(std::slice::from_ref(&path)).is_err());
    assert_eq!(env.generation(), 1);
    assert!(
        repo::doc_states(env.db.connection(), env.project.id)
            .unwrap()
            .iter()
            .any(|state| state.content_hash.is_empty())
    );
    assert_eq!(env.index.search("gammawidget", 10).unwrap().len(), 0);
    env.probe.fail.store(false, Ordering::SeqCst);
    assert_eq!(env.incremental(&[path]).unwrap().docs, 1);
    assert_eq!(env.generation(), 2);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.index.search("gammawidget", 10).unwrap().len(), 1);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 0);
}

#[test]
fn recreated_incremental_rebuilds_all_docs_through_same_context() {
    let mut env = Fixture::new();
    env.full().unwrap();
    env = env.recreate();
    fs::write(env.root.path().join("b.md"), "gammawidget").unwrap();
    let path = env.root.path().join("a.md");
    assert_eq!(env.incremental(&[path]).unwrap().docs, 2);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 1);
    assert_eq!(env.index.search("gammawidget", 10).unwrap().len(), 1);
    assert_eq!(env.coverage(), (2, 2));
    assert_eq!(env.probe.calls.load(Ordering::SeqCst), 2);
    assert!(
        !env.cache
            .path()
            .join(format!("projects/{}/tantivy", env.project.id))
            .join(REBUILD_MARKER)
            .exists()
    );
}

#[test]
fn wrong_owner_rejects_before_full_or_empty_incremental_state_changes() {
    let mut env = Fixture::new();
    let other = tempfile::tempdir().unwrap();
    let settings = docsbase_memory::vector_cache::settings::standalone().unwrap();
    let source =
        VectorCacheSource::Owned(VectorStore::acquire(other.path(), settings.settings()).unwrap());
    let enabled = IndexEmbedding::new(&source, Some(&env.probe));
    assert!(matches!(
        run_full_with_embedding(
            &mut env.db,
            &mut env.index,
            &env.project,
            &Config::default(),
            &enabled
        ),
        Err(Error::Admission { .. })
    ));
    let disabled = IndexEmbedding::new(&source, None);
    assert!(matches!(
        run_incremental_with_embedding(
            &mut env.db,
            &mut env.index,
            &env.project,
            &[],
            &Config::default(),
            &disabled
        ),
        Err(Error::Admission { .. })
    ));
    assert_eq!(
        repo::doc_states(env.db.connection(), env.project.id)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(env.generation(), 0);
    assert_eq!(env.probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 0);
}

#[test]
fn prepared_lexical_context_does_not_resolve_configured_providers() {
    let mut env = Fixture::new();
    let mut config = Config::default();
    config.hybrid_cfg.enabled = true;
    config.hybrid_cfg.ranking = Ranking::Rrf;
    // The composition root supplies the embedding decision explicitly; the
    // typed context must never silently resolve another provider from Config.
    assert!(docsbase_memory::hybrid::from_config(&config.hybrid_cfg, &config.providers).is_err());
    let embedding = IndexEmbedding::new(&env.source, None);
    let stats = run_full_with_embedding(
        &mut env.db,
        &mut env.index,
        &env.project,
        &config,
        &embedding,
    )
    .unwrap();
    assert_eq!(stats.docs, 2);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 1);
    assert_eq!(env.coverage(), (0, 2));
    assert_eq!(env.probe.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn owner_admission_precedes_unusable_registry_reads() {
    let mut env = Fixture::new();
    let other = tempfile::tempdir().unwrap();
    let mut foreign = Db::open(other.path()).unwrap();
    // If admission is delayed until embedding, metadata reads fail first.
    foreign
        .connection()
        .execute_batch("DROP TABLE docs;")
        .unwrap();
    let embedding = IndexEmbedding::new(&env.source, Some(&env.probe));
    assert!(matches!(
        run_full_with_embedding(
            &mut foreign,
            &mut env.index,
            &env.project,
            &Config::default(),
            &embedding
        ),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        run_incremental_with_embedding(
            &mut foreign,
            &mut env.index,
            &env.project,
            &[],
            &Config::default(),
            &embedding
        ),
        Err(Error::Admission { .. })
    ));
    assert_eq!(env.probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(env.index.search("alphawidget", 10).unwrap().len(), 0);
}
