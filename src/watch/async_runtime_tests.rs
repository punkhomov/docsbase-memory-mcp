//! Actual daemon watcher retry/refilter boundary, controlled by the real lease result.
use super::*;

#[test]
fn watcher_directory_limit_publishes_partial_indexing_diagnostics() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let fixture = search_phase_tests::Fixture::with_global(
            HybridStages::default(),
            "config_version=2\n[watch]\ndebounce_ms=10\nmax_debounce_ms=20\nmax_expand_files=1\n",
        );
        fixture.shared.activate_jobs(service).unwrap();
        let dir = fixture.project.canonical_root.join("incoming.md");
        std::fs::create_dir(&dir).unwrap();
        for name in ["first.md", "second.md"] {
            std::fs::write(dir.join(name), "# Arrival\n\nimported queue document\n").unwrap();
        }
        let empty = fixture.project.canonical_root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        fixture.shared.watchers.use_manual_events();
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        // Use just this directory event. A replayed native notification for
        // a child would admit that file independently of the directory limit.
        let project = fixture.project.id;
        fixture.shared.watchers.deliver_test_path(project, dir);
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let outcome = crate::index::outcome::read(&fixture.shared.db(), project).unwrap();
                if let Some(outcome) = outcome
                    && outcome.scope == crate::index::outcome::SurveyScope::Subset
                {
                    break outcome;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome.stats.docs, 1);
        assert!(
            outcome
                .stats
                .warnings
                .iter()
                .any(|warning| warning.message.contains("docsbase sync"))
        );
        let (config, stages) = fixture.shared.project_runtime(&fixture.project)?;
        let watch = Arc::new(crate::watch::WatchRuntime::new(
            config.clone(),
            stages,
            fixture.shared.vector_source_for(project, &config),
        ));
        let operation = fixture.shared.watch_operation()?;
        let (_, mut queue) = tokio::sync::mpsc::channel(1);
        let pending = crate::watch::merge_pending(
            &fixture.shared.payload_budget,
            None,
            Some(vec![empty]),
            &mut queue,
            &operation,
            &fixture.shared.watchers,
        )?;
        fixture
            .shared
            .run_watch_batch(
                fixture.project.clone(),
                watch,
                pending,
                operation,
                fixture.shared.activity.track(),
            )
            .await?;
        let after = crate::index::outcome::read(&fixture.shared.db(), project)?.unwrap();
        assert_eq!(after.attempt_id, outcome.attempt_id);
        assert_eq!(after.stats.warnings, outcome.stats.warnings);
        Ok(())
    })
    .unwrap();
}

#[test]
fn actual_watch_retry_deadline_reports_the_pending_paths() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let fixture = search_phase_tests::Fixture::with_global(
            HybridStages::default(),
            "config_version=2
[remote]
background_timeout_ms=3000
[watch]
debounce_ms=10
max_debounce_ms=20
lease_retry_ms=6000
",
        );
        fixture.shared.activate_jobs(service).unwrap();
        let (events, mut reported) = tokio::sync::mpsc::unbounded_channel();
        fixture.shared.watchers.observe_batches(Arc::new(move |event| { let _ = events.send(event); }));
        let (entered, held) = tokio::sync::oneshot::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let cache = fixture.shared.cache.clone();
        let id = fixture.project.id;
        let lease = std::thread::spawn(move || {
            lifecycle::with_writer_lease(&cache, id, || {
                entered.send(()).unwrap();
                proceed.recv_timeout(Duration::from_secs(15)).unwrap();
                Ok(())
            })
        });
        held.await.unwrap();
        let release = ReleaseOnDrop(Some(release));
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        std::fs::write(fixture.project.canonical_root.join("guide.md"), "# Updated

Busy writer
").unwrap();
        let event = tokio::time::timeout(Duration::from_secs(10), reported.recv()).await.unwrap().unwrap();
        assert!(matches!(event, crate::watch::BatchEvent::LeaseRetry { attempt: 1 }));
        let event = tokio::time::timeout(Duration::from_secs(10), reported.recv()).await.unwrap().unwrap();
        assert!(matches!(event, crate::watch::BatchEvent::DroppedTransient { attempts: 0, error } if error.contains("deadline")));
        let warnings: String = fixture.shared.db().connection().query_row(
            "SELECT stats_json FROM sync_jobs WHERE project_id=?1 AND state='error' ORDER BY id DESC LIMIT 1",
            [id], |row| row.get(0),
        ).unwrap();
        assert!(warnings.contains("guide.md") && warnings.contains("deadline"), "{warnings}");
        drop(release);
        lease.join().unwrap().unwrap();
        Ok(())
    }).unwrap();
}

