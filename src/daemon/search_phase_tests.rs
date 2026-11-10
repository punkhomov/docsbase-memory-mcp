//! Actual dispatch and provider gates; watchdogs only guard broken execution.

use super::*;
use crate::config::bootstrap::DefaultsContext;
use crate::config::resolve::resolve_component;
use crate::config::schema::SourceId;
use crate::hybrid::Reranker;
use crate::store::models::{Project, ProjectStatus};
use crate::store::settings::{ReadPoolPatch, ReadPoolSpec};
use crate::vector_cache::Embedder;
use std::sync::mpsc as std_mpsc;

const WATCHDOG: Duration = Duration::from_secs(300);

struct Fixture {
    shared: Arc<Shared>,
    project: Project,
    session: u64,
    _cache: tempfile::TempDir,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new(stages: HybridStages) -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let root = tempfile::tempdir().expect("project");
        std::fs::write(
            root.path().join("guide.md"),
            "# Guide\n\nold widgets text\n",
        )
        .expect("document");
        let pool = resolve_component::<ReadPoolSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Cli,
                ReadPoolPatch {
                    max_connections: Some(1),
                },
            )],
        )
        .expect("one prepared reader");
        let shared = Shared::open_with_policies(
            cache.path(),
            &crate::store::settings::standalone().expect("DB policy"),
            &pool,
        )
        .expect("shared");
        let project = {
            let mut db = shared.db();
            let project = registry::ensure_project(&mut db, root.path()).expect("project");
            tools::run_project_index(&mut db, cache.path(), &project, &Config::default())
                .expect("index");
            registry::project_by_id(&db, project.id)
                .expect("query")
                .expect("registered")
        };
        assert_eq!(project.status, ProjectStatus::Indexed);
        let slot = shared.project_slot(project.id);
        {
            let mut state = slot.state.lock().expect("state");
            state.config = Some(Arc::new(Config::default()));
            state.stages = Some(Arc::new(stages));
        }
        let (session, _cancel) = shared
            .sessions
            .join(
                std::process::id(),
                project.canonical_root.clone(),
                Some(project.id),
            )
            .expect("session");
        Self {
            shared,
            project,
            session,
            _cache: cache,
            _root: root,
        }
    }
}

struct GatedReranker {
    entered: std_mpsc::Sender<()>,
    release: Mutex<std_mpsc::Receiver<()>>,
}

