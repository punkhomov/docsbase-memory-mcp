//! Actual index dispatch/provider gates for resumable background work.
use super::*;
use crate::hybrid::remote::{AsyncEmbedder, RemoteCall, StageFuture};
use crate::vector_cache::Embedder;

struct DocumentGate {
    model: &'static str,
    queries: AtomicUsize,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    legacy: tokio::runtime::Handle,
    synchronous: AtomicUsize,
    batches: Mutex<Vec<Vec<String>>>,
    invalid_on: AtomicUsize,
}
impl Embedder for DocumentGate {
    fn model_id(&self) -> &'static str {
        self.model
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.synchronous.fetch_add(1, Ordering::SeqCst);
        let operation = crate::search::operation::SearchOperation::new(
            crate::search::settings::standalone().unwrap().settings(),
            Instant::now(),
            crate::hybrid::work::Cancellation::new(),
        )?;
        self.legacy.block_on(AsyncEmbedder::embed(
            self,
            Arc::from(texts),
            &RemoteCall::new(operation),
        ))
    }
    fn async_remote(&self) -> Option<&dyn AsyncEmbedder> {
        Some(self)
    }
}
impl AsyncEmbedder for DocumentGate {
    fn embed<'a>(
        &'a self,
        texts: Arc<[String]>,
        _: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async move {
            let call = {
                let mut batches = self.batches.lock().unwrap();
                batches.push(texts.to_vec());
                batches.len()
            };
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            let mut vectors = vec![vec![1.0, 0.0, 0.0]; texts.len()];
            if self.invalid_on.load(Ordering::SeqCst) == call
                && let Some(vector) = vectors.last_mut()
            {
                vector[0] = f32::NAN;
            }
            Ok(vectors)
        })
    }
    fn embed_query<'a>(&'a self, _: Arc<str>, _: &'a RemoteCall) -> StageFuture<'a, Vec<f32>> {
        Box::pin(async {
            self.queries.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1.0, 0.0, 0.0])
        })
    }
}

fn open_v2_index_shared(cache: &Path, config: &Path) -> Arc<Shared> {
    open_v2_index_shared_with_config(cache, config, "config_version = 2\n")
}

fn open_v2_index_shared_with_config(cache: &Path, config: &Path, toml: &str) -> Arc<Shared> {
    std::fs::write(config.join("config.toml"), toml).unwrap();
    let launch = LaunchSnapshot::capture_from(config)
        .strict(DaemonPatch::default())
        .unwrap();
    let dense = launch.config.settings().dense.clone();
    Shared::open_with_launch_config(
        cache,
        &crate::store::settings::standalone().unwrap(),
        &crate::store::settings::standalone_read_pool().unwrap(),
        &dense,
        launch,
    )
    .unwrap()
}

#[test]
fn two_v2_index_preparations_before_job_creation_keep_the_same_revision() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let shared = open_v2_index_shared(cache.path(), config.path());
    let jobs = crate::daemon::job_service::JobRoot::new(shared.daemon_settings());
    shared.activate_jobs(jobs.service()).unwrap();

    let args = json!({"path": project.path(), "refresh_config": true});
    let first = prepare_index_context(&shared, None, &args, false)
        .unwrap()
        .unwrap();
    let second = prepare_index_context(&shared, None, &args, false)
        .unwrap()
        .unwrap();
    let db = shared.db();
    assert!(
        crate::store::repo::active_sync_job(db.connection(), first.project.id)
            .unwrap()
            .is_none()
    );
    drop(db);
    let slot = first.revision.as_ref().unwrap();
    assert!(slot.preparation_lease(&first.config).is_ok());
    assert!(slot.preparation_lease(&second.config).is_ok());
    assert!(Arc::ptr_eq(&first.stages, &second.stages));
    std::fs::write(
        project.path().join(".docsbase.toml"),
        "max_docs_per_project = 19\n",
    )
    .unwrap();
    let revised = prepare_index_context(&shared, None, &args, false)
        .unwrap()
        .unwrap();
    assert_eq!(revised.config.max_docs_per_project, 19);
    assert!(slot.preparation_lease(&first.config).is_err());
    assert!(slot.preparation_lease(&revised.config).is_ok());
}

#[test]
fn explicit_v2_index_rejects_an_active_sync_at_each_boundary() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let shared = open_v2_index_shared(cache.path(), config.path());
    let mut jobs = crate::daemon::job_service::JobRoot::new(shared.daemon_settings());
    shared.activate_jobs(jobs.service()).unwrap();

    let args = json!({"path": project.path(), "refresh_config": true});
    let superseded = prepare_index_context(&shared, None, &args, false)
        .unwrap()
        .unwrap();
    let config_path = project.path().join(".docsbase.toml");
    std::fs::write(&config_path, "max_docs_per_project = 19\n").unwrap();
    let prepared = prepare_index_context(&shared, None, &args, false)
        .unwrap()
        .unwrap();
    let slot = prepared.revision.as_ref().unwrap();
    let project_id = prepared.project.id;

    // The other Sync creates its row before releasing its preparation lease.
    std::fs::write(config_path, "max_docs_per_project = 18\n").unwrap();
    let read_db = Db::open_with_settings(&shared.cache, &shared.db_settings).unwrap();
    let preparation = slot.preparation_lease(&prepared.config).unwrap();
    let mut job_id = None;
    let publication = shared.publish_project_config(
        &prepared.project,
        |config| shared.async_stages(config),
        || {
            job_id = crate::store::repo::create_sync_job(read_db.connection(), project_id).unwrap();
            drop(preparation);
        },
        Some(&read_db),
    );
    let job_id = job_id.expect("publication waited for competing sync");
    assert!(matches!(publication, Err(Error::Admission { .. })));
    assert!(slot.preparation_lease(&prepared.config).is_ok());
    assert!(matches!(
        prepare_index_context(&shared, None, &args, false),
        Err(Error::Admission { .. })
    ));
    for (request, detached) in [
        (json!({"path":project.path()}), false),
        (json!({"project_id":project_id}), true),
    ] {
        let existing = prepare_index_context(&shared, None, &request, detached)
            .unwrap()
            .err()
            .expect("ordinary Index and Sync join the current job");
        assert_eq!(existing["job_id"], job_id);
    }

    let operation = crate::search::operation::SearchOperation::with_timeout(
        Instant::now(),
        std::time::Duration::from_secs(5),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let service = jobs.service();
    jobs.run(&runtime, async {
        for context in [prepared, superseded] {
            let result = crate::index::async_job::run(
                &service,
                &shared.workers,
                shared.activity.track(),
                operation.clone(),
                &shared.payload_budget,
                context,
            )
            .await;
            assert!(
                matches!(&result, Err(Error::Admission { message }) if message.contains("retry `docsbase index`")),
                "{result:?}"
            );
        }
        Ok(())
    })
    .unwrap();
    let active_job = crate::store::repo::active_sync_job(read_db.connection(), project_id)
        .unwrap()
        .unwrap();
    assert_eq!(active_job.id, job_id);
}

#[test]
fn held_document_embedding_releases_actual_control_dispatch() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let settings = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(settings.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let provider = document_gate();
        let mut fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(provider.clone()),
            ..HybridStages::default()
        });
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Guide\n\nchanged widget document\n",
        )
        .unwrap();

        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut policy = shared.global_config.daemon.clone();
        policy.control_active = 1;
        policy.control_pending = 0;
        shared.workers = Arc::new(
            crate::daemon::worker_admission::WorkerAdmission::with_policies(
                &policy,
                &shared.global_config.remote,
            )
            .unwrap(),
        );
        fixture.shared.activate_jobs(service).unwrap();
        let shared = fixture.shared.clone();
        let session = fixture.session;
        let indexing = tokio::spawn(async move {
            call_tool(&shared, Some(session), "index_project", json!({})).await
        });
        tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let independent = tokio::time::timeout(
            Duration::from_secs(5),
            call_tool(&fixture.shared, Some(session), "list_projects", json!({})),
        )
        .await;
        let physical = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.shared.workers.background(
                &crate::search::operation::SearchOperation::with_timeout(
                    Instant::now(),
                    Duration::from_secs(5),
                    crate::hybrid::work::Cancellation::new(),
                )
                .unwrap(),
            ),
        )
        .await;
        let native_writer_refused =
            lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                .is_err();
        let global_free =
            fixture.shared.db.try_lock().is_ok() && fixture.shared.readers.try_acquire().is_ok();
        // Cleanup precedes every RED assertion and runtime teardown.
        provider.release.add_permits(10);
        drop(physical);
        let indexed = indexing.await.unwrap();
        assert!(native_writer_refused && global_free);
        let projects = independent
            .expect("control dispatch must finish while document embedding is held")
            .expect("independent control request must acquire capacity");
        assert!(projects.as_array().is_some());
        indexed.expect("index must succeed after release");
        assert_eq!(
            provider.synchronous.load(Ordering::SeqCst),
            0,
            "new index dispatch must not enter the live synchronous provider bridge"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn low_capture_budget_refuses_before_project_effects_and_terminalizes_only_own_sync() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let mut fixture = search_phase_tests::Fixture::new(HybridStages::default());

        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut budget = shared.global_config.search.clone();
        budget.max_request_payload_bytes = 16 * 1024;
        budget.max_retained_payload_bytes = 32 * 1024;
        shared.payload_budget = crate::search::budget::PayloadBudget::new(budget).unwrap();
        fixture.shared.activate_jobs(service).unwrap();
        let snapshot = |db: &Db| -> (String, i64, Option<String>, i64, Vec<crate::store::repo::DocState>) {
            let row: (String, i64, Option<String>, i64) = db.connection().query_row(
                "SELECT status,index_attempt,index_outcome_json,index_generation FROM projects WHERE id=?1", [fixture.project.id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).unwrap();
            (row.0,row.1,row.2,row.3,crate::store::repo::doc_states(db.connection(), fixture.project.id).unwrap())
        };
        let before = snapshot(&fixture.shared.db());
        let index_path = lifecycle::index_dir(&fixture.shared.cache, fixture.project.id);
        let index_before = std::fs::read(index_path.join("meta.json")).unwrap();
        std::fs::write(fixture.project.canonical_root.join("guide.md"), "# Changed\n\nreplacement words\n").unwrap();
        let failure = call_tool(&fixture.shared, Some(fixture.session), "index_project", json!({})).await;
        // Root drives terminal work even after the caller has already received refusal.
        fixture.shared.activity.drain(Duration::from_millis(policy.settings().drain_poll_ms)).await;
        let after = snapshot(&fixture.shared.db());
        assert!(matches!(failure, Err(Error::Admission { .. })), "{failure:?}");
        assert_eq!(before, after, "capture refusal must preserve status/attempt/outcome/docs/generation");
        assert_eq!(index_before, std::fs::read(index_path.join("meta.json")).unwrap(), "capture refusal must not recreate or commit the native index");
        let db = fixture.shared.db();
        let state: String = db.connection().query_row("SELECT state FROM sync_jobs WHERE project_id=?1 ORDER BY id DESC LIMIT 1", [fixture.project.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "error");
        Ok(())
    }).unwrap();
}

