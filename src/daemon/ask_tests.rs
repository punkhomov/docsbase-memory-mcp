use super::super::{call_tool, search_phase_tests::Fixture};
use super::*;
use crate::hybrid::HybridStages;

fn reads(fixture: &Fixture) -> Reads {
    let operation = SearchOperation::with_timeout(
        std::time::Instant::now(),
        AskLimits::TIMEOUT,
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let config = crate::config::Config {
        max_file_size: AskLimits::TOOL_BYTES as u64,
        ..Default::default()
    };
    Reads {
        shared: fixture.shared.clone(),
        session: Some(fixture.session),
        project: fixture.project.clone(),
        config,
        operation,
        activity: fixture.shared.activity.track(),
        reservation: Arc::new(
            fixture
                .shared
                .payload_budget
                .reserve(AskLimits::RETAINED_BYTES)
                .unwrap(),
        ),
    }
}

#[tokio::test]
async fn actual_document_tools_use_the_bound_project_and_reject_escape() {
    let fixture = Fixture::new(HybridStages::default());
    let reads = reads(&fixture);
    let hits = reads
        .call("search_docs", json!({"query":"widgets","limit":1000}))
        .await
        .unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["path"], "guide.md");
    let doc = reads
        .call("get_doc", json!({"path":"guide.md"}))
        .await
        .unwrap();
    assert!(
        doc["content"]
            .as_str()
            .unwrap()
            .contains("old widgets text")
    );
    assert!(
        reads
            .call("get_doc", json!({"path":"../outside.md"}))
            .await
            .is_err()
    );
    let chunks = reads
        .call("read_neighbors", json!({"chunk_id":hits[0]["chunk_id"]}))
        .await
        .unwrap();
    assert_eq!(chunks[0]["path"], "guide.md");
    assert!(
        reads
            .call(
                "read_neighbors",
                json!({"chunk_id":hits[0]["chunk_id"],"after":100})
            )
            .await
            .is_err()
    );
    std::fs::write(
        fixture.project.canonical_root.join("large.md"),
        "x".repeat(AskLimits::TOOL_BYTES + 1),
    )
    .unwrap();
    assert!(
        reads
            .call("get_doc", json!({"path":"large.md"}))
            .await
            .is_err()
    );
    fixture
        .shared
        .db()
        .connection()
        .execute(
            "UPDATE chunks SET text=?1",
            ["x".repeat(AskLimits::TOOL_BYTES + 1)],
        )
        .unwrap();
    assert!(
        reads
            .call("read_neighbors", json!({"chunk_id":hits[0]["chunk_id"]}))
            .await
            .is_err(),
        "atomic oversized chunks are rejected before owned row copies"
    );
}

#[tokio::test]
async fn unsupported_generation_fails_before_any_provider_request() {
    let fixture = Fixture::new(HybridStages::default());
    let error = call_tool(
        &fixture.shared,
        Some(fixture.session),
        "ask_docs",
        json!({"question":"widgets","model":"fastembed/example"}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("does not support generation"));
    assert_eq!(fixture.shared.remote.snapshot().attempts, 0);
    assert_eq!(
        fixture.shared.payload_budget.snapshot()["retained_payload_bytes"],
        0
    );
}

#[tokio::test]
#[ignore = "live: explicit OPENROUTER_API_KEY and DOCSBASE_ASK_MODEL; at most 4 single-shot turns"]
async fn live_ask_docs_smoke() {
    let model = std::env::var("DOCSBASE_ASK_MODEL").expect("explicit model required");
    assert!(
        model.ends_with(":free"),
        "smoke is restricted to a free model"
    );
    let fixture = Fixture::new(HybridStages::default());
    let result = call_tool(&fixture.shared,Some(fixture.session),"ask_docs",json!({
            "question":"Search for widgets, read guide.md, and report what the guide says about widgets. Cite the document.",
            "model":format!("openrouter/{model}"),"max_turns":4
        })).await.expect("live ask_docs");
    println!("ASK_LIVE_RESULT {result}");
    assert_eq!(result["stop_reason"], "answered");
    assert_eq!(result["sources"][0]["path"], "guide.md");
}

struct Model(std::sync::atomic::AtomicUsize);
impl crate::hybrid::generation::Generator for Model {
    fn complete<'a>(
        &'a self,
        input: rig_core::completion::CompletionRequest,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, rig_core::completion::CompletionResponse> {
        Box::pin(async move {
            call.check()?;
            let step = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (tool, args) = match step {
                0 => ("search_docs", json!({"query":"widgets"})),
                1 => ("get_doc", json!({"path":"guide.md"})),
                2 => {
                    let history = serde_json::to_string(&input.chat_history).unwrap();
                    assert!(history.contains("old widgets text"));
                    assert!(history.contains("S1"));
                    (
                        "finish_answer",
                        json!({"answer":"The guide says old widgets text. [S1]","citations":["S1"],"insufficient_evidence":false}),
                    )
                }
                _ => panic!("extra generation after completion"),
            };
            let body = json!({"id":format!("turn-{step}"),"choices":[{"index":0,"finish_reason":"tool_calls","message":{
                "role":"assistant","content":null,"tool_calls":[{"id":format!("call-{step}"),"type":"function","function":{"name":tool,"arguments":args.to_string()}}]
            }}]});
            let codec = rig_core::providers::openai::wire::OpenAIConfig::with_key(
                &rig_core::providers::openai::wire::OPENROUTER,
                "fixture",
            )
            .chat("fixture/model");
            Ok(rig_core::wire::decode_reply(
                &codec,
                &input,
                rig_core::wire::Mode::Unary,
                [rig_core::wire::WireFrame::Bytes(
                    body.to_string().into_bytes(),
                )],
            )
            .unwrap())
        })
    }
}

#[tokio::test]
async fn daemon_composition_runs_search_read_answer_and_releases_its_payload() {
    let fixture = Fixture::new(HybridStages::default());
    let model = Arc::new(Model(std::sync::atomic::AtomicUsize::new(0)));
    let captured = model.clone();
    let operation = super::super::search_operation(&fixture.shared, "ask_docs")
        .unwrap()
        .unwrap();
    let result = run_with_factory(
        &fixture.shared,
        Some(fixture.session),
        json!({"question":"widgets","model":"openrouter/fixture/model"}),
        operation,
        None,
        move |_, _, _| Ok(captured),
    )
    .await
    .unwrap();
    assert_eq!(result["stop_reason"], "answered");
    assert_eq!(result["sources"][0]["path"], "guide.md");
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(
        fixture.shared.payload_budget.snapshot()["retained_payload_bytes"],
        0
    );
}
