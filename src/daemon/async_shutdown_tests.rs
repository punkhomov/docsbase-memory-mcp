//! Actual producer/native retirement and root cancellation boundaries.
use super::*;

#[test]
fn stopped_watcher_keeps_physical_charge_and_defers_restart_until_native_drop_finishes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime,async move {
        let fixture=search_phase_tests::Fixture::new(HybridStages::default());fixture.shared.activate_jobs(service).unwrap();
        let (entered,started)=tokio::sync::oneshot::channel();let entered=Mutex::new(Some(entered));
        let (release,proceed)=std::sync::mpsc::channel();let proceed=Mutex::new(proceed);
        fixture.shared.watchers.observe_producer_retirement(Arc::new(move || {
            if let Some(entered)=entered.lock().unwrap().take() {
                entered.send(()).unwrap();proceed.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }));
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        assert_eq!(fixture.shared.activity.counter.load(Ordering::SeqCst),0,"idle watcher retirement owner is dormant");
        fixture.shared.watchers.stop_project(fixture.project.id);
        tokio::time::timeout(Duration::from_secs(10),started).await.unwrap().unwrap();
        let retained=fixture.shared.activity.counter.load(Ordering::SeqCst);
        let deferred=!fixture.shared.ensure_watcher(&fixture.project).unwrap();
        let progress=fixture.shared.workers.control().await;let independent=progress.is_ok();drop(progress);
        // Native producer gate is always released before assertions/root teardown.
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10),async { while fixture.shared.watchers.watching(fixture.project.id) { tokio::task::yield_now().await; } }).await.unwrap();
        fixture.shared.activity.drain(Duration::from_millis(policy.settings().drain_poll_ms)).await;
        assert_eq!(retained,1,"original existing Activity remains through actual native watcher Drop/join");
        assert!(deferred,"same-project replacement must not accumulate another native producer while retirement is held");
        assert!(independent,"native Drop is on existing producer thread, leaving Tokio/control free");
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap(),"physical completion permits restart");
        Ok(())
    }).unwrap();
}

#[test]
fn root_cancels_actual_watcher_remote_and_joins_producer_before_runtime_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    let retained = Mutex::new(None);
    let result = root.run(&runtime, async {
        let provider = document_gate();
        let fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(provider.clone()),
            ..HybridStages::default()
        });
        fixture.shared.activate_jobs(service).unwrap();
        {
            let slot = fixture.shared.project_slot(fixture.project.id);
            let mut state = slot.state.lock().unwrap();
            state.search_stages = state.stages.clone();
        }
        assert!(fixture.shared.ensure_watcher(&fixture.project).unwrap());
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            "# Cancel\n\nactualheldwatcher\n",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), provider.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        assert_eq!(provider.synchronous.load(Ordering::SeqCst), 0);
        *retained.lock().unwrap() = Some((fixture, provider));
        Err::<(), _>(Error::internal("controlled outer watcher-driver failure"))
    });
    assert!(result.is_err());
    let (fixture, provider) = retained.into_inner().unwrap().unwrap();
    assert_eq!(
        provider.release.available_permits(),
        0,
        "root cancellation drops the held remote future without release"
    );
    assert_eq!(fixture.shared.activity.counter.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.shared.watchers.count(), 0);
    assert!(
        lifecycle::with_writer_lease(&fixture.shared.cache, fixture.project.id, || Ok(())).is_ok()
    );
    let rows: i64 = fixture
        .shared
        .db()
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sync_jobs WHERE project_id=?1",
            [fixture.project.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn invalidated_ann_worker_unwind_retires_the_original_activity_after_physical_data() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let (retired, finished) = tokio::sync::oneshot::channel();
        let retired = Mutex::new(Some(retired));
        service.observe_retirement(Arc::new(move |event| {
            if event == "terminal_finished"
                && let Some(retired) = retired.lock().unwrap().take()
            {
                let _ = retired.send(());
            }
        }));
        let provider = document_gate();
        provider.release.add_permits(100);
        let mut fixture = search_phase_tests::Fixture::new(HybridStages {
            embedder: Some(provider),
            ranking: Ranking::Rrf,
            reranker: None,
        });
        let shared = Arc::get_mut(&mut fixture.shared).unwrap();
        let mut dense = crate::hybrid::dense_settings::standalone()
            .unwrap()
            .into_settings();
        dense.min_vectors = 1;
        shared.dense = crate::hybrid::dense::DenseCache::with_settings(
            crate::hybrid::dense::AnnPolicy::BuildOrLoad,
            &shared.cache,
            &dense,
        )
        .unwrap();
        let gate = shared.dense.pause_job_builds_for_test();
        let _release = OpenBuildOnDrop(gate.clone());
        fixture.shared.activate_jobs(service.clone()).unwrap();
        let context = lexical_context(&fixture, crate::index::async_job::JobInput::Sync);
        let operation = fixture.shared.watch_operation().unwrap();
        let activity = fixture.shared.activity.track();
        let original = Arc::downgrade(&activity);
        let result = crate::index::async_job::run(
            &service,
            &fixture.shared.workers,
            activity,
            operation.clone(),
            &fixture.shared.payload_budget,
            context,
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "done");
        tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .unwrap()
            .unwrap();
        fixture.shared.dense.invalidate_memory(fixture.project.id);
        let retained = original.upgrade().is_some();
        let dense = fixture.shared.dense.snapshot();
        let permit = fixture.shared.workers.background(&operation).await.unwrap();
        let joined = request::spawn(
            Some(permit),
            Some(fixture.shared.activity.track()),
            move || {
                gate.open_with_unwind();
                dense.join_job_builds_for_test()
            },
        )
        .await
        .unwrap();
        fixture
            .shared
            .activity
            .drain(Duration::from_millis(policy.settings().drain_poll_ms))
            .await;
        assert!(
            retained,
            "invalidation/caller completion cannot free the ANN worker's original physical Activity"
        );
        assert!(
            joined.is_err(),
            "the existing worker gate exercised actual unwind"
        );
        assert!(
            original.upgrade().is_none(),
            "actual unwind drops rows, reservation and original Activity"
        );
        Ok(())
    })
    .unwrap();
}