fn document_gate() -> Arc<DocumentGate> {
    document_gate_for("test/async-documents@d3@l2")
}
fn document_gate_for(model: &'static str) -> Arc<DocumentGate> {
    Arc::new(DocumentGate {
        model,
        queries: AtomicUsize::new(0),
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        legacy: tokio::runtime::Handle::current(),
        synchronous: AtomicUsize::new(0),
        batches: Mutex::new(vec![]),
        invalid_on: AtomicUsize::new(0),
    })
}
fn batch_fixture(
    provider: &Arc<DocumentGate>,
    service: crate::daemon::job_service::JobService,
) -> search_phase_tests::Fixture {
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(provider.clone()),
        ..HybridStages::default()
    });
    fixture.shared.activate_jobs(service).unwrap();
    let slot = fixture.shared.project_slot(fixture.project.id);
    let mut config = Config::default();
    config.remote.embed_batch_texts = 2;
    config.remote.embed_batch_bytes = 64;
    slot.state.lock().unwrap().config = Some(Arc::new(config));
    fixture
}
#[cfg(unix)]
fn selected_catalog_hits(
    shared: &Shared,
    project: &crate::store::models::Project,
    query: &str,
) -> usize {
    let reader = shared.readers.try_acquire().unwrap();
    crate::store::project_index::with_catalog_pair(reader, project, |_, local, _| {
        let dir = local.unwrap().tantivy_dir()?;
        Ok(crate::index::tantivy_index::ReadIndex::open(&dir)?
            .search(query, 3)?
            .len())
    })
    .unwrap()
}

#[cfg(unix)]
async fn catalog_index_waiting_for_provider(
    shared: &Arc<Shared>,
    args: &Value,
    provider: &DocumentGate,
) -> tokio::task::JoinHandle<Result<Value>> {
    let owner = shared.clone();
    let args = args.clone();
    let mut indexing =
        tokio::spawn(async move { call_tool(&owner, None, "index_project", args).await });
    tokio::select! {
        entered = tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire()) => entered.unwrap().unwrap().forget(),
        result = &mut indexing => panic!("catalog job finished before embedding: {result:?}"),
    }
    indexing
}

#[cfg(unix)]
fn open_v5_index_shared(cache: &Path, config: &Path, toml: &str) -> Arc<Shared> {
    Db::open_for_index(cache, &crate::store::settings::standalone().unwrap(), true)
        .unwrap()
        .close()
        .unwrap();
    let mut shared = open_v2_index_shared_with_config(cache, config, toml);
    Arc::get_mut(&mut shared).unwrap().watchers = Arc::new(Watchers::with_enabled(false));
    shared
}

#[cfg(unix)]
async fn catalog_search_hits(shared: &Arc<Shared>, session: u64, query: &str) -> usize {
    call_tool(
        shared,
        Some(session),
        "search_docs",
        json!({"query": query}),
    )
    .await
    .unwrap()
    .as_array()
    .unwrap()
    .len()
}

#[test]
#[cfg(unix)]
fn catalog_refresh_keeps_old_runtime_until_the_index_commits() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "retainedcatalogwidget").unwrap();
        std::fs::write(root.path().join("extra.md"), "omittedcatalogwidget").unwrap();
        let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
        shared.activate_jobs(service).unwrap();
        let args = json!({"path": root.path(), "refresh_config": true});
        call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let project = registry::list_projects(&shared.db()).unwrap().remove(0);
        let (session, _cancel) = shared
            .sessions
            .join(
                std::process::id(),
                project.canonical_root.clone(),
                Some(project.id),
            )
            .unwrap();
        let (before, before_stages) = shared.project_runtime_for_search(&project).unwrap();
        let hits = |query: &'static str| catalog_search_hits(&shared, session, query);
        assert_eq!(hits("omittedcatalogwidget").await, 1);
        shared
            .db()
            .connection()
            .execute_batch(
                "CREATE TABLE deferred_refresh_failure (
                   project_id INTEGER REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED
                 );
                 CREATE TRIGGER reject_catalog_refresh AFTER UPDATE ON project_generation_head
                 BEGIN INSERT INTO deferred_refresh_failure VALUES (-1); END;",
            )
            .unwrap();
        std::fs::write(
            root.path().join(".docsbase.toml"),
            "config_version = 2\nignores = [\"extra.md\"]\n",
        )
        .unwrap();
        let error = call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("new head was not committed"),
            "{error}"
        );
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let (active, stages) = shared.project_runtime_for_search(&project).unwrap();
        assert!(
            Arc::ptr_eq(&before, &active),
            "rollback replaced active config"
        );
        assert!(
            Arc::ptr_eq(&before_stages, &stages),
            "rollback replaced active stages"
        );
        assert!(Arc::ptr_eq(
            &before,
            &shared.open_project_config(&project).unwrap()
        ));
        assert_eq!(hits("omittedcatalogwidget").await, 1);

        shared
            .db()
            .connection()
            .execute_batch(
                "DROP TRIGGER reject_catalog_refresh; DROP TABLE deferred_refresh_failure",
            )
            .unwrap();
        call_tool(&shared, None, "index_project", args)
            .await
            .unwrap();
        assert_eq!(
            shared.project_config(&project).unwrap().ignores,
            ["extra.md"]
        );
        assert_eq!(hits("omittedcatalogwidget").await, 0);
        assert_eq!(hits("retainedcatalogwidget").await, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn shared_catalog_reopen_accepts_the_restored_published_recipe() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("guide.md"), "restoredcatalogwidget").unwrap();
    let mut original = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = original.service();
    original
        .run(&runtime, async {
            let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
            shared.activate_jobs(service).unwrap();
            call_tool(&shared, None, "index_project", json!({"path": root.path()}))
                .await
                .unwrap();
            shared
                .activity
                .drain(Duration::from_millis(policy.settings().drain_poll_ms))
                .await;
            Ok(())
        })
        .unwrap();

    let path = root.path().join(".docsbase.toml");
    std::fs::write(&path, "config_version = 2\nignores = ['guide.md']\n").unwrap();
    let mut restarted = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = restarted.service();
    restarted
        .run(&runtime, async {
            let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
            shared.activate_jobs(service).unwrap();
            let project = registry::list_projects(&shared.db()).unwrap().remove(0);
            assert_eq!(
                shared.open_project_config(&project).unwrap().ignores,
                ["guide.md"]
            );
            std::fs::remove_file(path).unwrap();
            let restored = shared.open_project_config(&project).unwrap();
            assert_eq!(restored.ignores, Vec::<String>::new());

            let (session, _cancel) = shared
                .sessions
                .join(
                    std::process::id(),
                    project.canonical_root.clone(),
                    Some(project.id),
                )
                .unwrap();
            assert_eq!(
                catalog_search_hits(&shared, session, "restoredcatalogwidget").await,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
async fn retry_catalog_watch_after_runtime_reconciliation(
    shared: &Arc<Shared>,
    project: &crate::store::models::Project,
    previous: Arc<crate::watch::WatchRuntime>,
    accepted: Arc<crate::watch::WatchRuntime>,
) {
    let doc = project.canonical_root.join("new.md");
    std::fs::write(&doc, "recoveredcatalogwatch").unwrap();
    let (_sender, mut events) = tokio::sync::mpsc::channel(1);
    let operation = shared.watch_operation().unwrap();
    let paths = crate::watch::merge_pending(
        &shared.payload_budget,
        None,
        Some(vec![doc]),
        &mut events,
        &operation,
        &shared.watchers,
    )
    .unwrap();
    let result = shared
        .run_watch_batch(
            project.clone(),
            previous,
            paths.clone(),
            operation.clone(),
            shared.watch_activity(),
        )
        .await;
    assert!(matches!(result, Err(Error::Internal { .. })), "{result:?}");
    let stats = shared
        .run_watch_batch(
            project.clone(),
            accepted,
            paths,
            operation,
            shared.watch_activity(),
        )
        .await
        .unwrap();
    assert_eq!(stats.docs, 1);
}

#[test]
#[cfg(unix)]
fn shared_catalog_reconciles_a_retained_runtime_against_the_selected_seal() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "selectedcatalogwidget").unwrap();
        let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
        shared.activate_jobs(service).unwrap();
        let args = json!({"path": root.path(), "refresh_config": true});
        call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let project = registry::list_projects(&shared.db()).unwrap().remove(0);
        let (previous, previous_stages) = shared.project_runtime_for_search(&project).unwrap();

        let path = root.path().join(".docsbase.toml");
        std::fs::write(&path, "config_version = 2\nignores = ['guide.md']\n").unwrap();
        call_tool(&shared, None, "index_project", args)
            .await
            .unwrap();
        let (committed, committed_stages) = shared.project_runtime_for_search(&project).unwrap();
        let head =
            CatalogHead::from_selected_fixture(shared.db().connection(), project.id).unwrap();
        let pending = Arc::new(crate::watch::WatchRuntime::new(
            committed.clone(),
            committed_stages,
            shared.vector_source_for(project.id, &committed),
        ));
        let slot = shared.project_slot(project.id);
        {
            // Emulate the in-memory boundary after SQLite committed and its
            // first independent verification failed. The actual seal is intact.
            let publication = slot.catalog_publication(&committed).unwrap();
            publication.install(previous.clone(), previous_stages.clone());
            let transition = slot.begin_watch_config_transition(committed.clone());
            publication.retain_unverified(head, pending.clone(), Some(transition));
        }
        assert!(slot.preparation_lease(&previous).is_err());
        // A source edit after the accepted commit must not replace its candidate.
        std::fs::remove_file(path).unwrap();
        let previous_runtime = Arc::new(crate::watch::WatchRuntime::new(
            previous.clone(),
            previous_stages,
            shared.vector_source_for(project.id, &previous),
        ));
        retry_catalog_watch_after_runtime_reconciliation(
            &shared,
            &project,
            previous_runtime,
            pending,
        )
        .await;
        let (session, _cancel) = shared
            .sessions
            .join(
                std::process::id(),
                project.canonical_root.clone(),
                Some(project.id),
            )
            .unwrap();
        assert_eq!(
            catalog_search_hits(&shared, session, "selectedcatalogwidget").await,
            0
        );
        assert_eq!(
            catalog_search_hits(&shared, session, "recoveredcatalogwatch").await,
            1
        );
        assert!(Arc::ptr_eq(
            &committed,
            &shared.project_config(&project).unwrap()
        ));
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn catalog_refresh_rechecks_the_project_source_after_capture() {
    use crate::index::async_job::catalog_sync;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "retainedcatalogsource").unwrap();
        let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
        shared.activate_jobs(service.clone()).unwrap();
        let args = json!({"path": root.path(), "refresh_config": true});
        call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let project = registry::list_projects(&shared.db()).unwrap().remove(0);
        let old = shared.project_config(&project).unwrap();

        let path = root.path().join(".docsbase.toml");
        std::fs::write(&path, "config_version = 2\nignores = ['guide.md']\n").unwrap();
        let context =
            capture_catalog_sync(&shared, None, &args, shared.catalog_epoch.unwrap(), false)
                .unwrap()
                .unwrap();
        std::fs::write(&path, "config_version = 2\nignores = ['different.md']\n").unwrap();

        let operation = shared.watch_operation().unwrap();
        let activity = shared.activity.track();
        let owner = service
            .admit(
                shared.workers.clone(),
                activity.clone(),
                &operation,
                &shared.payload_budget,
            )
            .await
            .unwrap();
        let call = operation.clone();
        let (owner, status) = service
            .phase(
                &shared.workers,
                activity.clone(),
                &operation,
                owner,
                move |state| state.prepare_catalog_sync(context, &call),
            )
            .await
            .unwrap();
        drop(status);
        let failure =
            catalog_sync::execute(&service, &shared.workers, &activity, &operation, owner)
                .await
                .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("configuration changed before publication"),
            "{failure}"
        );
        assert!(Arc::ptr_eq(&old, &shared.project_config(&project).unwrap()));
        assert_eq!(
            selected_catalog_hits(&shared, &project, "retainedcatalogsource"),
            1
        );
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
fn catalog_test_embedder(
    shared: &Arc<Shared>,
    project: &crate::store::models::Project,
    provider: &Arc<DocumentGate>,
) -> Arc<Config> {
    let mut config = (*shared.project_config(project).unwrap()).clone();
    config.remote.embed_batch_texts = 1;
    let config = Arc::new(config);
    let slot = shared.project_slot(project.id);
    let mut state = slot.state.lock().unwrap();
    state.config = Some(config.clone());
    state.search_stages = Some(Arc::new(HybridStages {
        embedder: Some(provider.clone()),
        ranking: Ranking::Rrf,
        ..HybridStages::default()
    }));
    config
}

#[test]
#[cfg(unix)]
fn catalog_hybrid_watcher_publishes_only_after_valid_embedding() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let doc = root.path().join("watch.md");
        std::fs::write(&doc, "oldwatchproofwidget").unwrap();
        let shared = open_v5_index_shared(cache.path(), config_dir.path(), "config_version = 2\n");
        shared.activate_jobs(service).unwrap();
        let project = registry::ensure_project(&mut shared.db(), root.path()).unwrap();
        let provider = document_gate_for("test/catalog-watch@d3@l2");
        catalog_test_embedder(&shared, &project, &provider);
        provider.release.add_permits(1);
        call_tool(&shared, None, "index_project", json!({"path": root.path()})).await.unwrap();
        provider.entered.try_acquire().unwrap().forget();
        let (session, _cancel) = shared.sessions.join(std::process::id(), project.canonical_root.clone(), Some(project.id)).unwrap();
        let before = catalog_diagnostics(&shared, session).await;
        let (config, stages) = shared.project_runtime_for_search(&project).unwrap();
        let source = shared.vector_source_for(project.id, &config);
        let watch_runtime = Arc::new(crate::watch::WatchRuntime::new(config, stages, source));
        std::fs::write(&doc, "newwatchproofwidget").unwrap();
        let (_events, mut receiver) = tokio::sync::mpsc::channel(1);
        let operation = shared.watch_operation().unwrap();
        let paths = crate::watch::merge_pending(&shared.payload_budget, None, Some(vec![doc]), &mut receiver, &operation, &shared.watchers).unwrap();
        provider.invalid_on.store(2, Ordering::SeqCst);
        let watch = || {
            let shared = shared.clone();
            let runtime = watch_runtime.clone();
            let project = project.clone();
            let paths = paths.clone();
            tokio::spawn(async move {
                let operation = shared.watch_operation().unwrap();
                shared.run_watch_batch(project, runtime, paths, operation, shared.watch_activity()).await
            })
        };
        let mut failed = watch();
        tokio::select! {
            entered = tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire()) => entered.unwrap().unwrap().forget(),
            result = &mut failed => panic!("watcher returned before preparing its vectors: {result:?}"),
        }
        assert_eq!(selected_catalog_hits(&shared, &project, "oldwatchproofwidget"), 1);
        assert_eq!(selected_catalog_hits(&shared, &project, "newwatchproofwidget"), 0);
        provider.release.add_permits(1);
        assert!(failed.await.unwrap().unwrap_err().to_string().contains("invalid embedding batch"));
        let failed = catalog_diagnostics(&shared, session).await;
        assert_eq!(failed["index_generation"], before["index_generation"]);
        assert_eq!(selected_catalog_hits(&shared, &project, "oldwatchproofwidget"), 1);

        provider.release.add_permits(1);
        assert_eq!(watch().await.unwrap().unwrap().docs, 1);
        let after = catalog_diagnostics(&shared, session).await;
        assert_eq!(after["index_generation"].as_i64().unwrap(), before["index_generation"].as_i64().unwrap() + 1);
        assert_eq!(after["embed_coverage"], json!({"total": 1, "cached": 1}));
        assert_eq!(selected_catalog_hits(&shared, &project, "newwatchproofwidget"), 1);
        assert_eq!(watch().await.unwrap().unwrap().docs, 0);
        assert_eq!(catalog_diagnostics(&shared, session).await["index_generation"], after["index_generation"]);
        let duplicate = root.path().join("duplicate.md");
        std::fs::write(&duplicate, "newwatchproofwidget").unwrap();
        let operation = shared.watch_operation().unwrap();
        let duplicate = catalog_test_watch_paths(&shared, duplicate, &operation);
        let stats = tokio::time::timeout(Duration::from_secs(5), shared.run_watch_batch(
            project, watch_runtime.clone(), duplicate, operation, shared.watch_activity(),
        )).await.unwrap().unwrap();
        assert_eq!(stats.docs, 1);
        assert_eq!(provider.batches.lock().unwrap().len(), 3);
        Ok(())
    }).unwrap();
}