#[test]
fn actual_watch_lease_retry_refilters_revised_ignores_before_embedding() {
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
        {let slot=fixture.shared.project_slot(fixture.project.id);let mut state=slot.state.lock().unwrap();state.search_stages=state.stages.clone();}
        let (retry,retried)=tokio::sync::oneshot::channel();let retry=Mutex::new(Some(retry));
        fixture.shared.watchers.observe_batches(Arc::new(move |event| {
            if matches!(event,crate::watch::BatchEvent::LeaseRetry{..}) && let Some(retry)=retry.lock().unwrap().take() {retry.send(()).unwrap();}
        }));
        let (entered,held)=tokio::sync::oneshot::channel();let (release,proceed)=std::sync::mpsc::channel();
        let cache=fixture.shared.cache.clone();let id=fixture.project.id;
        let lease=std::thread::spawn(move || lifecycle::with_writer_lease(&cache,id,|| {entered.send(()).unwrap();proceed.recv_timeout(Duration::from_secs(10)).unwrap();Ok(())}));
        held.await.unwrap();let release=ReleaseOnDrop(Some(release));
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        let path=fixture.project.canonical_root.join("guide.md");
        let original=std::fs::read(&path).unwrap();
        std::fs::write(&path,"# Ignored\n\nignoredbatchwidget\n").unwrap();
        tokio::time::timeout(Duration::from_secs(10),retried).await.unwrap().unwrap();
        std::fs::write(fixture.project.canonical_root.join(".docsbase.toml"),"ignores=['guide.md']\n").unwrap();
        fixture.shared.open_project_config_with(&fixture.project,|_|Ok(HybridStages{embedder:Some(provider.clone()),..HybridStages::default()})).unwrap();
        let keep=fixture.project.canonical_root.join("keep.md");let body="# Keep\n\nkeepbatchwidget\n";std::fs::write(&keep,body).unwrap();
        drop(release);lease.join().unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(10),provider.entered.acquire()).await.unwrap().unwrap().forget();
        let first=provider.batches.lock().unwrap()[0].clone();
        provider.release.add_permits(32);
        let hash=blake3::hash(body.as_bytes()).to_hex().to_string();
        tokio::time::timeout(Duration::from_secs(10),async {loop {
            let current:Option<String>=fixture.shared.db().connection().query_row("SELECT content_hash FROM docs WHERE project_id=?1 AND rel_path='keep.md'",[id],|row|row.get(0)).ok();
            if current.as_deref()==Some(hash.as_str()) && lifecycle::with_writer_lease(&fixture.shared.cache,id,||Ok(())).is_ok(){break;}
            tokio::task::yield_now().await;
        }}).await.unwrap();
        assert!(first.iter().all(|text|!text.contains("ignoredbatchwidget")),"a raw pending path admitted under the old config must be refiltered before retry embedding");
        let current:String=fixture.shared.db().connection().query_row("SELECT content_hash FROM docs WHERE project_id=?1 AND rel_path='guide.md'",[id],|row|row.get(0)).unwrap();
        assert_eq!(current,blake3::hash(&original).to_hex().to_string());
        assert_eq!(provider.synchronous.load(Ordering::SeqCst),0);
        Ok(())
    }).unwrap();
}

