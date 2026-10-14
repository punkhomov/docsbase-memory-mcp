#![cfg(feature = "hybrid-remote")]
//! Wire tests over the recorded `OpenRouter` fixtures (T1 spike, 2026-10-14):
//! request shape, response parsing, L2, rerank mapping, retry, error
//! mapping, key redaction and the blocking-context guard (FR-1/2/5/7/8/9,
//! SC-5; I1, I2, I3). No network: everything runs through a mock transport.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::Reranker;
use docsbase_memory::hybrid::blocking;
use docsbase_memory::hybrid::config::{EmbeddingCfg, RerankerCfg};
use docsbase_memory::hybrid::openrouter::{
    BASE_URL, OpenRouterClient, OpenRouterEmbedder, OpenRouterReranker,
};
use docsbase_memory::hybrid::wire::{Transport, TransportResponse};
use docsbase_memory::vector_cache::Embedder;
use serde_json::Value;

const EMBED_QWEN: &str = include_str!("fixtures/openrouter/embeddings/qwen3-8b-mixed-4lang.json");
const EMBED_PPLX: &str = include_str!("fixtures/openrouter/embeddings/pplx-0.6b-mixed-4lang.json");
const RERANK_V35: &str = include_str!("fixtures/openrouter/rerank/cohere-v3.5-topn2.json");
const RERANK_CLAMP: &str = include_str!("fixtures/openrouter/rerank/topn10-clamp.json");
const EMBED_404: &str =
    include_str!("fixtures/openrouter/embeddings/errors/embed-404-model-missing.json");
const EMBED_400: &str =
    include_str!("fixtures/openrouter/embeddings/errors/embed-400-empty-input.json");

struct Fixture {
    request: Value,
    status: u16,
    response: Value,
}

fn fixture(raw: &str) -> Fixture {
    let value: Value = serde_json::from_str(raw).expect("fixture json");
    Fixture {
        request: value["request"].clone(),
        status: u16::try_from(value["status"].as_u64().expect("status")).expect("status u16"),
        response: value["response"].clone(),
    }
}

fn texts(value: &Value) -> Vec<String> {
    strings(value, "input")
}

fn documents(value: &Value) -> Vec<String> {
    strings(value, "documents")
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("fixture request strings")
        .iter()
        .map(|item| item.as_str().expect("string input").to_owned())
        .collect()
}