#[test]
#[cfg(unix)]
fn catalog_hybrid_watcher_evicts_only_after_protecting_the_selected_pair() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let guide = root.path().join("guide.md");
        let removed = root.path().join("remove.md");
        std::fs::write(&guide, "oldcapacitywatchwidget").unwrap();
        std::fs::write(&removed, "retiredcapacitywatchwidget").unwrap();
        let shared = open_v5_index_shared(
            cache.path(),
            config_dir.path(),
            "config_version = 2\n[vector_cache]\nmax_entries = 2\n",
        );
        shared.activate_jobs(service).unwrap();
        let project = registry::ensure_project(&mut shared.db(), root.path()).unwrap();
        let provider = document_gate_for("test/catalog-watch-capacity@d3@l2");
        catalog_test_embedder(&shared, &project, &provider);
        provider.release.add_permits(5);
        call_tool(&shared, None, "index_project", json!({"path": root.path()}))
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let (session, _cancel) = shared
            .sessions
            .join(
                std::process::id(),
                project.canonical_root.clone(),
                Some(project.id),
            )
            .unwrap();
        let (config, stages) = shared.project_runtime_for_search(&project).unwrap();
        let source = shared.vector_source_for(project.id, &config);
        let watch_runtime = Arc::new(crate::watch::WatchRuntime::new(
            config,
            stages,
            source.clone(),
        ));
        let watch = |file: PathBuf| {
            let shared = shared.clone();
            let project = project.clone();
            let watch_runtime = watch_runtime.clone();
            async move {
                let operation = shared.watch_operation().unwrap();
                let paths = catalog_test_watch_paths(&shared, file, &operation);
                shared
                    .run_watch_batch(
                        project,
                        watch_runtime,
                        paths,
                        operation,
                        shared.watch_activity(),
                    )
                    .await
            }
        };
        let before = catalog_diagnostics(&shared, session).await;
        std::fs::write(&guide, "newcapacitywatchwidget").unwrap();
        assert_failed_watch_keeps_over_budget_cache(cache.path(), &provider, watch(guide.clone()))
            .await;
        let error = watch(guide.clone()).await.unwrap_err();
        assert!(
            error.to_string().contains("active and prepared vectors"),
            "{error}"
        );
        let refused = catalog_diagnostics(&shared, session).await;
        assert_eq!(refused["index_generation"], before["index_generation"]);
        assert_eq!(refused["embed_coverage"], json!({"total": 2, "cached": 2}));
        assert_eq!(
            selected_catalog_hits(&shared, &project, "oldcapacitywatchwidget"),
            1
        );
        std::fs::remove_file(&removed).unwrap();
        assert_eq!(watch(removed).await.unwrap().removed, 1);
        assert_eq!(watch(guide).await.unwrap().docs, 1);
        let applied = catalog_diagnostics(&shared, session).await;
        assert_eq!(
            applied["index_generation"].as_i64().unwrap(),
            before["index_generation"].as_i64().unwrap() + 2
        );
        assert_eq!(applied["embed_coverage"], json!({"total": 1, "cached": 1}));
        assert_eq!(
            selected_catalog_hits(&shared, &project, "newcapacitywatchwidget"),
            1
        );
        assert_eq!(provider.batches.lock().unwrap().len(), 5);
        assert_eq!(source.connect(cache.path()).unwrap().len().unwrap(), 2);
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
async fn assert_failed_watch_keeps_over_budget_cache(
    cache: &Path,
    provider: &DocumentGate,
    watch: impl std::future::Future<Output = Result<crate::index::job::JobStats>>,
) {
    // A previous, larger limit can leave more rows than the new owner allows.
    let db = rusqlite::Connection::open(cache.join(crate::vector_cache::VECTOR_DB_FILE)).unwrap();
    db.execute(
        "INSERT INTO vector_cache (model_id, chunk_hash, dim, vector, used_at)
                VALUES ('prior-model', 'unreferenced', 3, zeroblob(12), 1000000)",
        [],
    )
    .unwrap();
    provider.invalid_on.store(3, Ordering::SeqCst);
    let failure = watch.await.unwrap_err();
    assert!(
        failure.to_string().contains("invalid embedding batch"),
        "{failure}"
    );
    let count = db
        .query_row("SELECT COUNT(*) FROM vector_cache", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 3);
}

#[cfg(unix)]
fn catalog_test_watch_paths(
    shared: &Shared,
    file: PathBuf,
    operation: &crate::search::operation::SearchOperation,
) -> Arc<crate::watch::WatchPayload> {
    let (_sender, mut events) = tokio::sync::mpsc::channel(1);
    crate::watch::merge_pending(
        &shared.payload_budget,
        None,
        Some(vec![file]),
        &mut events,
        operation,
        &shared.watchers,
    )
    .unwrap()
}