#[test]
fn full_payload_budget_refuses_before_the_actual_pending_path_copy() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let fixture=search_phase_tests::Fixture::new(HybridStages::default());fixture.shared.activate_jobs(service).unwrap();
        let copies=Arc::new(AtomicUsize::new(0));let observed=copies.clone();
        fixture.shared.watchers.observe_pending_copy(Arc::new(move |_| {observed.fetch_add(1,Ordering::SeqCst);}));
        let (dropped,finished)=tokio::sync::oneshot::channel();let dropped=Mutex::new(Some(dropped));
        fixture.shared.watchers.observe_batches(Arc::new(move |event| {
            if matches!(event,crate::watch::BatchEvent::DroppedPermanent{..}) && let Some(dropped)=dropped.lock().unwrap().take() {dropped.send(()).unwrap();}
        }));
        let mut holds=Vec::new();let settings=fixture.shared.payload_budget.settings();let mut left=settings.max_retained_payload_bytes;
        while left>0 {let bytes=left.min(settings.max_request_payload_bytes);holds.push(fixture.shared.payload_budget.reserve(bytes).unwrap());left-=bytes;}
        let before:(String,i64,Option<String>,i64)=fixture.shared.db().connection().query_row("SELECT status,index_attempt,index_outcome_json,index_generation FROM projects WHERE id=?1",[fixture.project.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        let index=std::fs::read(lifecycle::index_dir(&fixture.shared.cache,fixture.project.id).join("meta.json")).unwrap();
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        std::fs::write(fixture.project.canonical_root.join("guide.md"),"# Budget\n\nnewpendingwidget\n").unwrap();
        tokio::time::timeout(Duration::from_secs(10),finished).await.unwrap().unwrap();
        let after:(String,i64,Option<String>,i64)=fixture.shared.db().connection().query_row("SELECT status,index_attempt,index_outcome_json,index_generation FROM projects WHERE id=?1",[fixture.project.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        let entered_count=copies.load(Ordering::SeqCst);
        drop(holds);
        assert_eq!(entered_count,0,"existing aggregate allowance must be reserved before entering the actual retained pending path/map copy");
        assert_eq!(before,after);
        assert_eq!(index,std::fs::read(lifecycle::index_dir(&fixture.shared.cache,fixture.project.id).join("meta.json")).unwrap());
        Ok(())
    }).unwrap();
}

struct NoWatchHttp(AtomicUsize);
impl crate::hybrid::remote::AsyncTransport for NoWatchHttp {
    fn post_json<'a>(
        &'a self,
        _: &'a crate::hybrid::remote::WireRequest,
        _: Duration,
    ) -> StageFuture<'a, crate::hybrid::wire::TransportResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(Error::internal(
                "unexpected watcher HTTP after codec cancellation",
            ))
        })
    }
}

