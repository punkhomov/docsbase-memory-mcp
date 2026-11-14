//! Actual index dispatch/provider gates for resumable background work.
use super::*;
use crate::hybrid::remote::{AsyncEmbedder, RemoteCall, StageFuture};
use crate::vector_cache::Embedder;

struct DocumentGate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    legacy: tokio::runtime::Handle,
    synchronous: AtomicUsize,
    batches: Mutex<Vec<Vec<String>>>,
    invalid_on: AtomicUsize,
}
impl Embedder for DocumentGate {
    fn model_id(&self) -> &'static str {
        "test/async-documents@d3@l2"
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
        Box::pin(async { Ok(vec![1.0, 0.0, 0.0]) })
    }
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
        let provider = Arc::new(DocumentGate {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            legacy: tokio::runtime::Handle::current(),
            synchronous: AtomicUsize::new(0),
            batches: Mutex::new(vec![]),
            invalid_on: AtomicUsize::new(0),
        });
        let mut fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(provider.clone()),
            ..HybridStages::default()
        });
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Guide\n\nchanged widget document\n",
        )
        .unwrap();
        fixture.shared.activate_jobs(service).unwrap();
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
        fixture.shared.activate_jobs(service).unwrap();
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut budget = shared.global_config.search.clone();
        budget.max_request_payload_bytes = 16 * 1024;
        budget.max_retained_payload_bytes = 32 * 1024;
        shared.payload_budget = crate::search::budget::PayloadBudget::new(budget).unwrap();
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
    Arc::new(DocumentGate {
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
                    .recv_timeout(Duration::from_secs(5))
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
            let release = ReleaseOnDrop(Some(release));
            tokio::time::timeout(Duration::from_secs(5), entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
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
fn captured_ttl_and_exact_ann_handoff_isolate_later_job_from_old_worker_cleanup() {
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
        fixture.shared.activate_jobs(service).unwrap();
        let shared=Arc::get_mut(&mut fixture.shared).unwrap();
        let mut dense=crate::hybrid::dense_settings::standalone().unwrap().into_settings();dense.min_vectors=1;
        dense.ann_lifetimes=crate::hybrid::dense_settings::AnnLifetimes::new(Duration::from_secs(123),Duration::from_secs(456)).unwrap();
        shared.dense=crate::hybrid::dense::DenseCache::with_settings(crate::hybrid::dense::AnnPolicy::BuildOrLoad,&shared.cache,&dense).unwrap();
        let build=shared.dense.pause_job_builds_for_test();let _open_on_drop=OpenBuildOnDrop(build.clone());
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