#[cfg(unix)]
fn catalog_warm_metadata(
    shared: &Arc<Shared>,
    project: &crate::store::models::Project,
) -> Option<(i64, String)> {
    let (called, metadata) = std::sync::mpsc::channel();
    shared
        .refresh_ann_after_sync_with(project, move |db, _, model| {
            let generation = db
                .connection()
                .query_row(
                    "SELECT generation_id FROM generation_state WHERE singleton=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            called.send((generation, model.to_owned())).unwrap();
            Ok(())
        })
        .unwrap()
        .join()
        .unwrap();
    metadata.try_recv().ok()
}

#[cfg(unix)]
async fn catalog_diagnostics(shared: &Arc<Shared>, session: u64) -> Value {
    let mut status = call_tool(shared, Some(session), "status", json!({}))
        .await
        .unwrap();
    let project = status["projects"][0].take();
    let panel = call_tool(shared, Some(session), "panel_projects", json!({}))
        .await
        .unwrap();
    assert_eq!(
        panel[0]["cached_vectors"],
        project["embed_coverage"]["cached"]
    );
    assert_eq!(
        panel[0]["vector_chunks"],
        project["embed_coverage"]["total"]
    );
    assert_eq!(panel[0]["ann_state"], project["ann"]["state"]);
    project
}

#[cfg(unix)]
async fn catalog_diagnostics_with_small_payload_allowance(shared: &Arc<Shared>, session: u64) {
    let policy = shared.payload_budget.settings();
    let mut remaining = policy.max_retained_payload_bytes - 192;
    let mut held = Vec::new();
    while remaining > 0 {
        let bytes = remaining.min(policy.max_request_payload_bytes);
        held.push(shared.payload_budget.reserve(bytes).unwrap());
        remaining -= bytes;
    }
    let view = catalog_diagnostics(shared, session).await;
    assert_eq!(view["docs"], 1);
    assert!(view["embed_coverage"].is_null());
    drop(held);
}

#[cfg(unix)]
async fn catalog_config_only_diagnostics(shared: &Arc<Shared>, project: &Project, session: u64) {
    let slot = shared.project_slot(project.id);
    let prior = {
        let mut state = slot.state.lock().unwrap();
        let mut config = (**state.config.as_ref().unwrap()).clone();
        config.hybrid_cfg.enabled = true;
        config.hybrid_cfg.ranking = Ranking::Rrf;
        config.hybrid_cfg.embedding = Some(crate::hybrid::config::EmbeddingCfg {
            provider: "openrouter".into(),
            model: "test/sealed-model".into(),
            dimensions: 3,
            normalize: true,
        });
        let previous = state.config.replace(Arc::new(config));
        (previous, state.stages.take(), state.search_stages.take())
    };
    let view = catalog_diagnostics(shared, session).await;
    assert_eq!(view["embed_coverage"], json!({"cached": 1, "total": 1}));
    let mut state = slot.state.lock().unwrap();
    assert!(state.stages.is_none() && state.search_stages.is_none());
    (state.config, state.stages, state.search_stages) = prior;
}

#[cfg(unix)]
fn assert_catalog_embedding_seal(cache: &Path, project: i64, generation: i64, model: &str) {
    let seal = cache
        .join("projects")
        .join(project.to_string())
        .join("generations")
        .join(generation.to_string())
        .join("seal.json");
    let seal = std::fs::read_to_string(seal).unwrap();
    assert!(seal.contains("embedding_recipe"));
    assert!(!seal.contains(model));
}

#[test]
#[cfg(unix)]
fn catalog_search_checks_the_sealed_model_before_query_embedding() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("guide.md"), "sealedmodelwidget").unwrap();
    let model = "openrouter/test/sealed-model@d3@l2";

    let mut initial = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = initial.service();
    initial
        .run(&runtime, async {
            let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
            shared.activate_jobs(service).unwrap();
            let project = registry::ensure_project(&mut shared.db(), root.path()).unwrap();
            let provider = document_gate_for(model);
            catalog_test_embedder(&shared, &project, &provider);
            provider.release.add_permits(1);
            call_tool(&shared, None, "index_project", json!({"path": root.path()}))
                .await
                .unwrap();
            shared
                .activity
                .drain(Duration::from_millis(policy.settings().drain_poll_ms))
                .await;
            Ok(())
        })
        .unwrap();

    let mut restarted = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = restarted.service();
    restarted
        .run(&runtime, async {
            let shared = open_v5_index_shared(cache.path(), config.path(), "config_version = 2\n");
            shared.activate_jobs(service).unwrap();
            let project = registry::list_projects(&shared.db()).unwrap().remove(0);
            let (session, _cancel) = shared
                .sessions
                .join(
                    std::process::id(),
                    project.canonical_root.clone(),
                    Some(project.id),
                )
                .unwrap();
            let other = document_gate_for("test/new-model@d3@l2");
            catalog_test_embedder(&shared, &project, &other);
            let error = call_tool(
                &shared,
                Some(session),
                "search_docs",
                json!({"query": "sealedmodelwidget"}),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("embedding recipe"), "{error}");
            assert_eq!(other.queries.load(Ordering::SeqCst), 0);
            assert_eq!(catalog_warm_metadata(&shared, &project), None);
            let view = catalog_diagnostics(&shared, session).await;
            assert!(view["embed_coverage"].is_null());

            let original = document_gate_for(model);
            catalog_config_only_diagnostics(&shared, &project, session).await;
            catalog_test_embedder(&shared, &project, &original);
            catalog_diagnostics_with_small_payload_allowance(&shared, session).await;
            assert_eq!(original.queries.load(Ordering::SeqCst), 0);
            let (generation, used) = catalog_warm_metadata(&shared, &project).unwrap();
            assert_eq!(used, model);
            assert_catalog_embedding_seal(&shared.cache, project.id, generation, model);
            assert_eq!(
                catalog_search_hits(&shared, session, "sealedmodelwidget").await,
                1
            );
            assert_eq!(original.queries.load(Ordering::SeqCst), 1);

            let view = catalog_diagnostics(&shared, session).await;
            assert_eq!(view["ann"]["state"], "ready");

            catalog_test_embedder(&shared, &project, &other);
            let view = catalog_diagnostics(&shared, session).await;
            assert_eq!(view["ann"]["state"], "stale");

            catalog_test_embedder(&shared, &project, &original);
            std::fs::write(root.path().join("guide.md"), "newsealedmodelwidget").unwrap();
            std::fs::write(root.path().join("duplicate.md"), "newsealedmodelwidget").unwrap();
            original.release.add_permits(2);
            call_tool(
                &shared,
                Some(session),
                "index_project",
                json!({"path": root.path()}),
            )
            .await
            .unwrap();
            let view = catalog_diagnostics(&shared, session).await;
            assert_eq!(view["docs"], 2);
            assert_eq!(view["chunks"], 2);
            assert_eq!(view["embed_coverage"], json!({"cached": 1, "total": 1}));
            assert_eq!(view["ann"]["state"], "loading");
            assert!(view["ann"]["vectors"].is_null());
            Ok(())
        })
        .unwrap();
}

#[test]
#[cfg(unix)]
fn catalog_index_and_detached_sync_warm_the_published_ann() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut policy = crate::daemon::settings::standalone()
        .unwrap()
        .into_settings();
    policy.background_active = 1;
    policy.background_pending = 0;
    let mut jobs = crate::daemon::job_service::JobRoot::new(&policy);
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "catalogwarmguide").unwrap();
        std::fs::write(root.path().join("reference.md"), "catalogwarmreference").unwrap();
        let mut shared = open_v5_index_shared(
            cache.path(),
            config.path(),
            "config_version = 2\n[dense]\nmin_vectors = 1\n[daemon]\nbackground_active = 1\nbackground_pending = 0\n",
        );
        let warm_gate = Arc::get_mut(&mut shared).unwrap().dense.pause_job_builds_for_test();
        shared.activate_jobs(service).unwrap();
        let project = registry::ensure_project(&mut shared.db(), root.path()).unwrap();
        let provider = document_gate_for("test/catalog-warm@d3@l2");
        catalog_test_embedder(&shared, &project, &provider);
        provider.release.add_permits(3);
        let (session, _cancel) = shared
            .sessions
            .join(
                std::process::id(),
                project.canonical_root.clone(),
                Some(project.id),
            )
            .unwrap();
        let wait_for_background = || async {
            tokio::time::timeout(
                Duration::from_secs(8),
                shared
                    .activity
                    .drain(Duration::from_millis(policy.drain_poll_ms)),
            )
            .await
            .unwrap();
        };

        let reply = tokio::time::timeout(Duration::from_secs(8), call_tool(
            &shared, Some(session), "index_project", json!({"path": root.path()}),
        )).await;
        let warm_active = shared.activity.counter.load(Ordering::SeqCst) > 0;
        warm_gate.open();
        reply.unwrap().unwrap();
        assert!(warm_active);
        wait_for_background().await;
        let view = catalog_diagnostics(&shared, session).await;
        assert_eq!(view["ann"]["vectors"], 2);
        assert_eq!(view["ann"]["state"], "loading");

        reopen_catalog_with_new_ann_recipe(&shared, &project, &provider);
        let reloaded = catalog_diagnostics(&shared, session).await;
        assert_eq!(reloaded["index_generation"], view["index_generation"]);
        assert!(reloaded["ann"]["vectors"].is_null());

        std::fs::write(root.path().join("new.md"), "catalogwarmnew").unwrap();
        let ack = call_tool(
            &shared,
            Some(session),
            "sync_start",
            json!({"project_id": project.id}),
        )
        .await
        .unwrap();
        assert_eq!(ack["state"], "running");
        wait_for_background().await;
        let sync = call_tool(
            &shared,
            Some(session),
            "sync_status",
            json!({"job_id": ack["job_id"]}),
        )
        .await
        .unwrap();
        assert_eq!(sync["state"], "done");
        let view = catalog_diagnostics(&shared, session).await;
        assert_eq!(view["ann"]["vectors"], 3);
        assert_eq!(provider.queries.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
fn reopen_catalog_with_new_ann_recipe(
    shared: &Shared,
    project: &Project,
    provider: &Arc<DocumentGate>,
) {
    std::fs::write(
        project.canonical_root.join(".docsbase.toml"),
        "config_version = 2\n[ann.build]\nm = 12\n",
    )
    .unwrap();
    let reopened = shared
        .open_project_config_with(project, |_| {
            Ok(HybridStages {
                embedder: Some(provider.clone()),
                ranking: Ranking::Rrf,
                ..HybridStages::default()
            })
        })
        .unwrap();
    assert_eq!(reopened.ann_build.m(), 12);
}

#[test]
#[cfg(unix)]
fn catalog_index_capacity_failure_preserves_selected_vectors() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let guide = root.path().join("guide.md");
        let old_text = "oldcatalogwidget";
        std::fs::write(&guide, old_text).unwrap();
        let mut shared = open_v5_index_shared(
            cache.path(),
            config.path(),
            "config_version = 2\n[vector_cache]\nmax_entries = 2\n",
        );
        let owner = Arc::get_mut(&mut shared).unwrap();
        let mut budget = owner.global_config.search.clone();
        budget.max_request_payload_bytes = 32 * 1024 * 1024;
        owner.payload_budget = crate::search::budget::PayloadBudget::new(budget).unwrap();
        shared.activate_jobs(service).unwrap();
        let args = json!({"path": root.path()});
        let project = registry::ensure_project(&mut shared.db(), root.path()).unwrap();
        let original = document_gate_for("test/initial-catalog-cache@d3@l2");
        catalog_test_embedder(&shared, &project, &original);
        original.release.add_permits(1);
        call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;

        let provider = document_gate_for("test/replacement-catalog-cache@d3@l2");
        let config = catalog_test_embedder(&shared, &project, &provider);
        let source = shared.vector_source_for(project.id, &config);
        let old_hash = crate::vector_cache::chunk_hash(old_text);
        let mut vectors = source.connect(cache.path()).unwrap();
        vectors
            .put("aaa/foreign-model@d3@l2", "unreferenced", &[0.0, 0.0, 1.0])
            .unwrap();
        std::fs::write(&guide, "newcatalogwidget").unwrap();
        std::fs::write(root.path().join("second.md"), "anothercatalogwidget").unwrap();

        provider.release.add_permits(2);
        let failure = tokio::time::timeout(
            Duration::from_secs(5),
            call_tool(&shared, None, "index_project", args),
        )
        .await
        .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        assert_eq!(
            vectors.get(original.model_id(), &old_hash).unwrap(),
            Some(vec![1.0, 0.0, 0.0])
        );
        assert_eq!(
            vectors.len().unwrap(),
            2,
            "staging must respect the shared entry cap"
        );
        let batches = provider.batches.lock().unwrap();
        assert_eq!(batches.len(), 2, "{failure:?}");
        let first = crate::vector_cache::chunk_hash(&batches[0][0]);
        let second = crate::vector_cache::chunk_hash(&batches[1][0]);
        assert_eq!(
            vectors.get(provider.model_id(), &first).unwrap(),
            Some(vec![1.0, 0.0, 0.0])
        );
        assert!(vectors.get(provider.model_id(), &second).unwrap().is_none());
        assert_eq!(selected_catalog_hits(&shared, &project, old_text), 1);
        assert_eq!(
            selected_catalog_hits(&shared, &project, "newcatalogwidget"),
            0
        );
        assert!(
            matches!(failure, Err(Error::Admission { .. })),
            "{failure:?}"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
#[allow(clippy::too_many_lines)] // The same live catalog exercises two failure boundaries and retry.
fn catalog_sync_embeds_before_publication_and_reuses_valid_batches_on_retry() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut jobs = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = jobs.service();
    jobs.run(&runtime, async move {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let guide = root.path().join("guide.md");
        std::fs::write(&guide, "oldcatalogwidget").unwrap();
        let db = Db::open_for_index(
            cache.path(),
            &crate::store::settings::standalone().unwrap(),
            true,
        )
        .unwrap();
        db.close().unwrap();
        let mut shared = open_v2_index_shared(cache.path(), config.path());
        let owner = Arc::get_mut(&mut shared).unwrap();
        owner.watchers = Arc::new(Watchers::with_enabled(false));
        let mut workers = owner.global_config.daemon.clone();
        workers.background_active = 1;
        owner.workers = Arc::new(
            crate::daemon::worker_admission::WorkerAdmission::with_policies(
                &workers,
                &owner.global_config.remote,
            )
            .unwrap(),
        );
        shared.activate_jobs(service).unwrap();

        let args = json!({"path": root.path()});
        call_tool(&shared, None, "index_project", args.clone())
            .await
            .unwrap();
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let project = registry::list_projects(&shared.db()).unwrap().remove(0);
        assert_eq!(
            selected_catalog_hits(&shared, &project, "oldcatalogwidget"),
            1
        );

        let provider = document_gate();
        provider.invalid_on.store(2, Ordering::SeqCst);
        let slot = shared.project_slot(project.id);
        {
            let mut state = slot.state.lock().unwrap();
            let mut config = (**state.config.as_ref().unwrap()).clone();
            config.remote.embed_batch_texts = 1;
            state.config = Some(Arc::new(config));
            state.search_stages = Some(Arc::new(HybridStages {
                embedder: Some(provider.clone()),
                ..HybridStages::default()
            }));
        }
        {
            let config = shared.project_config(&project).unwrap();
            let source = shared.vector_source_for(project.id, &config);
            let mut cache = source.connect(&shared.cache).unwrap();
            for (text, vector) in [
                ("newcatalogwidget", &[0.0, 1.0][..]),
                ("anothercatalogwidget", &[f32::NAN, 0.0, 0.0]),
            ] {
                cache
                    .put(
                        provider.model_id(),
                        &crate::vector_cache::chunk_hash(text),
                        vector,
                    )
                    .unwrap();
            }
        }
        std::fs::write(&guide, "newcatalogwidget").unwrap();
        std::fs::write(root.path().join("second.md"), "anothercatalogwidget").unwrap();
        let indexing = catalog_index_waiting_for_provider(&shared, &args, &provider).await;
        let operation = crate::search::operation::SearchOperation::with_timeout(
            Instant::now(),
            Duration::from_secs(5),
            crate::hybrid::work::Cancellation::new(),
        )
        .unwrap();
        let free_background = shared.workers.background(&operation).await;
        let free_writer =
            lifecycle::with_writer_lease(&shared.cache, project.id, || Ok(())).is_ok();
        let old_hit = selected_catalog_hits(&shared, &project, "oldcatalogwidget");
        let new_hit = selected_catalog_hits(&shared, &project, "newcatalogwidget");
        provider.release.add_permits(1);
        assert!(
            free_background.is_ok(),
            "provider must release native worker admission"
        );
        drop(free_background);
        assert!(
            free_writer,
            "provider must not hold the active pair's writer lease"
        );
        assert_eq!((old_hit, new_hit), (1, 0));
        tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        provider.release.add_permits(1);
        let error = indexing.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("invalid embedding batch"));
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        assert_eq!(
            selected_catalog_hits(&shared, &project, "oldcatalogwidget"),
            1
        );
        assert_eq!(
            selected_catalog_hits(&shared, &project, "newcatalogwidget"),
            0
        );

        provider.invalid_on.store(0, Ordering::SeqCst);
        let retry = catalog_index_waiting_for_provider(&shared, &args, &provider).await;
        {
            // The valid first batch was reused before waiting on the second.
            // Replace it now to exercise the final proof, not batch validation.
            let config = shared.project_config(&project).unwrap();
            let mut cache = shared
                .vector_source_for(project.id, &config)
                .connect(&shared.cache)
                .unwrap();
            cache
                .put(
                    provider.model_id(),
                    &crate::vector_cache::chunk_hash("newcatalogwidget"),
                    &[f32::NAN, 0.0, 0.0],
                )
                .unwrap();
        }
        provider.release.add_permits(1);
        let error = tokio::time::timeout(Duration::from_secs(5), retry)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("prepared embedding vector has invalid values")
        );
        shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        assert_eq!(
            selected_catalog_hits(&shared, &project, "oldcatalogwidget"),
            1
        );
        assert_eq!(
            selected_catalog_hits(&shared, &project, "newcatalogwidget"),
            0
        );

        provider.release.add_permits(1);
        tokio::time::timeout(
            Duration::from_secs(5),
            call_tool(&shared, None, "index_project", args),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(provider.batches.lock().unwrap().len(), 4);
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        assert_eq!(
            selected_catalog_hits(&shared, &project, "oldcatalogwidget"),
            0
        );
        assert_eq!(
            selected_catalog_hits(&shared, &project, "newcatalogwidget"),
            1
        );
        assert_eq!(
            selected_catalog_hits(&shared, &project, "anothercatalogwidget"),
            1
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn detached_sync_acknowledges_own_job_and_deduplicates_while_provider_is_held() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let provider = document_gate();
        let fixture = batch_fixture(&provider, service);
        let first = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "sync_start",
            json!({}),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let second = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "sync_start",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(first["job_id"], second["job_id"]);
        assert!(matches!(
            second["state"].as_str(),
            Some("queued" | "running")
        ));
        assert_eq!(provider.batches.lock().unwrap().len(), 1);
        provider.release.add_permits(20);
        fixture
            .shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let status = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "sync_status",
            json!({"job_id":first["job_id"]}),
        )
        .await
        .unwrap();
        assert_eq!(status["state"], "done");
        assert!(
            status["stats"]["index_outcome"].is_object(),
            "own captured association must survive detached execution: {status}"
        );
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
}
#[test]
fn count_byte_batches_reject_whole_invalid_batch_and_reuse_earlier_success_on_retry() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let provider = document_gate();
        provider.release.add_permits(100);
        provider.invalid_on.store(2, Ordering::SeqCst);
        let fixture = batch_fixture(&provider, service);
        for n in 0..5 {
            std::fs::write(
                fixture.project.canonical_root.join(format!("new-{n}.md")),
                format!("# Item {n}\n\nbody number {n}\n"),
            )
            .unwrap();
        }
        let failed = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await;
        assert!(failed.is_err());
        fixture
            .shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        let batches = provider.batches.lock().unwrap().clone();
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.len() <= 2
            && (batch.iter().map(String::len).sum::<usize>() <= 64 || batch.len() == 1)));
        let mut cache = crate::vector_cache::VectorCache::open(&fixture.shared.cache).unwrap();
        for text in &batches[0] {
            assert!(
                cache
                    .get(provider.model_id(), &crate::vector_cache::chunk_hash(text))
                    .unwrap()
                    .is_some()
            );
        }
        for text in &batches[1] {
            assert!(
                cache
                    .get(provider.model_id(), &crate::vector_cache::chunk_hash(text))
                    .unwrap()
                    .is_none(),
                "whole invalid batch must write nothing"
            );
        }
        provider.invalid_on.store(0, Ordering::SeqCst);
        let result = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "done");
        let all = provider.batches.lock().unwrap();
        let retried: Vec<_> = all[2..].iter().flatten().collect();
        assert!(
            batches[0].iter().all(|text| !retried.contains(&text)),
            "earlier valid cache batch must be reused"
        );
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
}