#[test]
fn cancelled_actual_watch_codec_keeps_shared_charge_until_finish_or_unwind() {
    for unwind in [false, true] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = crate::daemon::settings::standalone().unwrap();
        let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
        let service = root.service();
        root.run(&runtime,async move {
            let fixture=search_phase_tests::Fixture::new(HybridStages::default());
            let wire=Arc::new(NoWatchHttp(AtomicUsize::new(0)));
            let mut execution=crate::hybrid::remote::RemoteExecution::with_owners(&fixture.shared.global_config.remote,wire.clone(),fixture.shared.workers.clone(),fixture.shared.activity.clone()).unwrap();
            let (entered,started)=tokio::sync::oneshot::channel();let entered=Mutex::new(Some(entered));
            let (release,proceed)=std::sync::mpsc::channel();let proceed=Mutex::new(proceed);
            execution.observe_cpu(Arc::new(move || {
                if let Some(entered)=entered.lock().unwrap().take() {entered.send(()).unwrap();proceed.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();assert!(!unwind,"controlled actual watcher codec unwind");}
            }));
            let client=Arc::new(crate::hybrid::openrouter::OpenRouterClient::with_execution(crate::hybrid::openrouter::BASE_URL,"fixture-secret",Arc::new(execution)).unwrap());
            let stages=Arc::new(HybridStages{embedder:Some(Arc::new(crate::hybrid::openrouter::OpenRouterEmbedder::with_client(crate::hybrid::EmbeddingCfg{model:"fixture".into(),dimensions:3,..crate::hybrid::EmbeddingCfg::default()},client))),..HybridStages::default()});
            {let slot=fixture.shared.project_slot(fixture.project.id);let mut state=slot.state.lock().unwrap();state.stages=Some(stages.clone());state.search_stages=Some(stages);}
            fixture.shared.activate_jobs(service).unwrap();
            let input=Arc::new(Mutex::new(None::<std::sync::Weak<crate::search::budget::PayloadPermit>>));let observed=input.clone();
            fixture.shared.watchers.observe_pending_copy(Arc::new(move |account|{*observed.lock().unwrap()=Some(Arc::downgrade(account));}));
            assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
            std::fs::write(fixture.project.canonical_root.join("guide.md"),"# Watch codec\n\nactually retained encoding inputs\n").unwrap();
            tokio::time::timeout(Duration::from_secs(10),started).await.unwrap().unwrap();
            let settings=fixture.shared.payload_budget.settings();let cap=settings.max_request_payload_bytes;
            let mut holds=Vec::new();let mut left=settings.max_retained_payload_bytes-cap;
            while left>0 {let bytes=left.min(cap);holds.push(fixture.shared.payload_budget.reserve(bytes).unwrap());left-=bytes;}
            fixture.shared.watchers.stop_project(fixture.project.id);
            tokio::time::timeout(Duration::from_secs(10),async {while !fixture.shared.watchers.consumer_finished(fixture.project.id) {tokio::task::yield_now().await;}}).await.unwrap();
            let retained=fixture.shared.payload_budget.reserve(cap).is_err();
            let guard_alive=input.lock().unwrap().as_ref().unwrap().upgrade().is_some();
            let writer_refused=lifecycle::with_writer_lease(&fixture.shared.cache,fixture.project.id,||Ok(())).is_err();
            release.send(()).unwrap();
            fixture.shared.activity.drain(Duration::from_millis(policy.settings().drain_poll_ms)).await;
            assert_eq!(registry::project_by_id(&fixture.shared.db(), fixture.project.id).unwrap().unwrap().status, crate::store::models::ProjectStatus::Indexed, "watch cancellation/unwind preserves the legacy incremental status after terminal retirement");
            assert!(retained && guard_alive,"consumer cancellation must retain the same charged pending/native inputs while actual codec work owns them");
            assert!(writer_refused,"native teardown waits actual codec physical retirement");
            assert!(input.lock().unwrap().as_ref().unwrap().upgrade().is_none(),"the last physical/native input owner releases the shared allowance");
            let refund=fixture.shared.payload_budget.reserve(cap).unwrap();drop(refund);drop(holds);
            assert_eq!(wire.0.load(Ordering::SeqCst),0);
            Ok(())
        }).unwrap();
    }
}