impl Reranker for GatedReranker {
    fn rerank(&self, _: &str, documents: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        assert_eq!(documents.len(), 1);
        assert_eq!(top_n, 1);
        self.entered
            .send(())
            .map_err(|err| Error::internal(err.to_string()))?;
        self.release
            .lock()
            .expect("release gate")
            .recv_timeout(WATCHDOG)
            .map_err(|err| Error::internal(err.to_string()))?;
        Ok(vec![(0, 1.0)])
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gated_rerank_does_not_own_global_database() {
    let (entered, observed) = std_mpsc::channel();
    let (release, proceed) = std_mpsc::channel();
    let fixture = Fixture::new(HybridStages {
        reranker: Some(Arc::new(GatedReranker {
            entered,
            release: Mutex::new(proceed),
        })),
        ..HybridStages::default()
    });
    let shared = Arc::clone(&fixture.shared);
    let session = fixture.session;
    let blocked = tokio::spawn(async move {
        call_tool(
            &shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets", "rerank":true}),
        )
        .await
    });
    observed.recv_timeout(WATCHDOG).expect("provider entered");
    let database_free = fixture.shared.db.try_lock().is_ok();
    let reader_free = fixture.shared.readers.try_acquire().is_ok();
    let slot = fixture.shared.project_slot(fixture.project.id);
    let preparation_free = slot.preparation.try_write().is_ok();
    let writer_free = crate::daemon::lifecycle::with_writer_lease(
        &fixture.shared.cache,
        fixture.project.id,
        || Ok(()),
    )
    .is_ok();
    // Detect the original mutex hostage immediately. Never wait a whole
    // watchdog on other requests when their known prerequisite is unavailable.
    let independent = if database_free && reader_free && preparation_free && writer_free {
        tokio::time::timeout(WATCHDOG, async {
            call_tool(&fixture.shared, Some(session), "status", json!({})).await?;
            call_tool(&fixture.shared, Some(session), "list_projects", json!({})).await?;
            call_tool(
                &fixture.shared,
                Some(session),
                "search_docs",
                json!({"query":"widgets", "rerank":false}),
            )
            .await
        })
        .await
        .map_err(|err| err.to_string())
        .and_then(|result| result.map_err(|err| err.to_string()))
    } else {
        Err("global DB held by gated rerank".to_owned())
    };
    let _ = release.send(());
    let completed = blocked.await.expect("search worker");
    // Both provider and async worker have completed before failure assertions.
    assert!(
        database_free,
        "provider retained the daemon-wide database mutex"
    );
    assert!(reader_free, "provider retained the only prepared reader");
    assert!(
        preparation_free,
        "provider retained config preparation READ"
    );
    assert!(writer_free, "provider retained the project file read lease");
    let lexical = independent.expect("independent tools finish before provider release");
    assert_eq!(lexical.as_array().expect("lexical hits").len(), 1);
    assert_eq!(
        completed
            .expect("gated response")
            .as_array()
            .expect("hits")
            .len(),
        1
    );
}

struct TestEmbedder {
    entered: Option<std_mpsc::Sender<()>>,
    release: Mutex<std_mpsc::Receiver<()>>,
    calls: Arc<AtomicUsize>,
}

impl Embedder for TestEmbedder {
    fn model_id(&self) -> &'static str {
        "test/phases@d3@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }

    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = &self.entered {
            entered
                .send(())
                .map_err(|err| Error::internal(err.to_string()))?;
            self.release
                .lock()
                .expect("embedding gate")
                .recv_timeout(WATCHDOG)
                .map_err(|err| Error::internal(err.to_string()))?;
        }
        Ok(vec![1.0, 0.0, 0.0])
    }
}

fn embedding_fixture() -> (Fixture, std_mpsc::Receiver<()>, std_mpsc::Sender<()>) {
    let (entered, observed) = std_mpsc::channel();
    let (release, proceed) = std_mpsc::channel();
    let fixture = Fixture::new(HybridStages {
        embedder: Some(Arc::new(TestEmbedder {
            entered: Some(entered),
            release: Mutex::new(proceed),
            calls: Arc::default(),
        })),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    (fixture, observed, release)
}

fn start_search(fixture: &Fixture) -> tokio::task::JoinHandle<Result<Value>> {
    let shared = Arc::clone(&fixture.shared);
    let session = fixture.session;
    tokio::spawn(async move {
        call_tool(
            &shared,
            Some(session),
            "search_docs",
            json!({"query":"widgets"}),
        )
        .await
    })
}

struct CountingReranker(Arc<AtomicUsize>);

impl Reranker for CountingReranker {
    fn rerank(&self, _: &str, documents: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(documents
            .iter()
            .enumerate()
            .take(top_n)
            .map(|(index, _)| (index, 1.0))
            .collect())
    }
}

#[test]
fn abandoning_dispatch_during_embedding_skips_rerank() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let calls = Arc::new(AtomicUsize::new(0));
    let (fixture, entered, aborted) = runtime.block_on(async {
        let (entered, observed) = std_mpsc::channel();
        let (release, proceed) = std_mpsc::channel();
        let fixture = Fixture::new(HybridStages {
            embedder: Some(Arc::new(TestEmbedder {
                entered: Some(entered),
                release: Mutex::new(proceed),
                calls: Arc::default(),
            })),
            ranking: Ranking::Rrf,
            reranker: Some(Arc::new(CountingReranker(Arc::clone(&calls)))),
        });
        let shared = Arc::clone(&fixture.shared);
        let session = fixture.session;
        let blocked = tokio::spawn(async move {
            call_tool(
                &shared,
                Some(session),
                "search_docs",
                json!({"query":"widgets", "rerank":true}),
            )
            .await
        });
        let entered = observed.recv_timeout(WATCHDOG).is_ok();
        blocked.abort();
        let aborted = blocked.await.is_err_and(|err| err.is_cancelled());
        let _ = release.send(());
        (fixture, entered, aborted)
    });
    // Dropping this runtime waits for the actual blocking worker after release,
    // rather than assuming aborting its async waiter ended provider work.
    drop(runtime);
    assert!(entered && aborted);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "abandoned caller ran rerank"
    );
    assert!(fixture.shared.readers.try_acquire().is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedding_releases_reader_preparation_file_and_global_database() {
    let (fixture, observed, release) = embedding_fixture();
    let blocked = start_search(&fixture);
    observed.recv_timeout(WATCHDOG).expect("embedding started");
    let database_free = fixture.shared.db.try_lock().is_ok();
    let reader_free = fixture.shared.readers.try_acquire().is_ok();
    let slot = fixture.shared.project_slot(fixture.project.id);
    let preparation_free = slot.preparation.try_write().is_ok();
    let writer_free = crate::daemon::lifecycle::with_writer_lease(
        &fixture.shared.cache,
        fixture.project.id,
        || Ok(()),
    )
    .is_ok();
    let status = if database_free {
        tokio::time::timeout(
            WATCHDOG,
            call_tool(&fixture.shared, Some(fixture.session), "status", json!({})),
        )
        .await
        .map_err(|err| err.to_string())
        .and_then(|value| value.map_err(|err| err.to_string()))
    } else {
        Err("DB hostage".to_owned())
    };
    let _ = release.send(());
    let result = blocked.await.expect("worker");
    assert!(
        database_free && reader_free && preparation_free && writer_free,
        "resource availability: DB={database_free}, reader={reader_free}, revision={preparation_free}, file={writer_free}"
    );
    status.expect("status before embedding release");
    result.expect("hybrid result");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_during_embedding_refuses_original_generation_before_payload() {
    let (fixture, observed, release) = embedding_fixture();
    let blocked = start_search(&fixture);
    observed.recv_timeout(WATCHDOG).expect("embedding started");
    let sync = match fixture.shared.db.try_lock() {
        Ok(mut db) => {
            std::fs::write(
                fixture.project.canonical_root.join("guide.md"),
                "# Guide\n\nnew widgets text\n",
            )
            .expect("edit");
            tools::run_project_index(
                &mut db,
                &fixture.shared.cache,
                &fixture.project,
                &Config::default(),
            )
        }
        Err(err) => Err(Error::internal(err.to_string())),
    };
    let _ = release.send(());
    let result = blocked.await.expect("worker");
    sync.expect("sync completes while embedding is paused");
    assert!(matches!(result, Err(Error::Project { ref message, .. })
        if message == "project generation changed during search preparation"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_during_embedding_rejects_old_revision_and_preserves_query_cache() {
    let (fixture, observed, release) = embedding_fixture();
    let blocked = start_search(&fixture);
    observed.recv_timeout(WATCHDOG).expect("embedding started");
    std::fs::write(
        fixture.project.canonical_root.join(".docsbase.toml"),
        "max_file_size = 2048\n",
    )
    .expect("new config");
    let reload = fixture
        .shared
        .open_project_config_with(&fixture.project, |_| Ok(HybridStages::default()));
    let _ = release.send(());
    let result = blocked.await.expect("worker");
    reload.expect("reload while embedding is paused");
    assert!(matches!(result, Err(Error::Project { ref message, .. })
        if message == "project configuration changed before search preparation"));
    assert_eq!(fixture.shared.dense.query_cache_stats().entries, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_bm25_query_never_calls_embedding() {
    let (_, release) = std_mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = Fixture::new(HybridStages {
        embedder: Some(Arc::new(TestEmbedder {
            entered: None,
            release: Mutex::new(release),
            calls: Arc::clone(&calls),
        })),
        ranking: Ranking::Rrf,
        reranker: None,
    });
    let result = call_tool(
        &fixture.shared,
        Some(fixture.session),
        "search_docs",
        json!({"query":"("}),
    )
    .await;
    assert!(matches!(result, Err(Error::Query { .. })));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn caller_transaction_is_refused_without_ending_its_snapshot() {
    let fixture = Fixture::new(HybridStages::default());
    let db = fixture.shared.db();
    db.connection()
        .execute_batch("BEGIN; SELECT * FROM projects")
        .expect("caller transaction");
    let context = tools::SearchContext {
        stages: &HybridStages::default(),
        dense: &fixture.shared.dense,
        cache: &fixture.shared.cache,
        project: &fixture.project,
        window: 1,
        payload_budget: &fixture.shared.payload_budget,
        operation: crate::search::operation::SearchOperation::new(
            &fixture.shared.global_config.search,
            Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )
        .expect("operation"),
    };
    let result = tools::search_docs_in(&context, &db, &json!({"query":"widgets"}));
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(
        !db.connection().is_autocommit(),
        "caller still owns its transaction"
    );
    db.connection()
        .execute_batch("ROLLBACK")
        .expect("caller cleanup");
}

#[test]
fn metadata_source_cannot_mix_another_cache_with_the_bound_project() {
    let fixture = Fixture::new(HybridStages::default());
    let other = Fixture::new(HybridStages::default());
    let context = tools::SearchContext {
        stages: &HybridStages::default(),
        dense: &fixture.shared.dense,
        cache: &fixture.shared.cache,
        project: &fixture.project,
        window: 1,
        payload_budget: &fixture.shared.payload_budget,
        operation: crate::search::operation::SearchOperation::new(
            &fixture.shared.global_config.search,
            Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )
        .expect("operation"),
    };
    let result = tools::search_docs_from(
        &context,
        crate::search::metadata::MetadataSource::Prepared(&other.shared.readers),
        &json!({"query":"widgets"}),
        || Ok(()),
    );
    assert!(matches!(result, Err(Error::Admission { ref message })
        if message == "search metadata belongs to a different cache root"));
    assert!(other.shared.readers.try_acquire().is_ok());
}

#[test]
fn explain_counts_time_before_search_preparation_without_real_waiting() {
    let fixture = Fixture::new(HybridStages::default());
    let queued = Duration::from_millis(50);
    let context = tools::SearchContext {
        stages: &HybridStages::default(),
        dense: &fixture.shared.dense,
        cache: &fixture.shared.cache,
        project: &fixture.project,
        window: 1,
        payload_budget: &fixture.shared.payload_budget,
        operation: crate::search::operation::SearchOperation::new(
            &fixture.shared.global_config.search,
            Instant::now()
                .checked_sub(queued)
                .expect("injected original start"),
            crate::hybrid::work::Cancellation::new(),
        )
        .expect("operation"),
    };
    for limit in [1, 0] {
        let response = tools::search_docs_from(
            &context,
            crate::search::metadata::MetadataSource::Prepared(&fixture.shared.readers),
            &json!({"query":"widgets", "explain":true, "limit":limit}),
            || Ok(()),
        )
        .expect("search");
        // Injected history, not a host-dependent latency SLA.
        assert!(
            u128::from(
                response["meta"]["stage_ms"]["total"]
                    .as_u64()
                    .expect("elapsed")
            ) >= queued.as_millis(),
            "limit={limit}"
        );
        if limit == 0 {
            assert!(response["hits"].as_array().expect("hits").is_empty());
            assert_eq!(response["meta"]["window"], 0);
            assert!(
                response["meta"]["stages"]
                    .as_array()
                    .expect("stages")
                    .is_empty()
            );
        }
    }
}