struct ReleaseOnDrop(Option<std::sync::mpsc::Sender<()>>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
#[test]
#[allow(clippy::too_many_lines)] // One ordered real-resource cancellation/retirement proof.
fn cancellation_transfers_native_index_after_shutdown_and_keeps_ingress_until_terminal_retirement()
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    assert_eq!(policy.settings().background_active, 1); // J=L=T=1 composition
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    let retiring = Arc::new(tokio::sync::Semaphore::new(0));
    let (release, proceed) = std::sync::mpsc::channel();
    let proceed = Mutex::new(proceed);
    service.observe_retirement(Arc::new({
        let retiring = retiring.clone();
        move |stage| {
            if stage == "before_index" {
                retiring.add_permits(1);
                proceed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }
    }));
    root.run(&runtime, async move {
        let release = ReleaseOnDrop(Some(release));
        let provider = document_gate();
        let fixture = batch_fixture(&provider, service);
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Cancel\n\nnew pending content\n",
        )
        .unwrap();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let connection = Arc::new(slots.clone().try_acquire_owned().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let (events, mut retired) = tokio::sync::mpsc::channel(1);
        let activity = Activity::new(&count, &events, Some(&connection));
        drop(connection);
        let shared = fixture.shared.clone();
        let session = fixture.session;
        let request = tokio::spawn(async move {
            call_tool_in(
                &shared,
                Some(session),
                "index_project",
                json!({}),
                None,
                Some(activity),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), provider.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        fixture.shared.workers.shutdown();
        request.abort();
        let _ = request.await;
        tokio::time::timeout(Duration::from_secs(5), retiring.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let connection_charged = slots.clone().try_acquire_owned().is_err();
        let writer_refused =
            lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                .is_err();
        let native_not_retired = count.load(Ordering::SeqCst) == 1;
        let state: String = fixture
            .shared
            .db()
            .connection()
            .query_row(
                "SELECT state FROM sync_jobs WHERE project_id=?1 ORDER BY id DESC LIMIT 1",
                [fixture.project.id],
                |r| r.get(0),
            )
            .unwrap();
        // This executes on the only Tokio worker while the native finalizer is gated.
        tokio::task::yield_now().await;
        drop(release);
        tokio::time::timeout(Duration::from_secs(5), retired.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(connection_charged && writer_refused && native_not_retired);
        assert_eq!(
            state, "error",
            "terminal publication precedes native retirement/drain"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(slots.clone().try_acquire_owned().is_ok());
        assert!(
            lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                .is_ok()
        );
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
}
struct PanickingLocal(std::sync::atomic::AtomicBool);
impl Embedder for PanickingLocal {
    fn model_id(&self) -> &'static str {
        "test/cpu-unwind@d3@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        assert!(crate::hybrid::blocking::in_context());
        assert!(
            !self.0.swap(false, Ordering::SeqCst),
            "controlled local inference unwind"
        );
        Ok(vec![vec![1.0, 0.0, 0.0]; texts.len()])
    }
}
#[test]
fn local_cpu_unwind_transfers_unique_native_state_and_retry_reprocesses_markers() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let provider = Arc::new(PanickingLocal(std::sync::atomic::AtomicBool::new(true)));
        let fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(provider),
            ..HybridStages::default()
        });
        fixture.shared.activate_jobs(service).unwrap();
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Unwind\n\nchanged CPU input\n",
        )
        .unwrap();
        let failed = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await;
        fixture
            .shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        assert!(failed.is_err());
        assert!(
            lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                .is_ok()
        );
        assert!(
            crate::store::repo::doc_states(fixture.shared.db().connection(), fixture.project.id)
                .unwrap()
                .iter()
                .any(|doc| doc.content_hash.is_empty())
        );
        let retried = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(retried["state"], "done");
        assert!(
            crate::store::repo::doc_states(fixture.shared.db().connection(), fixture.project.id)
                .unwrap()
                .iter()
                .all(|doc| !doc.content_hash.is_empty())
        );
        Ok(())
    })
    .unwrap();
}

struct NoNetwork(AtomicUsize);
impl crate::hybrid::remote::AsyncTransport for NoNetwork {
    fn post_json<'a>(
        &'a self,
        _: &'a crate::hybrid::remote::WireRequest,
        _: Duration,
    ) -> crate::hybrid::remote::StageFuture<'a, crate::hybrid::wire::TransportResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(Error::internal(
                "unexpected provider transport after cancellation",
            ))
        })
    }
}
#[test]
#[allow(clippy::too_many_lines)] // One ordered real-resource cancellation/retirement proof.
fn abandoned_actual_document_codec_holds_native_retirement_until_physical_finish_or_unwind() {
    const PHASE_WATCHDOG: Duration = Duration::from_mins(1);

    for unwind in [false, true] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = crate::daemon::settings::standalone().unwrap();
        let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
        let service = root.service();
        let native_retirements = Arc::new(AtomicUsize::new(0));
        service.observe_retirement(Arc::new({
            let count = native_retirements.clone();
            move |stage| {
                if stage == "before_index" {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        root.run(&runtime, async move {
            let fixture = search_phase_tests::Fixture::new(HybridStages::default());
            fixture.shared.activate_jobs(service).unwrap();
            let transport = Arc::new(NoNetwork(AtomicUsize::new(0)));
            let mut execution = crate::hybrid::remote::RemoteExecution::with_owners(
                &fixture.shared.global_config.remote,
                transport.clone(),
                fixture.shared.workers.clone(),
                fixture.shared.activity.clone(),
            )
            .unwrap();
            let entered = Arc::new(tokio::sync::Semaphore::new(0));
            let entered_work = entered.clone();
            let (release, proceed) = std::sync::mpsc::channel();
            let proceed = Mutex::new(proceed);
            execution.observe_cpu(Arc::new(move || {
                entered_work.add_permits(1);
                proceed
                    .lock()
                    .unwrap()
                    .recv_timeout(PHASE_WATCHDOG)
                    .unwrap();
                assert!(!unwind, "controlled real document codec unwind");
            }));
            let client = Arc::new(
                crate::hybrid::openrouter::OpenRouterClient::with_execution(
                    crate::hybrid::openrouter::BASE_URL,
                    "fixture-secret",
                    Arc::new(execution),
                )
                .unwrap(),
            );
            let embedder = Arc::new(crate::hybrid::openrouter::OpenRouterEmbedder::with_client(
                crate::hybrid::EmbeddingCfg::default(),
                client,
            ));
            fixture
                .shared
                .project_slot(fixture.project.id)
                .state
                .lock()
                .unwrap()
                .stages = Some(Arc::new(HybridStages {
                embedder: Some(embedder),
                ..HybridStages::default()
            }));
            std::fs::write(
                fixture.project.canonical_root.join("guide.md"),
                "# Codec\n\nnew encoding payload\n",
            )
            .unwrap();
            let slots = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = Arc::new(slots.clone().try_acquire_owned().unwrap());
            let activity = Activity::new(
                &fixture.shared.activity.counter,
                &fixture.shared.activity.events,
                Some(&permit),
            );
            drop(permit);
            let shared = fixture.shared.clone();
            let session = fixture.session;
            let mut request = tokio::spawn(async move {
                call_tool_in(
                    &shared,
                    Some(session),
                    "index_project",
                    json!({}),
                    None,
                    Some(activity),
                )
                .await
            });
            let release = ReleaseOnDrop(Some(release));
            tokio::select! {
                result = &mut request => {
                    panic!("index request ended before document codec admission: {result:?}");
                }
                entered = tokio::time::timeout(PHASE_WATCHDOG, entered.acquire()) => {
                    entered.expect("document codec admission").unwrap().forget();
                }
            }
            request.abort();
            let _ = request.await;
            tokio::task::yield_now().await;
            let no_native_retirement = native_retirements.load(Ordering::SeqCst) == 0;
            let ingress_held = slots.clone().try_acquire_owned().is_err();
            let source_and_ingress = fixture.shared.activity.counter.load(Ordering::SeqCst) >= 2;
            let writer_refused =
                lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                    .is_err();
            drop(release);
            fixture
                .shared
                .activity
                .drain(Duration::from_millis(policy.settings().drain_poll_ms))
                .await;
            assert!(no_native_retirement && ingress_held && source_and_ingress && writer_refused);
            assert_eq!(native_retirements.load(Ordering::SeqCst), 1);
            assert!(slots.clone().try_acquire_owned().is_ok());
            assert!(
                lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(()))
                    .is_ok()
            );
            assert_eq!(transport.0.load(Ordering::SeqCst), 0);
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn outer_root_unwind_cancels_real_held_job_and_drains_native_state_before_runtime_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    let (provider, fixture) = {
        let _entered = runtime.enter();
        let provider = document_gate();
        let fixture = batch_fixture(&provider, service);
        (provider, fixture)
    };
    std::fs::write(
        fixture.project.canonical_root.join("guide.md"),
        "# Root unwind\n\nnew detached source\n",
    )
    .unwrap();
    let shared = fixture.shared.clone();
    let session = fixture.session;
    let held = provider.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        root.run(&runtime, async move {
            drop(tokio::spawn(async move {
                let _ = call_tool(&shared, Some(session), "index_project", json!({})).await;
            }));
            tokio::time::timeout(Duration::from_secs(5), held.entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            panic!("controlled work-driver unwind with live native job");
            #[allow(unreachable_code)]
            Ok(())
        })
    }));
    assert!(result.is_err());
    assert_eq!(
        provider.release.available_permits(),
        0,
        "root drain must cancel the same provider future without requiring release"
    );
    assert_eq!(fixture.shared.activity.counter.load(Ordering::SeqCst), 0);
    assert!(
        lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(())).is_ok()
    );
    let state: String = fixture
        .shared
        .db()
        .connection()
        .query_row(
            "SELECT state FROM sync_jobs WHERE project_id=?1 ORDER BY id DESC LIMIT 1",
            [fixture.project.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "error");
    drop(root);
    drop(fixture);
    drop(provider);
    drop(runtime);
}

fn lexical_context(
    fixture: &search_phase_tests::Fixture,
    input: crate::index::async_job::JobInput<'static>,
) -> crate::index::async_job::JobContext {
    let (config, stages) = fixture.shared.project_runtime(&fixture.project).unwrap();
    let source = fixture
        .shared
        .vector_source_for(fixture.project.id, &config);
    crate::index::async_job::JobContext {
        input,
        singleton: None,
        cache: fixture.shared.cache.clone(),
        db_settings: fixture.shared.db_settings.clone(),
        project: fixture.project.clone(),
        config,
        stages,
        source,
        revision: Some(fixture.shared.project_slot(fixture.project.id)),
        dense: Some(Arc::new(fixture.shared.dense.snapshot())),
    }
}
#[test]
fn async_incremental_subset_noop_and_recreated_full_recovery_preserve_lexical_contracts() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let fixture = search_phase_tests::Fixture::new(HybridStages::default());
        let added = fixture.project.canonical_root.join("added.md");
        std::fs::write(&added, "# Added\n\nnew independent document\n").unwrap();
        let run = |input| {
            let context = lexical_context(&fixture, input);
            let operation = crate::search::operation::SearchOperation::with_timeout(
                Instant::now(),
                Duration::from_secs(60),
                crate::hybrid::work::Cancellation::new(),
            )
            .unwrap();
            crate::index::async_job::run(
                &service,
                &fixture.shared.workers,
                fixture.shared.activity.track(),
                operation,
                &fixture.shared.payload_budget,
                context,
            )
        };
        let subset = run(crate::index::async_job::JobInput::Incremental(
            vec![added.clone()].into(),
        ))
        .await
        .unwrap();
        assert_eq!(subset["docs"], 1);
        let generation = crate::store::repo::index_generation(
            fixture.shared.db().connection(),
            fixture.project.id,
        )
        .unwrap();
        let outcome = crate::index::outcome::read(&fixture.shared.db(), fixture.project.id)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.scope, crate::index::outcome::SurveyScope::Subset);
        assert!(!outcome.is_complete());
        let no_op = run(crate::index::async_job::JobInput::Full).await.unwrap();
        assert_eq!(no_op["skipped"], 2);
        assert_eq!(
            crate::store::repo::index_generation(
                fixture.shared.db().connection(),
                fixture.project.id
            )
            .unwrap(),
            generation
        );
        // Corrupt derived cache BEFORE job capture; incremental must become a full survey.
        std::fs::write(
            lifecycle::index_dir(&fixture.shared.cache, fixture.project.id).join("meta.json"),
            "broken derived index",
        )
        .unwrap();
        let rebuilt = run(crate::index::async_job::JobInput::Incremental(
            vec![added].into(),
        ))
        .await
        .unwrap();
        assert_eq!(rebuilt["docs"], 2);
        let outcome = crate::index::outcome::read(&fixture.shared.db(), fixture.project.id)
            .unwrap()
            .unwrap();
        assert!(outcome.is_complete());
        let count: i64 = fixture
            .shared
            .db()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sync_jobs WHERE project_id=?1",
                [fixture.project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "lexical callers must not manufacture Sync rows");
        assert_eq!(
            crate::store::repo::doc_states(fixture.shared.db().connection(), fixture.project.id)
                .unwrap()
                .len(),
            2
        );
        Ok(())
    })
    .unwrap();
}