/// Mock transport: a FIFO of recorded responses, plus a call log.
#[derive(Clone, Default)]
struct MockTransport {
    responses: Arc<Mutex<VecDeque<(u16, String)>>>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockTransport {
    fn scripted(responses: &[&Fixture]) -> Self {
        let mock = Self::default();
        {
            let mut queue = mock.responses.lock().expect("queue");
            for fixture in responses {
                queue.push_back((fixture.status, fixture.response.to_string()));
            }
        }
        mock
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Transport for MockTransport {
    fn post_json(
        &self,
        url: &str,
        _headers: &[(String, String)],
        body: &str,
    ) -> Result<TransportResponse> {
        self.calls
            .lock()
            .expect("calls")
            .push((url.to_owned(), body.to_owned()));
        let (status, body) = self
            .responses
            .lock()
            .expect("queue")
            .pop_front()
            .expect("mock transport received more calls than scripted");
        Ok(TransportResponse { status, body })
    }
}

fn make_client(mock: &MockTransport) -> OpenRouterClient {
    OpenRouterClient::new(BASE_URL, "test-key", Box::new(mock.clone())).expect("client")
}

fn vector_norm(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

#[test]
fn embed_parses_fixture_in_request_order() {
    let fx = fixture(EMBED_QWEN);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let input = texts(&fx.request);
    let dimensions =
        u32::try_from(fx.request["dimensions"].as_u64().expect("dimensions")).expect("u32");

    let vectors =
        blocking::enter(|| client.embed_texts("qwen/qwen3-embedding-8b", dimensions, true, &input))
            .expect("embedding must parse");
    assert_eq!(vectors.len(), input.len());
    for vector in &vectors {
        assert_eq!(vector.len(), dimensions as usize);
        assert!(
            (vector_norm(vector) - 1.0).abs() < 1e-3,
            "qwen3 vectors are already normalized; L2 is idempotent"
        );
    }

    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.ends_with("/embeddings"), "{}", calls[0].0);
    let sent: Value = serde_json::from_str(&calls[0].1).expect("request json");
    assert_eq!(sent["input_type"], "search_document");
    assert_eq!(sent["dimensions"], dimensions);
    assert_eq!(sent["model"], "qwen/qwen3-embedding-8b");
}

#[test]
fn l2_normalizes_raw_provider_vectors_only_when_enabled() {
    let fx = fixture(EMBED_PPLX);
    let input = texts(&fx.request);
    let model = "perplexity/pplx-embed-v1-0.6b";
    let dimensions = u32::try_from(
        fx.response["data"][0]["embedding"]
            .as_array()
            .expect("vector")
            .len() as u64,
    )
    .expect("u32");

    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let normalized = blocking::enter(|| client.embed_texts(model, dimensions, true, &input))
        .expect("normalized");
    for vector in &normalized {
        assert!((vector_norm(vector) - 1.0).abs() < 1e-4);
    }

    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let raw =
        blocking::enter(|| client.embed_texts(model, dimensions, false, &input)).expect("raw");
    assert!(
        (vector_norm(&raw[0]) - 4.83).abs() < 0.5,
        "the pplx fixture records an unnormalized vector (4.83)"
    );
}

#[test]
fn rerank_maps_provider_indices_and_sorts_desc() {
    let fx = fixture(RERANK_V35);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let docs = documents(&fx.request);
    let top_n = usize::try_from(fx.request["top_n"].as_u64().expect("top_n")).expect("usize");

    let ranked = blocking::enter(|| client.rerank("cohere/rerank-v3.5", "query", &docs, top_n))
        .expect("rerank must parse");
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0, 0);
    assert_eq!(ranked[1].0, 3);
    assert!(ranked[0].1 > ranked[1].1, "descending relevance");
    assert!((ranked[0].1 - 0.4452).abs() < 0.01);

    let calls = mock.calls();
    assert!(calls[0].0.ends_with("/rerank"), "{}", calls[0].0);
    let sent: Value = serde_json::from_str(&calls[0].1).expect("request json");
    assert_eq!(sent["top_n"], top_n);
    assert_eq!(sent["documents"], fx.request["documents"]);
}

#[test]
fn reranker_clamps_window_to_document_count() {
    let fixture = fixture(RERANK_CLAMP);
    let mock = MockTransport::scripted(&[&fixture]);
    let client = Arc::new(make_client(&mock));
    let reranker = OpenRouterReranker::with_client(RerankerCfg::default(), client);
    let docs = documents(&fixture.request);

    // Config window (50) is larger than the candidate list: the stage sends
    // the document count, the provider returns it, and no more are invented.
    let ranked = blocking::enter(|| reranker.rerank("query", &docs, 50)).expect("rerank");
    assert!(ranked.len() <= docs.len());
    let calls = mock.calls();
    let sent: Value = serde_json::from_str(&calls[0].1).expect("request json");
    assert_eq!(sent["top_n"], docs.len());
}

#[test]
fn reranking_no_candidates_never_calls_the_provider() {
    let mock = MockTransport::default();
    let client = Arc::new(make_client(&mock));
    let reranker = OpenRouterReranker::with_client(RerankerCfg::default(), client);
    let ranked = blocking::enter(|| reranker.rerank("query", &[], 10)).expect("empty ok");
    assert!(ranked.is_empty());
    assert!(mock.calls().is_empty(), "no HTTP call for empty candidates");
}

#[test]
fn provider_errors_map_to_explicit_messages() {
    let fx = fixture(EMBED_404);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let error = blocking::enter(|| {
        client.embed_texts("qwen/qwen3-embedding-0.6b", 1024, true, &["x".to_owned()])
    })
    .expect_err("404 must fail");
    let message = format!("{error}");
    assert!(message.contains("model not found"), "{message}");
    assert!(message.contains("No endpoints found"), "{message}");

    let fx = fixture(EMBED_400);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let error = blocking::enter(|| {
        client.embed_texts("qwen/qwen3-embedding-8b", 1024, true, &["x".to_owned()])
    })
    .expect_err("400 must fail");
    let message = format!("{error}");
    assert!(message.contains("rejected the request"), "{message}");
    assert!(message.contains("Input is empty"), "{message}");
}

#[test]
fn retryable_status_is_retried_once_then_succeeds() {
    let fx = fixture(EMBED_QWEN);
    let failing = Fixture {
        request: fx.request.clone(),
        status: 503,
        response: serde_json::json!({"error": {"message": "Service Unavailable"}}),
    };
    let mock = MockTransport::scripted(&[&failing, &fx]);
    let client = make_client(&mock);
    let input = texts(&fx.request);

    let vectors =
        blocking::enter(|| client.embed_texts("qwen/qwen3-embedding-8b", 1024, true, &input))
            .expect("retry must succeed");
    assert_eq!(vectors.len(), input.len());
    assert_eq!(mock.calls().len(), 2, "one retry after the 503");
}

#[test]
fn non_retryable_status_is_not_retried() {
    let fx = fixture(EMBED_400);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let _ = blocking::enter(|| client.embed_texts("m", 1024, true, &["x".to_owned()]));
    assert_eq!(mock.calls().len(), 1, "400 must not be retried");
}

#[test]
fn unsanctioned_call_is_an_explicit_error() {
    let fx = fixture(EMBED_QWEN);
    let mock = MockTransport::scripted(&[&fx]);
    let client = make_client(&mock);
    let error = client
        .embed_texts("qwen/qwen3-embedding-8b", 1024, true, &["x".to_owned()])
        .expect_err("calling outside blocking::enter must fail (I3)");
    let message = format!("{error}");
    assert!(message.contains("blocking::enter"), "{message}");
    assert!(
        mock.calls().is_empty(),
        "the guard fires before any HTTP call"
    );
}

#[test]
fn debug_never_leaks_the_api_key() {
    let mock = MockTransport::default();
    let client =
        OpenRouterClient::new(BASE_URL, "sk-or-super-secret", Box::new(mock)).expect("client");
    let debug = format!("{client:?}");
    assert!(!debug.contains("sk-or-super-secret"), "{debug}");
    assert!(debug.contains("ApiKey(***)"), "{debug}");
}

#[test]
fn base_url_outside_the_allowlist_is_rejected() {
    let mock = MockTransport::default();
    let error = OpenRouterClient::new("https://evil.example/api/v1", "key", Box::new(mock))
        .expect_err("allowlist");
    assert!(matches!(error, Error::Admission { .. }), "{error}");
}

#[test]
fn embedder_adapter_reports_the_cache_key() {
    let cfg = EmbeddingCfg::default();
    let mock = MockTransport::default();
    let client = Arc::new(make_client(&mock));
    let embedder = OpenRouterEmbedder::with_client(cfg, client);
    assert_eq!(
        embedder.model_id(),
        "openrouter/qwen/qwen3-embedding-8b@d1024@l2"
    );
}
