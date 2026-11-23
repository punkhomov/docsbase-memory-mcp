use super::*;
use crate::hybrid::remote::{AsyncTransport, RemoteExecution, WireRequest};
use crate::hybrid::wire::TransportResponse;
use crate::hybrid::work::Cancellation;
use crate::search::operation::SearchOperation;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Script {
    replies: Mutex<VecDeque<(u16, Value)>>,
    sent: Mutex<Vec<Value>>,
}
impl AsyncTransport for Script {
    fn post_json<'a>(
        &'a self,
        request: &'a WireRequest,
        _: Duration,
    ) -> StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            assert_eq!(request.url, "https://openrouter.ai/api/v1/chat/completions");
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::from_str(&request.body).unwrap());
            let (status, body) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra model request");
            Ok(TransportResponse {
                status,
                body: body.to_string(),
                retry_after: None,
            })
        })
    }
}
#[derive(Default)]
struct Reads(Mutex<Vec<String>>);
impl DocumentTools for Reads {
    fn call<'a>(&'a self, name: &'a str, _: Value) -> StageFuture<'a, Value> {
        Box::pin(async move {
            self.0.lock().unwrap().push(name.to_owned());
            Ok(match name {
                "search_docs" => json!([{"path":"guide.md", "chunk_id":1}]),
                "get_doc" => json!({"path":"guide.md","content":"The widget switch is blue."}),
                "read_neighbors" => {
                    json!([{"path":"guide.md","text":"The widget switch is blue.","lines":[2,2]}])
                }
                _ => panic!("unexpected tool reached backend"),
            })
        })
    }
}
fn turn(calls: &[(&str, Value)]) -> Value {
    json!({"id":"completion-1","model":"fixture/model","choices":[{"index":0,"finish_reason":"tool_calls",
        "message":{"role":"assistant","content":null,"tool_calls":calls.iter().enumerate().map(|(n,(name,args))|json!({
            "id":format!("call_{n}"),"type":"function","function":{"name":name,"arguments":args.to_string()}
        })).collect::<Vec<_>>()}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})
}
fn finish(ids: &[&str]) -> Value {
    json!({"answer":"The widget switch is blue. [S1]","citations":ids,"insufficient_evidence":false})
}
fn setup(replies: Vec<(u16, Value)>) -> (Arc<Script>, Arc<dyn Generator>, RemoteCall) {
    let script = Arc::new(Script {
        replies: Mutex::new(replies.into()),
        sent: Mutex::new(vec![]),
    });
    let remote = crate::hybrid::remote_settings::standalone().unwrap();
    let daemon = crate::daemon::settings::standalone().unwrap();
    let execution = Arc::new(
        RemoteExecution::with_transport(remote.settings(), daemon.settings(), script.clone())
            .unwrap(),
    );
    let client = Arc::new(
        crate::hybrid::openrouter::OpenRouterClient::with_execution(
            crate::hybrid::openrouter::BASE_URL,
            "fixture-not-a-real-key",
            execution,
        )
        .unwrap(),
    );
    let generator = client.generation("fixture/model".into());
    let operation =
        SearchOperation::with_timeout(Instant::now(), AskLimits::TIMEOUT, Cancellation::new())
            .unwrap();
    (script, generator, RemoteCall::new(operation))
}
fn request(turns: usize) -> AskRequest {
    AskRequest::parse(json!({"question":"What color is the widget switch?","model":"openrouter/fixture/model","max_turns":turns})).unwrap()
}

#[tokio::test]
async fn real_rig_codec_searches_reads_and_returns_verified_sources() {
    let first = turn(&[("search_docs", json!({"query":"widget switch"}))]);
    let second = turn(&[
        ("get_doc", json!({"path":"guide.md"})),
        ("read_neighbors", json!({"chunk_id":1})),
    ]);
    let (wire, generator, call) = setup(vec![
        (200, first),
        (200, second),
        (200, turn(&[("finish_answer", finish(&["S1"]))])),
    ]);
    let reads = Reads::default();
    let result = run(request(6), generator.as_ref(), &reads, &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "answered");
    assert_eq!(result["sources"][0]["path"], "guide.md");
    assert_eq!(result["sources"][0]["lines"], json!([1, 1]));
    assert_eq!(
        result["sources"][0]["content_hash"],
        blake3::hash(b"The widget switch is blue.")
            .to_hex()
            .as_str()
    );
    assert_eq!(result["usage"][0]["input_tokens"], 100);
    assert_eq!(
        *reads.0.lock().unwrap(),
        vec!["search_docs", "get_doc", "read_neighbors"]
    );
    let sent = wire.sent.lock().unwrap();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0]["max_tokens"], 4096);
    assert!(
        sent[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_1")
    );
    assert_eq!(sent[0]["tools"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn unknown_and_recursive_tools_never_reach_backend_and_exhaust_turns() {
    let (wire, generator, call) = setup(vec![(
        200,
        turn(&[("ask_docs", json!({})), ("write_file", json!({}))]),
    )]);
    let reads = Reads::default();
    let result = run(request(1), generator.as_ref(), &reads, &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "turn_limit");
    assert!(result["answer"].is_null());
    assert!(reads.0.lock().unwrap().is_empty());
    assert_eq!(wire.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invented_citations_are_rejected_then_can_be_corrected() {
    let (wire, generator, call) = setup(vec![
        (
            200,
            turn(&[
                ("search_docs", json!({"query":"widget"})),
                ("get_doc", json!({"path":"guide.md"})),
            ]),
        ),
        (200, turn(&[("finish_answer", finish(&["S99"]))])),
        (200, turn(&[("finish_answer", finish(&["S1"]))])),
    ]);
    let result = run(request(3), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "answered");
    let sent = wire.sent.lock().unwrap();
    assert!(
        sent[2]["messages"]
            .to_string()
            .contains("citations must be unique source IDs")
    );
    assert!(!result.to_string().contains("S99"));
}

#[tokio::test]
async fn missing_evidence_is_a_distinct_successful_stop() {
    let (.., generator, call) = setup(vec![
        (200, turn(&[("search_docs", json!({"query":"missing"}))])),
        (
            200,
            turn(&[(
                "finish_answer",
                json!({"answer":"No supporting documentation found.","citations":[],"insufficient_evidence":true}),
            )]),
        ),
    ]);
    let result = run(request(2), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "insufficient_evidence");
    assert_eq!(result["sources"], json!([]));
}

#[tokio::test]
async fn provider_429_is_not_retried_and_response_body_is_private() {
    let (wire, generator, call) = setup(vec![(
        429,
        json!({"error":{"message":"sensitive provider echo"}}),
    )]);
    let error = run(request(6), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("429"));
    assert!(!format!("{error:?}").contains("sensitive provider echo"));
    assert_eq!(wire.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_before_start_sends_nothing() {
    let (wire, generator, call) = setup(vec![]);
    call.operation().cancellation().cancel();
    assert!(
        run(request(6), generator.as_ref(), &Reads::default(), &call)
            .await
            .is_err()
    );
    assert!(wire.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tool_call_limit_is_global_even_with_many_calls_in_one_turn() {
    let calls: Vec<_> = (0..20)
        .map(|_| ("search_docs", json!({"query":"widget"})))
        .collect();
    let (_, generator, call) = setup(vec![(200, turn(&calls))]);
    let reads = Reads::default();
    let result = run(request(6), generator.as_ref(), &reads, &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "tool_limit");
    assert_eq!(reads.0.lock().unwrap().len(), 16);
}

#[test]
fn explicit_model_and_small_validated_request_are_required() {
    for args in [
        json!({"question":"x"}),
        json!({"question":"x","model":"unknown/x"}),
        json!({"question":"x","model":"openrouter/x","max_turns":7}),
        json!({"question":"x","model":"openrouter/x","api_key":"forbidden"}),
    ] {
        assert!(AskRequest::parse(args).is_err());
    }
}

#[tokio::test]
async fn oversized_provider_reply_is_rejected_without_retry() {
    let (wire, generator, call) = setup(vec![(
        200,
        json!({"content":"x".repeat(usize::try_from(AskLimits::RESPONSE_BYTES).unwrap())}),
    )]);
    assert!(
        run(request(6), generator.as_ref(), &Reads::default(), &call)
            .await
            .is_err()
    );
    assert_eq!(wire.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn reasoning_is_preserved_in_continuation_but_never_published() {
    let mut first = turn(&[("search_docs", json!({"query":"widget"}))]);
    first["choices"][0]["message"]["reasoning"] = json!("private fixture reasoning");
    first["choices"][0]["message"]["reasoning_details"] = json!([{"type":"reasoning.text","text":"private fixture reasoning","signature":"fixture-signature","format":"unknown","index":0}]);
    let (wire, generator, call) = setup(vec![
        (200, first),
        (200, turn(&[("get_doc", json!({"path":"guide.md"}))])),
        (200, turn(&[("finish_answer", finish(&["S1"]))])),
    ]);
    let result = run(request(3), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap();
    assert!(!result.to_string().contains("private fixture reasoning"));
    let sent = wire.sent.lock().unwrap();
    assert!(
        sent[1]["messages"]
            .to_string()
            .contains("fixture-signature")
    );
}

#[tokio::test]
async fn context_limit_stops_before_the_next_request() {
    let mut first = turn(&[("search_docs", json!({"query":"widget"}))]);
    first["choices"][0]["message"]["reasoning"] = json!("r".repeat(AskLimits::CONTEXT + 1));
    let (wire, generator, call) = setup(vec![(200, first)]);
    let result = run(request(6), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "context_limit");
    assert_eq!(wire.sent.lock().unwrap().len(), 1);
}

#[test]
fn encoded_context_accounts_for_json_escaping_without_an_allocation() {
    assert!(fits_json(&json!("a"), 3));
    assert!(!fits_json(&json!("\n"), 3));
    assert!(!fits_json(&json!({"text":"x".repeat(10000)}), 100));
}

#[tokio::test]
async fn library_call_cannot_bypass_turn_validation_by_direct_deserialization() {
    let invalid: AskRequest = serde_json::from_value(
        json!({"question":"x","model":"openrouter/fixture","max_turns":1000}),
    )
    .unwrap();
    let (wire, generator, call) = setup(vec![]);
    assert!(
        run(invalid, generator.as_ref(), &Reads::default(), &call)
            .await
            .is_err()
    );
    assert!(wire.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn answer_cannot_cite_a_read_requested_in_the_same_model_turn() {
    let (_, generator, call) = setup(vec![(
        200,
        turn(&[
            ("search_docs", json!({"query":"widget"})),
            ("get_doc", json!({"path":"guide.md"})),
            ("finish_answer", finish(&["S1"])),
        ]),
    )]);
    let result = run(request(1), generator.as_ref(), &Reads::default(), &call)
        .await
        .unwrap();
    assert_eq!(result["stop_reason"], "turn_limit");
    assert!(result["answer"].is_null());
}

#[test]
fn source_envelope_is_included_in_the_read_limit() {
    let mut state = State::default();
    let value = json!({"path":"x.md","content":"x".repeat(AskLimits::TOOL_BYTES-32)});
    assert!(fits_json(&value, AskLimits::TOOL_BYTES));
    let output = state.evidence("get_doc", value);
    assert!(output.get("error").is_some());
    assert!(
        state.sources.is_empty(),
        "undelivered text must not authorize a citation"
    );
}