struct OpenBuildOnDrop(crate::hybrid::dense::TestBuildGate);
impl Drop for OpenBuildOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

#[test]
fn async_sync_hands_the_resolved_recipe_to_the_physical_ann_worker() {
    use crate::hybrid::ann_settings::AnnBuildSettings;
    use crate::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore, chunk_set_digest};
    use crate::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let provider = document_gate();
        let stages = || HybridStages {
            embedder: Some(provider.clone()),
            ranking: Ranking::Rrf,
            reranker: None,
        };
        let mut fixture = search_phase_tests::Fixture::new(stages());
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        shared.dense = DenseCache::with_policy_limits(
            AnnPolicy::BuildOrLoad,
            &shared.cache,
            DenseCacheLimits {
                min_vectors: 1,
                ..DenseCacheLimits::default()
            },
        );
        fixture.shared.activate_jobs(service).unwrap();
        std::fs::write(
            fixture.project.canonical_root.join(".docsbase.toml"),
            "[ann.build]\nm=12\nef_construction=64\n",
        )
        .unwrap();
        let config = fixture
            .shared
            .open_project_config_with(&fixture.project, |_| Ok(stages()))
            .unwrap();
        assert_eq!(config.ann_build, AnnBuildSettings::new(12, 64).unwrap());
        provider.release.add_permits(1);
        let result = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "done");
        let dense = fixture.shared.dense.snapshot();
        tokio::task::spawn_blocking(move || dense.join_job_builds_for_test())
            .await
            .unwrap()
            .unwrap();

        let project_id = fixture.project.id;
        let db = fixture.shared.db();
        let chunks = crate::store::repo::chunk_hashes(db.connection(), project_id).unwrap();
        let key = AnnKey {
            project_id,
            generation: crate::store::repo::index_generation(db.connection(), project_id).unwrap(),
            model_id: provider.model_id().to_owned(),
            dim: 3,
            chunk_count: chunks.len(),
            chunk_set_digest: chunk_set_digest(&chunks),
            format_version: ANN_FORMAT_VERSION,
        };
        let store = AnnStore::new(fixture.shared.cache.clone());
        let slot = store
            .load_with_settings(&key, config.ann_build)
            .unwrap()
            .expect("async sync must publish the captured recipe");
        assert_eq!((slot.meta.m, slot.meta.ef_construction), (12, 64));
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn captured_ttl_and_exact_ann_handoff_isolate_later_job_from_old_worker_cleanup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let (retired, terminal_finished) = tokio::sync::oneshot::channel();
        let retired = Mutex::new(Some(retired));
        service.observe_retirement(Arc::new(move |event| {
            if event == "terminal_finished" && let Some(retired) = retired.lock().unwrap().take() { let _ = retired.send(()); }
        }));
        let provider=document_gate();
        let mut fixture=search_phase_tests::Fixture::new(HybridStages{embedder:Some(provider.clone()),ranking:Ranking::Rrf,reranker:None});

        let shared=Arc::get_mut(&mut fixture.shared).unwrap();
        let mut dense=crate::hybrid::dense_settings::standalone().unwrap().into_settings();dense.min_vectors=1;
        dense.ann_lifetimes=crate::hybrid::dense_settings::AnnLifetimes::new(Duration::from_secs(123),Duration::from_secs(456)).unwrap();
        shared.dense=crate::hybrid::dense::DenseCache::with_settings(crate::hybrid::dense::AnnPolicy::BuildOrLoad,&shared.cache,&dense).unwrap();
        let build=shared.dense.pause_job_builds_for_test();let _open_on_drop=OpenBuildOnDrop(build.clone());
        fixture.shared.activate_jobs(service).unwrap();
        std::fs::write(fixture.project.canonical_root.join("guide.md"),"# First owner\n\nfirst job chunk\n").unwrap();
        let before=i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()).unwrap();
        let shared=fixture.shared.clone();let session=fixture.session;
        let first=tokio::spawn(async move {call_tool(&shared,Some(session),"index_project",json!({})).await});
        tokio::time::timeout(Duration::from_secs(5),provider.entered.acquire()).await.unwrap().unwrap().forget();
        let pins=||->Vec<(i64,i64)>{let conn=rusqlite::Connection::open(fixture.shared.cache.join(crate::vector_cache::VECTOR_DB_FILE)).unwrap();let mut statement=conn.prepare("SELECT lease_id,MAX(expires_at) FROM vector_cache_leases WHERE lease_id<0 AND lease_id<>?1 GROUP BY lease_id ORDER BY lease_id").unwrap();statement.query_map([i64::MIN],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()};
        let after=i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()).unwrap();
        let initial=pins();assert_eq!(initial.len(),1);assert_eq!(initial[0].0,-1);assert!((before+123..=after+123).contains(&initial[0].1),"TTL must come from captured DenseSpec");
        provider.release.add_permits(1);assert_eq!(first.await.unwrap().unwrap()["state"],"done");
        assert_eq!(pins().iter().map(|row|row.0).collect::<Vec<_>>(),vec![-1],"completed job moved authority into physically retained ANN worker");
        tokio::time::timeout(Duration::from_secs(5), terminal_finished).await.unwrap().unwrap();
        let ann_retained_activity = fixture.shared.activity.counter.load(Ordering::SeqCst);

        std::fs::write(fixture.project.canonical_root.join("guide.md"),"# Second owner\n\nsecond job new chunk\n").unwrap();
        let shared=fixture.shared.clone();let session=fixture.session;
        let second=tokio::spawn(async move {call_tool(&shared,Some(session),"index_project",json!({})).await});
        tokio::time::timeout(Duration::from_secs(5),provider.entered.acquire()).await.unwrap().unwrap().forget();
        assert_eq!(pins().iter().map(|row|row.0).collect::<Vec<_>>(),vec![-2,-1]);
        let join=fixture.shared.dense.snapshot();let call=crate::search::operation::SearchOperation::with_timeout(Instant::now(),Duration::from_secs(60),crate::hybrid::work::Cancellation::new()).unwrap();
        let permit=fixture.shared.workers.background(&call).await.unwrap();
        request::spawn(Some(permit),Some(fixture.shared.activity.track()),move || {build.open();join.join_job_builds_for_test()}).await.unwrap().unwrap();
        assert_eq!(pins().iter().map(|row|row.0).collect::<Vec<_>>(),vec![-2],"old ANN cleanup must release only the exact moved authority");
        provider.release.add_permits(1);assert_eq!(second.await.unwrap().unwrap()["state"],"done");
        let join=fixture.shared.dense.snapshot();let permit=fixture.shared.workers.background(&call).await.unwrap();
        request::spawn(Some(permit),Some(fixture.shared.activity.track()),move ||join.join_job_builds_for_test()).await.unwrap().unwrap();
        assert_eq!(pins(),Vec::<(i64,i64)>::new());
        assert_eq!(provider.synchronous.load(Ordering::SeqCst),0);
        assert_eq!(ann_retained_activity, 1, "the original Activity must outlive terminal CPU retirement while the actual ANN worker owns its rows/pin");
        Ok(())
    }).unwrap();
}