struct MultiProjectFixture {
    fixture: search_phase_tests::Fixture,
    wire: Arc<FactoryWire>,
    projects: Vec<crate::store::models::Project>,
    roots: [tempfile::TempDir; 2],
}
fn queue_fixture(
    policy: &crate::daemon::settings::DaemonSettings,
    service: crate::daemon::job_service::JobService,
) -> MultiProjectFixture {
    let mut fixture = search_phase_tests::Fixture::new(HybridStages::default());
    let wire = Arc::new(FactoryWire {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let owner = Arc::get_mut(&mut fixture.shared).unwrap();
    let mut remote = owner.global_config.remote.clone();
    remote.background_active = 1;
    remote.background_pending = 1;
    owner.workers = Arc::new(
        crate::daemon::worker_admission::WorkerAdmission::with_policies(policy, &remote).unwrap(),
    );
    owner.remote = Arc::new(
        crate::hybrid::remote::RemoteExecution::with_owners(
            &remote,
            wire.clone(),
            owner.workers.clone(),
            owner.activity.clone(),
        )
        .unwrap(),
    );
    let client = Arc::new(
        crate::hybrid::openrouter::OpenRouterClient::with_execution(
            crate::hybrid::openrouter::BASE_URL,
            "fixture-secret",
            owner.remote.clone(),
        )
        .unwrap(),
    );
    let stages = Arc::new(HybridStages {
        embedder: Some(Arc::new(
            crate::hybrid::openrouter::OpenRouterEmbedder::with_client(
                crate::hybrid::EmbeddingCfg {
                    model: "fixture".into(),
                    dimensions: 3,
                    ..crate::hybrid::EmbeddingCfg::default()
                },
                client,
            ),
        )),
        ..HybridStages::default()
    });
    let second = tempfile::tempdir().unwrap();
    let third = tempfile::tempdir().unwrap();
    let mut projects = vec![fixture.project.clone()];
    for path in [second.path(), third.path()] {
        std::fs::write(
            path.join("guide.md"),
            "# Other\n\ninitial lexical document\n",
        )
        .unwrap();
        let mut db = fixture.shared.db();
        let project = registry::ensure_project(&mut db, path).unwrap();
        tools::run_project_index(&mut db, &fixture.shared.cache, &project, &Config::default())
            .unwrap();
        projects.push(registry::project_by_id(&db, project.id).unwrap().unwrap());
    }
    for project in &projects {
        let slot = fixture.shared.project_slot(project.id);
        let mut state = slot.state.lock().unwrap();
        state.config = Some(Arc::new(Config::default()));
        state.stages = Some(stages.clone());
        state.search_stages = Some(stages.clone());
    }
    fixture.shared.activate_jobs(service).unwrap();
    MultiProjectFixture {
        fixture,
        wire,
        projects,
        roots: [second, third],
    }
}

#[test]
fn multiple_watch_projects_and_explicit_sync_share_bounded_jobs_and_provider_lanes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut policy = crate::daemon::settings::standalone()
        .unwrap()
        .into_settings();
    policy.background_active = 2;
    policy.background_pending = 1;
    policy.max_blocking_threads += 1;
    let mut root = crate::daemon::job_service::JobRoot::new(&policy);
    let service = root.service();
    root.run(&runtime, async move {
        let MultiProjectFixture {
            fixture,
            wire,
            projects,
            roots: _roots,
        } = queue_fixture(&policy, service.clone());
        for project in &projects[..2] {
            assert!(fixture.shared.ensure_watcher(project).unwrap());
            std::fs::write(
                project.canonical_root.join("guide.md"),
                format!("# Changed\n\nchanged project {}\n", project.id),
            )
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), wire.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        tokio::time::timeout(Duration::from_secs(10), async {
            while fixture.shared.remote.snapshot().pending != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let shared = fixture.shared.clone();
        let id = projects[2].id;
        let sync = tokio::spawn(async move {
            call_tool(&shared, None, "sync_start", json!({"project_id":id})).await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while service.pending_available() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let rows: i64 = fixture
            .shared
            .db()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sync_jobs WHERE project_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        let overflow = call_tool(
            &fixture.shared,
            None,
            "sync_start",
            json!({"project_id":id}),
        )
        .await;
        let operation = fixture.shared.watch_operation().unwrap();
        let interactive = crate::hybrid::remote::RemoteCall::new(operation.clone());
        let independent_remote = fixture.shared.remote.admit(&interactive).await.unwrap();
        let control = fixture.shared.workers.control().await.unwrap();
        let search = fixture.shared.workers.search(&operation).await.unwrap();
        let cpu = fixture.shared.workers.background(&operation).await.unwrap();
        let active = fixture.shared.remote.snapshot().active;
        drop((independent_remote, control, search, cpu));
        finish_queued_sync(&fixture, &wire, sync).await;
        fixture.shared.close_watchers();
        fixture
            .shared
            .activity
            .drain(Duration::from_millis(policy.drain_poll_ms))
            .await;
        assert_eq!(
            rows, 0,
            "queued Sync has no row/terminal authority before active admission"
        );
        assert!(
            overflow.is_err(),
            "the one existing logical pending slot is finite"
        );
        assert_eq!(
            active, 2,
            "interactive provider reserve is independent of saturated background reserve"
        );
        assert_eq!(wire.calls.load(Ordering::SeqCst), 3);
        assert_no_watch_sync_rows(&fixture, id);
        Ok(())
    })
    .unwrap();
}

async fn finish_queued_sync(
    fixture: &search_phase_tests::Fixture,
    wire: &FactoryWire,
    sync: tokio::task::JoinHandle<Result<Value>>,
) {
    wire.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), wire.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let accepted = tokio::time::timeout(Duration::from_secs(10), sync)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let job = accepted["job_id"].as_i64().unwrap();
    wire.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), wire.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    wire.release.add_permits(8);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = call_tool(&fixture.shared, None, "sync_status", json!({"job_id":job}))
                .await
                .unwrap();
            if state["state"] == "done" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn assert_no_watch_sync_rows(fixture: &search_phase_tests::Fixture, id: i64) {
    let watch_rows: i64 = fixture
        .shared
        .db()
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sync_jobs WHERE project_id<>?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(watch_rows, 0);
}

#[test]
fn actual_watch_reload_while_remote_is_held_retries_the_original_raw_batch() {
    const PHASE_WATCHDOG: Duration = Duration::from_mins(1);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let provider=document_gate();
        let fixture=search_phase_tests::Fixture::with_global(
            HybridStages{embedder:Some(provider.clone()),..HybridStages::default()},
            "config_version=2\n[watch]\ntransient_retries=1\n",
        );
        fixture.shared.activate_jobs(service).unwrap();
        {let slot=fixture.shared.project_slot(fixture.project.id);let mut state=slot.state.lock().unwrap();state.search_stages=state.stages.clone();}
        fixture.shared.watchers.use_manual_events();
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        let body="# Reload\n\nheld remote reload must retry this document\n";
        let path = fixture.project.canonical_root.join("guide.md");
        std::fs::write(&path, body).unwrap();
        fixture.shared.watchers.deliver_test_path(fixture.project.id, path);
        tokio::time::timeout(PHASE_WATCHDOG,provider.entered.acquire()).await.unwrap().unwrap().forget();
        std::fs::write(fixture.project.canonical_root.join(".docsbase.toml"),"config_version=2\nignores=['unrelated/**']\n[watch]\ntransient_retries=0\n").unwrap();
        fixture.shared.open_project_config_with(&fixture.project,|_|Ok(HybridStages{embedder:Some(provider.clone()),..HybridStages::default()})).unwrap();
        provider.release.add_permits(1);
        let retried=tokio::time::timeout(PHASE_WATCHDOG,provider.entered.acquire()).await.is_ok_and(|entered| {entered.unwrap().forget();true});
        provider.release.add_permits(16);
        if retried {
            let expected=blake3::hash(body.as_bytes()).to_hex().to_string();
            tokio::time::timeout(PHASE_WATCHDOG,async {loop {
                let current:Option<String>=fixture.shared.db().connection().query_row("SELECT content_hash FROM docs WHERE project_id=?1 AND rel_path='guide.md'",[fixture.project.id],|row|row.get(0)).ok();
                if current.as_deref()==Some(expected.as_str()) && lifecycle::with_writer_lease(&fixture.shared.cache,fixture.project.id,||Ok(())).is_ok(){break;}
                tokio::task::yield_now().await;
            }}).await.unwrap();
        }
        fixture.shared.close_watchers();fixture.shared.activity.drain(Duration::from_millis(policy.settings().drain_poll_ms)).await;
        assert!(retried,"a pending watch batch retains its admitted retry even after the new project revision disables retries");
        assert_eq!(provider.synchronous.load(Ordering::SeqCst),0);
        Ok(())
    }).unwrap();
}

struct OrderedWatchResult {
    free_slots: usize,
    admitted: Vec<String>,
    retained: Vec<String>,
    provider: Vec<String>,
    indexed: Vec<String>,
}

#[test]
fn actual_ordered_watch_batch_preserves_provider_order_and_last_document_slot() {
    let mut observed = Vec::new();
    for free_slots in [2, 1] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let policy = crate::daemon::settings::standalone().unwrap();
        let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
        let service = root.service();
        observed.push(
            root.run(&runtime, ordered_watch_case(service, free_slots))
                .unwrap(),
        );
    }
    for result in &observed {
        eprintln!(
            "order slots={} admitted={:?} retained={:?} provider={:?} indexed={:?}",
            result.free_slots, result.admitted, result.retained, result.provider, result.indexed
        );
    }
    for result in observed {
        assert_eq!(
            result.retained, result.admitted,
            "refilter must preserve the admitted Batch order rather than root traversal"
        );
        assert_eq!(
            result.provider,
            result.admitted[..result.free_slots],
            "actual retained chunks must reach the provider in admitted order"
        );
        let mut expected = result.admitted[..result.free_slots].to_vec();
        expected.sort();
        assert_eq!(
            result.indexed, expected,
            "the same first admitted document must win the existing last free slot"
        );
    }
}

async fn ordered_watch_case(
    service: crate::daemon::job_service::JobService,
    free_slots: usize,
) -> Result<OrderedWatchResult> {
    let provider = document_gate();
    provider.release.add_permits(16);
    let fixture = search_phase_tests::Fixture::new(HybridStages {
        embedder: Some(provider.clone()),
        ..HybridStages::default()
    });
    let config = Arc::new(Config {
        max_docs_per_project: 1 + free_slots,
        ..Config::default()
    });
    let stages = Arc::new(HybridStages {
        embedder: Some(provider.clone()),
        ..HybridStages::default()
    });
    {
        let slot = fixture.shared.project_slot(fixture.project.id);
        let mut state = slot.state.lock().unwrap();
        state.config = Some(config.clone());
        state.stages = Some(stages.clone());
        state.search_stages = Some(stages.clone());
    }
    fixture.shared.activate_jobs(service).unwrap();
    for name in ["arrival-a", "arrival-b"] {
        std::fs::write(
            fixture.project.canonical_root.join(format!("{name}.md")),
            format!("# Arrival\n\n{name}\n"),
        )
        .unwrap();
    }
    // Static fixture: explicitly reverse the actual walker, with no OS-event
    // arrival, delay or new traversal-sort assumption in the test or product.
    let mut paths: Vec<_> = crate::index::walk::walk(&fixture.project.canonical_root, &config)
        .unwrap()
        .map(std::result::Result::unwrap)
        .filter(|path| path.file_name().unwrap() != "guide.md")
        .collect();
    assert_eq!(paths.len(), 2);
    paths.reverse();
    let names = |paths: &[PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect()
    };
    let admitted = names(&paths);
    let operation = fixture.shared.watch_operation().unwrap();
    let charge = fixture.shared.activity.track();
    let permit = fixture.shared.workers.background(&operation).await.unwrap();
    let owner = fixture.shared.clone();
    let project = fixture.project.clone();
    let config_copy = config.clone();
    let call = operation.clone();
    let (pending, retained) = request::spawn(Some(permit), Some(charge.clone()), move || {
        let (_, mut receiver) = tokio::sync::mpsc::channel(1);
        let pending = crate::watch::merge_pending(
            &owner.payload_budget,
            None,
            Some(paths),
            &mut receiver,
            &call,
            &owner.watchers,
        )?;
        let selection = crate::watch::refilter_accounted(
            &project,
            &owner.cache,
            &pending.paths,
            &config_copy,
            None,
        )?;
        assert!(!selection.expansion_limited);
        Ok((pending, selection.paths))
    })
    .await
    .unwrap()
    .unwrap();
    let retained = names(&retained);
    let runtime = Arc::new(crate::watch::WatchRuntime::new(
        config.clone(),
        stages,
        fixture
            .shared
            .vector_source_for(fixture.project.id, &config),
    ));
    let stats = fixture
        .shared
        .run_watch_batch(fixture.project.clone(), runtime, pending, operation, charge)
        .await
        .unwrap();
    assert_eq!(stats.docs, free_slots);
    fixture
        .shared
        .activity
        .drain(Duration::from_millis(1))
        .await;
    Ok(ordered_observation(
        &fixture, &provider, free_slots, admitted, retained,
    ))
}

fn ordered_observation(
    fixture: &search_phase_tests::Fixture,
    provider: &DocumentGate,
    free_slots: usize,
    admitted: Vec<String>,
    retained: Vec<String>,
) -> OrderedWatchResult {
    let batches = provider.batches.lock().unwrap();
    let provider_order = batches
        .iter()
        .flatten()
        .map(|text| {
            if text.contains("arrival-a") {
                "arrival-a".to_owned()
            } else if text.contains("arrival-b") {
                "arrival-b".to_owned()
            } else {
                panic!("unexpected retained provider text")
            }
        })
        .collect();
    let mut indexed: Vec<_> =
        crate::store::repo::doc_states(fixture.shared.db().connection(), fixture.project.id)
            .unwrap()
            .into_iter()
            .filter(|doc| doc.rel_path != "guide.md")
            .map(|doc| doc.rel_path.trim_end_matches(".md").to_owned())
            .collect();
    indexed.sort();
    assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
    OrderedWatchResult {
        free_slots,
        admitted,
        retained,
        provider: provider_order,
        indexed,
    }
}
