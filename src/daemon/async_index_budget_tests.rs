use super::*;
use std::future::{Future, poll_fn};
use std::task::Poll;

#[test]
fn original_background_deadline_expires_while_initial_control_is_still_held() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let mut fixture = search_phase_tests::Fixture::new(HybridStages::default());
        let shared=Arc::get_mut(&mut fixture.shared).unwrap();
        shared.global_config.remote.background_timeout_ms = 1;
        let mut daemon=shared.global_config.daemon.clone();daemon.control_active=1;daemon.control_pending=1;
        shared.workers=Arc::new(crate::daemon::worker_admission::WorkerAdmission::with_policies(&daemon,&shared.global_config.remote).unwrap());
        let held = fixture.shared.workers.control().await.unwrap();
        let new_root = tempfile::tempdir().unwrap();
        let before: i64 = fixture.shared.db().connection().query_row("SELECT COUNT(*) FROM projects",[],|row|row.get(0)).unwrap();
        let mut work = Box::pin(index_async(&fixture.shared,&service,None,json!({"path":new_root.path()}),None,false));
        assert!(poll_fn(|cx|Poll::Ready(work.as_mut().poll(cx).is_pending())).await);
        // This exact budget boundary is later than the creation inside first poll.
        tokio::time::sleep_until(tokio::time::Instant::now()+Duration::from_millis(1)).await;
        let completion = poll_fn(|cx|Poll::Ready(work.as_mut().poll(cx))).await;
        let completed_before_release = completion.is_ready();
        drop(work);
        let after: i64 = fixture.shared.db().connection().query_row("SELECT COUNT(*) FROM projects",[],|row|row.get(0)).unwrap();
        drop(held);
        assert!(completed_before_release,"original background deadline must complete initial admission before control permit release");
        assert_eq!(before,after,"expired admission starts no registration/context effects");
        if let Poll::Ready(result) = completion {
            assert!(matches!(result,Err(Error::Admission { .. })));
        }
        Ok(())
    }).unwrap();
}

struct PeakCodec {
    inner: crate::hybrid::openrouter::OpenRouterEmbedder,
    charged: Arc<std::sync::atomic::AtomicU64>,
}
impl Embedder for PeakCodec {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Embedder::embed(&self.inner, texts)
    }
    fn async_remote(&self) -> Option<&dyn AsyncEmbedder> {
        Some(self)
    }
}
impl AsyncEmbedder for PeakCodec {
    fn embed<'a>(
        &'a self,
        texts: Arc<[String]>,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<Vec<f32>>> {
        self.charged.store(
            call.payload_reservation().unwrap().bytes(),
            Ordering::SeqCst,
        );
        self.inner.async_remote().unwrap().embed(texts, call)
    }
    fn embed_query<'a>(
        &'a self,
        text: Arc<str>,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, Vec<f32>> {
        self.inner.async_remote().unwrap().embed_query(text, call)
    }
}
struct EscapedWire {
    charged: Arc<std::sync::atomic::AtomicU64>,
    minimum: std::sync::atomic::AtomicU64,
    enough: std::sync::atomic::AtomicBool,
    calls: AtomicUsize,
}
impl crate::hybrid::remote::AsyncTransport for EscapedWire {
    fn post_json<'a>(
        &'a self,
        request: &'a crate::hybrid::remote::WireRequest,
        _: Duration,
    ) -> StageFuture<'a, crate::hybrid::wire::TransportResponse> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_str(&request.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            assert_eq!(inputs.len(), 1);
            assert_eq!(body["input_type"], "search_document");
            let bytes = inputs[0].as_str().unwrap().len() as u64;
            assert!(
                bytes > 128 * 1024,
                "atomic new fenced chunk must exceed tiny batch byte limit"
            );
            assert!(
                request.body.len() as u64 > 5 * bytes,
                "fixture exercises JSON escaping"
            );
            // These live allocations coexist at actual send: captured plan,
            // batch String, retained Rig input String, and encoded wire body.
            let minimum = request.body.capacity() as u64 + 3 * bytes;
            self.minimum.store(minimum, Ordering::SeqCst);
            self.enough.store(
                self.charged.load(Ordering::SeqCst) >= minimum,
                Ordering::SeqCst,
            );
            Ok(crate::hybrid::wire::TransportResponse {
                status: 200,
                body: json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}).to_string(),
                retry_after: None,
            })
        })
    }
}

#[test]
fn oversized_new_escaped_atomic_chunk_is_charged_through_real_rig_codec() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let policy = crate::daemon::settings::standalone().unwrap();
    let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
    let service = root.service();
    root.run(&runtime, async move {
        let fixture = search_phase_tests::Fixture::new(HybridStages::default());
        fixture.shared.activate_jobs(service).unwrap();
        let charged = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let wire = Arc::new(EscapedWire {
            charged: charged.clone(),
            minimum: std::sync::atomic::AtomicU64::new(0),
            enough: std::sync::atomic::AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let execution = Arc::new(
            crate::hybrid::remote::RemoteExecution::with_owners(
                &fixture.shared.global_config.remote,
                wire.clone(),
                fixture.shared.workers.clone(),
                fixture.shared.activity.clone(),
            )
            .unwrap(),
        );
        let client = Arc::new(
            crate::hybrid::openrouter::OpenRouterClient::with_execution(
                crate::hybrid::openrouter::BASE_URL,
                "fixture-secret",
                execution,
            )
            .unwrap(),
        );
        let embedder = Arc::new(PeakCodec {
            inner: crate::hybrid::openrouter::OpenRouterEmbedder::with_client(
                crate::hybrid::EmbeddingCfg {
                    model: "fixture".into(),
                    dimensions: 3,
                    ..crate::hybrid::EmbeddingCfg::default()
                },
                client,
            ),
            charged: charged.clone(),
        });
        let slot = fixture.shared.project_slot(fixture.project.id);
        {
            let mut state = slot.state.lock().unwrap();
            let mut config = (**state.config.as_ref().unwrap()).clone();
            config.remote.embed_batch_texts = 1;
            config.remote.embed_batch_bytes = 16;
            state.config = Some(Arc::new(config));
            state.stages = Some(Arc::new(HybridStages {
                embedder: Some(embedder),
                ..HybridStages::default()
            }));
            state.search_stages = None;
        }
        std::fs::write(
            fixture.project.canonical_root.join("guide.md"),
            format!("```text\n{}\n```\n", "\0".repeat(160 * 1024)),
        )
        .unwrap();
        let result = call_tool(
            &fixture.shared,
            Some(fixture.session),
            "index_project",
            json!({}),
        )
        .await;
        assert_eq!(result.unwrap()["state"], "done");
        assert_eq!(wire.calls.load(Ordering::SeqCst), 1);
        assert!(
            wire.enough.load(Ordering::SeqCst),
            "actual admitted job charge{} is below simultaneously live codec lower bound{}",
            charged.load(Ordering::SeqCst),
            wire.minimum.load(Ordering::SeqCst)
        );
        Ok(())
    })
    .unwrap();
}