#[test]
fn lexical_full_embeds_changed_documents_without_backfilling_unchanged_offline_chunks() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let provider=document_gate();provider.release.add_permits(100);
        let fixture=batch_fixture(&provider,service.clone());
        std::fs::write(fixture.project.canonical_root.join("new.md"),"# New changed document\n\nnew input only\n").unwrap();
        let context=lexical_context(&fixture,crate::index::async_job::JobInput::Full);
        let operation=crate::search::operation::SearchOperation::with_timeout(Instant::now(),Duration::from_secs(60),crate::hybrid::work::Cancellation::new()).unwrap();
        let result=crate::index::async_job::run(&service,&fixture.shared.workers,fixture.shared.activity.track(),operation,&fixture.shared.payload_budget,context).await.unwrap();
        assert_eq!(result["docs"],1);
        let batches=provider.batches.lock().unwrap();
        assert!(!batches.iter().flatten().any(|text|text.contains("old widgets text")),"full lexical indexing must preserve the existing separate explicit Sync/backfill boundary");
        Ok(())
    }).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)] // One real cold-flight follower/revision/read-lease boundary.
fn terminal_ann_follower_releases_revision_and_project_read_leases_before_waiting() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let provider=document_gate();
        let mut fixture=search_phase_tests::Fixture::new(HybridStages{embedder:Some(provider.clone()),ranking:Ranking::Rrf,reranker:None});
        let shared=Arc::get_mut(&mut fixture.shared).unwrap();
        let mut policy=crate::hybrid::dense_settings::standalone().unwrap().into_settings();policy.min_vectors=1;
        shared.dense=crate::hybrid::dense::DenseCache::with_settings(crate::hybrid::dense::AnnPolicy::BuildOrLoad,&shared.cache,&policy).unwrap();
        fixture.shared.activate_jobs(service).unwrap();
        let (selected,started)=tokio::sync::oneshot::channel();let selected=Mutex::new(Some(selected));
        let (release,proceed)=std::sync::mpsc::channel();let proceed=Mutex::new(proceed);
        fixture.shared.dense.observe_cold_start(Arc::new(move || {selected.lock().unwrap().take().unwrap().send(()).unwrap();proceed.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();}));
        let (joined,joins)=std::sync::mpsc::channel();fixture.shared.dense.observe_cold_joins(joined);
        let (config,_)=fixture.shared.project_runtime(&fixture.project).unwrap();
        let slot=fixture.shared.project_slot(fixture.project.id);
        let dense=fixture.shared.dense.snapshot();let cache=fixture.shared.cache.clone();let settings=fixture.shared.db_settings.clone();let id=fixture.project.id;
        let generation=crate::store::repo::index_generation(fixture.shared.db().connection(),id).unwrap();
        let model=provider.model_id().to_owned();let leader_slot=slot.clone();let leader_config=config.clone();
        let leader=std::thread::spawn(move ||dense.warm_with_metadata_admitted(crate::hybrid::dense::DenseRequest{cache_root:&cache,project_id:id,generation,model_id:&model},&crate::hybrid::work::Cancellation::new(),||lifecycle::with_project_read_lease(&cache,id,||{let db=Db::open_with_settings(&cache,&settings)?;crate::hybrid::dense_metadata::DenseMetadata::capture(&db,id)}),||leader_slot.preparation_lease(&leader_config)));
        let release=ReleaseOnDrop(Some(release));started.await.unwrap();
        let shared=fixture.shared.clone();let session=fixture.session;
        let index=tokio::spawn(async move {call_tool(&shared,Some(session),"index_project",json!({})).await});
        tokio::time::timeout(Duration::from_secs(5),provider.entered.acquire()).await.unwrap().unwrap().forget();provider.release.add_permits(1);
        tokio::task::spawn_blocking(move || {joins.recv_timeout(Duration::from_secs(5)).unwrap();joins.recv_timeout(Duration::from_secs(5)).unwrap();}).await.unwrap();
        let revision_free=slot.preparation.try_write().is_ok();
        let native_writer_free=lifecycle::with_writer_lease(&fixture.shared.cache,id,||Ok(())).is_ok();
        // Always let the elected leader and actual terminal follower retire before assertions.
        drop(release);
        tokio::task::spawn_blocking(move ||leader.join().unwrap()).await.unwrap().unwrap();
        let result=index.await.unwrap();
        let dense=fixture.shared.dense.snapshot();tokio::task::spawn_blocking(move ||dense.join_job_builds_for_test()).await.unwrap().unwrap();
        assert!(revision_free,"terminal cold-flight follower must not retain preparation READ while waiting for the elected leader");
        assert!(native_writer_free,"terminal cold-flight follower must not retain project read lease while waiting");
        assert_eq!(result.unwrap()["state"],"done");
        Ok(())
    }).unwrap();
}

struct FactoryWire {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    calls: AtomicUsize,
}
impl crate::hybrid::remote::AsyncTransport for FactoryWire {
    fn post_json<'a>(
        &'a self,
        request: &'a crate::hybrid::remote::WireRequest,
        _: Duration,
    ) -> StageFuture<'a, crate::hybrid::wire::TransportResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            let request: Value = serde_json::from_str(&request.body).unwrap();
            let count = request["input"].as_array().unwrap().len();
            let dimension = usize::try_from(request["dimensions"].as_u64().unwrap()).unwrap();
            let data: Vec<_> = (0..count)
                .map(|index| {
                    let mut vector = vec![0.0_f32; dimension];
                    vector[0] = 1.0;
                    json!({"index":index,"embedding":vector})
                })
                .collect();
            Ok(crate::hybrid::wire::TransportResponse{status:200,body:json!({"object":"list","data":data,"model":crate::hybrid::config::DEFAULT_EMBEDDING_MODEL,"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string(),retry_after:None})
        })
    }
}
#[test]
#[allow(clippy::too_many_lines)] // One isolated real factory/codec/HTTP ownership proof.
fn actual_background_remote_factory_is_async_and_held_http_releases_cpu() {
    const CHILD: &str = "DOCSBASE_B1_ACTUAL_FACTORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","daemon::server::async_index_tests::actual_background_remote_factory_is_async_and_held_http_releases_cpu","--nocapture"]).env(CHILD,"1").env("OPENROUTER_API_KEY","fixture-secret").output().unwrap();
        assert!(
            output.status.success(),
            "isolated actual factory boundary failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let mut fixture = search_phase_tests::Fixture::new(HybridStages::default());

        let wire = Arc::new(FactoryWire {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            calls: AtomicUsize::new(0),
        });
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut daemon = shared.global_config.daemon.clone();
        daemon.control_active = 1;
        shared.workers = Arc::new(
            crate::daemon::worker_admission::WorkerAdmission::with_policies(
                &daemon,
                &shared.global_config.remote,
            )
            .unwrap(),
        );
        shared.remote = Arc::new(
            crate::hybrid::remote::RemoteExecution::with_owners(
                &shared.global_config.remote,
                wire.clone(),
                shared.workers.clone(),
                shared.activity.clone(),
            )
            .unwrap(),
        );
        let slot = shared.project_slot(fixture.project.id);
        let mut config = Config {
            hybrid_cfg: crate::hybrid::HybridCfg::enabled_with_defaults(),
            ..Config::default()
        };
        config.hybrid_cfg.reranker = None;
        {
            let mut state = slot.state.lock().unwrap();
            state.config = Some(Arc::new(config));
            state.stages = None;
            state.search_stages = None;
        }
        fixture.shared.activate_jobs(service).unwrap();
        // Exercise the REAL configured provider registry/client constructor on a
        // cold stage cache. Stop before any provider call if legacy was selected.
        let owner = fixture.shared.clone();
        let session = fixture.session;
        let permit = owner.workers.control().await.unwrap();
        let context = request::spawn(Some(permit), Some(owner.activity.track()), move || {
            prepare_index_context(&owner, Some(session), &json!({}), false)
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert!(
            context
                .stages
                .embedder
                .as_ref()
                .unwrap()
                .async_remote()
                .is_some(),
            "actual configured background factory selected legacy sync transport/bridge"
        );
        drop(context);
        // B2: the real watcher constructor must use the async cache even when
        // the separate legacy cache is cold. Inspect before any provider send.
        let owner = fixture.shared.clone();
        let project = fixture.project.clone();
        let permit = owner.workers.control().await.unwrap();
        request::spawn(Some(permit), Some(owner.activity.track()), move || {
            owner.ensure_watcher(&project)
        })
        .await
        .unwrap()
        .unwrap();
        let (_, stages, _) = fixture
            .shared
            .watchers
            .runtime_for_test(fixture.project.id)
            .unwrap();
        assert!(
            stages.embedder.as_ref().unwrap().async_remote().is_some(),
            "actual watcher factory selected legacy synchronous stages"
        );
        assert_eq!(wire.calls.load(Ordering::SeqCst), 0);
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Actual factory\n\nnew document role input\n",
        )
        .unwrap();
        let owner = fixture.shared.clone();
        let index = tokio::spawn(async move {
            call_tool(&owner, Some(session), "index_project", json!({})).await
        });
        tokio::time::timeout(Duration::from_secs(5), wire.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let call = crate::search::operation::SearchOperation::with_timeout(
            Instant::now(),
            Duration::from_secs(5),
            crate::hybrid::work::Cancellation::new(),
        )
        .unwrap();
        let physical = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.shared.workers.background(&call),
        )
        .await;
        let control = call_tool(&fixture.shared, Some(session), "list_projects", json!({})).await;
        let cpu_free = physical.as_ref().is_ok_and(std::result::Result::is_ok);
        drop(physical);
        wire.release.add_permits(20);
        let result = index.await.unwrap();
        assert!(cpu_free);
        assert!(control.is_ok());
        assert_eq!(result.unwrap()["state"], "done");
        assert_eq!(wire.calls.load(Ordering::SeqCst), 1);
        Ok(())
    })
    .unwrap();
}

#[path = "async_index_budget_tests.rs"]
mod budget_regressions;

#[test]
fn actual_watcher_held_remote_uses_async_consumer_and_coalesces_repeated_changes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let provider=document_gate();
        let fixture=search_phase_tests::Fixture::new(HybridStages{embedder:Some(provider.clone()),..HybridStages::default()});
        fixture.shared.activate_jobs(service).unwrap();
        // Prepared dependency gate through the actual production watcher. The
        // separate cold-factory acceptance must exercise configured construction.
        let slot=fixture.shared.project_slot(fixture.project.id);
        {let mut state=slot.state.lock().unwrap();state.search_stages=state.stages.clone();}
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        let path=fixture.project.canonical_root.join("guide.md");
        std::fs::write(&path,"# Watch\n\nwatcherfirst\n").unwrap();
        tokio::time::timeout(Duration::from_secs(10),provider.entered.acquire()).await.unwrap().unwrap().forget();
        let synchronous_while_held=provider.synchronous.load(Ordering::SeqCst);
        let control=fixture.shared.workers.control().await.unwrap();drop(control);
        for index in 0..16 {std::fs::write(&path,format!("# Watch\n\nwatcherlatest {index}\n")).unwrap();}
        provider.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10),provider.entered.acquire()).await.unwrap().unwrap().forget();
        provider.release.add_permits(32);
        let source_hash=blake3::hash(b"# Watch\n\nwatcherlatest 15\n").to_hex().to_string();
        tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                let current:Option<String>=fixture.shared.db().connection().query_row("SELECT content_hash FROM docs WHERE project_id=?1 AND rel_path='guide.md'",[fixture.project.id],|row|row.get(0)).ok();
                if current.as_deref()==Some(source_hash.as_str()) && lifecycle::with_writer_lease(&fixture.shared.cache,fixture.project.id,||Ok(())).is_ok() {break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let batches=provider.batches.lock().unwrap().clone();
        assert_eq!(synchronous_while_held,0,"actual watcher parked its consumer thread in the synchronous provider path while remote work was held");
        assert_eq!(batches.len(),2,"repeated same-path producer changes coalesce and unchanged followups do not embed again");
        assert!(batches[1].iter().any(|text|text.contains("watcherlatest 15")));
        let rows:i64=fixture.shared.db().connection().query_row("SELECT COUNT(*) FROM sync_jobs WHERE project_id=?1",[fixture.project.id],|row|row.get(0)).unwrap();
        assert_eq!(rows,0,"successful lexical watcher work does not fabricate a Sync row");
        Ok(())
    }).unwrap();
}

#[path = "async_shutdown_tests.rs"]
mod async_shutdown_tests;

#[test]
#[allow(clippy::too_many_lines)] // One isolated real configured registration/backfill boundary.
fn automatic_registration_and_backfill_use_configured_async_factory() {
    const CHILD: &str = "DOCSBASE_B2_AUTO_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","daemon::server::async_index_tests::automatic_registration_and_backfill_use_configured_async_factory","--nocapture"]).env(CHILD,"1").env("OPENROUTER_API_KEY","fixture-secret").output().unwrap();
        assert!(
            output.status.success(),
            "isolated automatic route failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for registered in [true, false] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = crate::daemon::settings::standalone().unwrap();
        let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
        let service = root.service();
        root.run(&runtime,async move {
            let mut fixture=search_phase_tests::Fixture::new(HybridStages::default());
            let new_root=tempfile::tempdir().unwrap();
            let project_root=if registered {fixture.project.canonical_root.clone()} else {new_root.path().to_path_buf()};
            std::fs::create_dir_all(project_root.join(".git")).unwrap();
            std::fs::write(project_root.join("guide.md"),"# Automatic\n\nautomaticwidget\n").unwrap();
            std::fs::write(project_root.join(".docsbase.toml"),"auto_index=true\n[hybrid]\nenabled=true\nranking='lexical'\n[hybrid.embedding]\nprovider='openrouter'\nmodel='fixture'\ndimensions=3\nnormalize=true\n").unwrap();
            let wire=Arc::new(FactoryWire{entered:tokio::sync::Semaphore::new(0),release:tokio::sync::Semaphore::new(0),calls:AtomicUsize::new(0)});
            let owner=Arc::get_mut(&mut fixture.shared).unwrap();
            let mut daemon=owner.global_config.daemon.clone();daemon.control_active=1;daemon.control_pending=1;
            owner.workers=Arc::new(crate::daemon::worker_admission::WorkerAdmission::with_policies(&daemon,&owner.global_config.remote).unwrap());
            owner.remote=Arc::new(crate::hybrid::remote::RemoteExecution::with_owners(&owner.global_config.remote,wire.clone(),owner.workers.clone(),owner.activity.clone()).unwrap());
            fixture.shared.activate_jobs(service).unwrap();
            let shared=fixture.shared.clone();let cwd=project_root.clone();
            let mut registration=tokio::spawn(async move { register_session(&shared,std::process::id(),&cwd,false).await });
            let mut completed=None;
            let reached=tokio::select! {
                result=wire.entered.acquire() => {result.unwrap().forget();true},
                result=&mut registration => {
                    let result=result.unwrap();let success=result.is_ok();completed=Some(result);
                    if success {tokio::time::timeout(Duration::from_secs(10),wire.entered.acquire()).await.unwrap().unwrap().forget();true} else {false}
                }
            };
            let control=if reached {Some(tokio::time::timeout(Duration::from_secs(10),fixture.shared.workers.control()).await)} else {None};
            let independent=control.as_ref().is_some_and(|result|result.as_ref().is_ok_and(std::result::Result::is_ok));drop(control);
            wire.release.add_permits(32);
            let outcome=match completed {Some(result)=>result,None=>registration.await.unwrap()};
            eprintln!("automatic registered={registered}, reached_remote={reached}, calls={}",wire.calls.load(Ordering::SeqCst));
            assert!(reached,"valid configured automatic route did not reach its async remote gate (registered={registered}): {outcome:?}");
            let (_,value,_)=outcome.unwrap();assert_eq!(value["status"],"indexed");
            assert!(independent,"automatic remote wait must release the only control worker");
            fixture.shared.activity.drain(Duration::from_millis(policy.settings().drain_poll_ms)).await;
            assert!(wire.calls.load(Ordering::SeqCst)>0);
            Ok(())
        }).unwrap();
    }
}

#[path = "../watch/async_runtime_tests.rs"]
mod async_runtime_tests;

#[test]
fn cached_search_without_job_pin_keeps_ann_in_root_drain_through_finish_or_unwind() {
    use std::future::Future;
    use std::task::Poll;
    for unwind in [false, true] {
        let (prepared, ready) = std::sync::mpsc::channel();
        let (returned, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let policy = crate::daemon::settings::standalone().unwrap();
            let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
            let (fixture,gate)=runtime.block_on(async {
                let provider=document_gate();let mut fixture=search_phase_tests::Fixture::new(HybridStages{embedder:Some(provider.clone()),ranking:Ranking::Rrf,reranker:None});
                let shared=Arc::get_mut(&mut fixture.shared).unwrap();let mut settings=crate::hybrid::dense_settings::standalone().unwrap().into_settings();settings.min_vectors=1;
                shared.dense=crate::hybrid::dense::DenseCache::with_settings(crate::hybrid::dense::AnnPolicy::BuildOrLoad,&shared.cache,&settings).unwrap();
                let gate=shared.dense.pause_job_builds_for_test();let mut vectors=crate::vector_cache::VectorCache::open(&shared.cache).unwrap();
                let db=shared.db();let mut query=db.connection().prepare("SELECT c.text FROM chunks c JOIN docs d ON d.id=c.doc_id WHERE d.project_id=?1").unwrap();
                let texts=query.query_map([fixture.project.id],|row|row.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();assert_ne!(texts,Vec::<String>::new());
                for text in &texts{vectors.put(provider.model_id(),&crate::vector_cache::chunk_hash(text),&[1.,0.,0.]).unwrap();}
                drop(query);drop(db);drop(vectors);fixture.shared.activate_jobs(root.service()).unwrap();(fixture,gate)
            });
            let result = root.run(&runtime, async {
                let response = call_tool(
                    &fixture.shared,
                    Some(fixture.session),
                    "search_docs",
                    json!({"query":"widgets"}),
                )
                .await?;
                assert_eq!(response.as_array().unwrap().len(), 1);
                let count = fixture.shared.activity.counter.load(Ordering::SeqCst);
                let mut drain = Box::pin(fixture.shared.activity.drain(Duration::from_millis(1)));
                let pending =
                    std::future::poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending()))
                        .await;
                drop(drain);
                if unwind {
                    fixture.shared.dense.invalidate(fixture.project.id).unwrap();
                }
                prepared.send((gate.clone(), count, pending)).unwrap();
                Ok(())
            });
            returned.send(result.is_ok()).unwrap();
            let joined = fixture.shared.dense.join_job_builds_for_test();
            assert_eq!(joined.is_err(), unwind);
            assert_eq!(fixture.shared.activity.counter.load(Ordering::SeqCst), 0);
        });
        let (gate, count, pending) = ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let early = done.try_recv();
        let root_waiting = matches!(early, Err(std::sync::mpsc::TryRecvError::Empty));
        if unwind {
            gate.open_with_unwind();
        } else {
            gate.open();
        }
        let completed = match early {
            Ok(value) => value,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                done.recv_timeout(Duration::from_secs(10)).unwrap()
            }
            Err(error) => panic!("root delivery: {error}"),
        };
        assert!(completed);
        worker.join().unwrap();
        assert_eq!(
            count, 1,
            "actual search-origin ANN owns the SAME Activity without a JobPin"
        );
        assert!(
            pending && root_waiting,
            "root source drain must remain pending after the real response"
        );
    }
}
