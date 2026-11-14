//! HTTP-independent wire helpers: the transport seam, the retry policy, error
//! mapping and request chunking (FR-1, FR-6, FR-7, FR-9; SC-5).
//!
//! Wire tests replay the recorded T1 fixtures (`tests/fixtures/openrouter/`)
//! through a mock [`Transport`], so CI needs no network.

use std::ops::Range;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// One recorded HTTP response; independent of the HTTP stack (SC-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body verbatim.
    pub body: String,
    /// `Retry-After` in seconds when the provider sent one (FR-7).
    pub retry_after: Option<Duration>,
}

/// Minimal synchronous HTTP seam; the real implementation uses a root-owned runtime's
/// retirable Handle bridge and never owns a Runtime.
pub trait Transport: Send + Sync {
    /// Sends one JSON POST request.
    ///
    /// # Errors
    /// Transport-level failures (DNS, TLS, timeouts).
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<TransportResponse>;

    /// Sends one JSON GET request (no body). Used by the connect key probe
    /// (FR-3: 0 model tokens).
    ///
    /// # Errors
    /// Transport-level failures (DNS, TLS, timeouts).
    fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<TransportResponse>;
}

/// Maximum input texts per embeddings request (FR-6).
pub const EMBED_BATCH_TEXTS: usize = super::remote_settings::RemoteSpec::DEFAULTS.embed_batch_texts;

/// Byte budget per embeddings request; a single text may exceed it and goes
/// alone in its own request (FR-6).
pub const EMBED_BATCH_BYTES: usize = super::remote_settings::RemoteSpec::DEFAULTS.embed_batch_bytes;

/// Total attempts (first try plus retries) for retryable failures (FR-7).
pub const RETRY_ATTEMPTS: u32 = super::remote_settings::RemoteSpec::DEFAULTS.retry_attempts;

/// Upper bound for a provider-requested `Retry-After` (a rogue value must not
/// stall an interactive search forever).
pub const MAX_RETRY_AFTER: Duration =
    Duration::from_millis(super::remote_settings::RemoteSpec::DEFAULTS.max_retry_after_ms);

/// Statuses that get an exponential-backoff retry (FR-7, spike table).
#[must_use]
pub fn is_retryable(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 524 | 529)
}

/// Maps a non-2xx status to an explicit error (FR-7); no retry judgement
/// here: the caller already decided.
#[must_use]
pub fn error_for_status(status: u16, body: &str) -> Error {
    let message = error_message(body);
    match status {
        400 => Error::internal(format!("OpenRouter rejected the request: {message}")),
        401 | 403 => Error::internal(format!(
            "OpenRouter authentication failed ({status}): {message}; check OPENROUTER_API_KEY"
        )),
        402 => Error::internal(format!(
            "OpenRouter payment required: {message}; top up or disable hybrid"
        )),
        404 => Error::internal(format!(
            "OpenRouter model not found: {message}; check hybrid provider settings"
        )),
        413 => Error::internal(format!("OpenRouter payload too large: {message}")),
        _ => Error::internal(format!("OpenRouter error {status}: {message}")),
    }
}

/// Extracts `error.message` from the provider envelope, falling back to a
/// truncated raw body.
#[must_use]
pub fn error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")?
                .get("message")?
                .as_str()
                .map(|message| truncate(message, 300))
        });
    parsed.unwrap_or_else(|| truncate(body, 300))
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// POSTs with exponential backoff and jitter for retryable statuses and
/// transport failures (FR-7). Non-retryable statuses fail immediately.
///
/// # Errors
/// Returns the last retryable failure, or the first non-retryable one.
pub fn post_with_retry(
    transport: &dyn Transport,
    url: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<TransportResponse> {
    post_with_retry_with_timing(transport, url, headers, body, std::thread::sleep, jitter)
}

// Per-call timing boundary: tests observe policy without consuming real backoff.
fn post_with_retry_with_timing(
    transport: &dyn Transport,
    url: &str,
    headers: &[(String, String)],
    body: &str,
    mut sleep: impl FnMut(Duration),
    mut jitter: impl FnMut() -> Duration,
) -> Result<TransportResponse> {
    let policy = &super::remote_settings::RemoteSpec::DEFAULTS;
    let mut delay = Duration::from_millis(policy.retry_initial_ms);
    let mut last: Option<Error> = None;
    for attempt in 1..=RETRY_ATTEMPTS {
        let mut wait = None;
        match transport.post_json(url, headers, body) {
            Ok(response) if (200..300).contains(&response.status) => return Ok(response),
            Ok(response) if is_retryable(response.status) => {
                if attempt == RETRY_ATTEMPTS {
                    return Err(error_for_status(response.status, &response.body));
                }
                if response.status == 429 {
                    // Honor Retry-After (capped), falling back to our backoff.
                    wait = response.retry_after;
                }
                last = Some(error_for_status(response.status, &response.body));
            }
            Ok(response) => return Err(error_for_status(response.status, &response.body)),
            Err(err) => {
                if attempt == RETRY_ATTEMPTS {
                    return Err(err);
                }
                last = Some(err);
            }
        }
        let pause = wait.map_or(delay, |after| after.min(MAX_RETRY_AFTER));
        sleep(pause.saturating_add(jitter()));
        delay = delay
            .saturating_mul(policy.retry_multiplier)
            .min(Duration::from_millis(policy.retry_max_ms));
    }
    Err(last.unwrap_or_else(|| Error::internal("OpenRouter retry loop exhausted")))
}

/// Up to 50 ms of deterministic-from-time jitter to avoid synchronized bursts.
fn jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    Duration::from_millis(
        u64::from(nanos) % super::remote_settings::RemoteSpec::DEFAULTS.jitter_max_ms,
    )
}

/// Splits input texts into request batches bounded by count and bytes; one
/// oversized text forms a batch on its own (FR-6).
#[must_use]
pub fn embed_batches(texts: &[String]) -> Vec<Range<usize>> {
    embed_batches_with_settings(texts, &super::remote_settings::RemoteSpec::DEFAULTS)
}

/// Ordered batching using complete owner policy; oversized single texts remain alone.
#[must_use]
pub fn embed_batches_with_settings(
    texts: &[String],
    settings: &super::remote_settings::RemoteSettings,
) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0usize;
    for (index, text) in texts.iter().enumerate() {
        let size = text.len();
        let count_full = index - start >= settings.embed_batch_texts;
        let bytes_full = index > start && bytes.saturating_add(size) > settings.embed_batch_bytes;
        if count_full || bytes_full {
            batches.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(size);
    }
    if start < texts.len() {
        batches.push(start..texts.len());
    }
    batches
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum RetryEvent {
        Post,
        Jitter,
        Sleep(Duration),
    }

    use RetryEvent::{Jitter, Post, Sleep};

    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<TransportResponse>>>,
        events: Mutex<Vec<RetryEvent>>,
    }

    impl ScriptedTransport {
        fn new(responses: impl IntoIterator<Item = Result<TransportResponse>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                events: Mutex::new(Vec::new()),
            }
        }

        fn run(&self, jitter_ms: &[u64]) -> Result<TransportResponse> {
            let mut jitter_ms = jitter_ms.iter();
            let result = post_with_retry_with_timing(
                self,
                "https://example.test/embeddings",
                &[("Authorization".to_owned(), "Bearer test-key".to_owned())],
                r#"{"input":["same input"]}"#,
                |duration| self.events.lock().expect("events").push(Sleep(duration)),
                || {
                    self.events.lock().expect("events").push(Jitter);
                    Duration::from_millis(*jitter_ms.next().expect("unexpected jitter request"))
                },
            );
            assert_eq!(self.responses.lock().expect("responses").len(), 0);
            assert!(jitter_ms.next().is_none(), "all jitter samples used");
            result
        }

        fn assert_events(&self, expected: &[RetryEvent]) {
            assert_eq!(*self.events.lock().expect("events"), expected);
        }
    }

    impl Transport for ScriptedTransport {
        fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &str,
        ) -> Result<TransportResponse> {
            assert_eq!(url, "https://example.test/embeddings");
            assert_eq!(
                headers,
                &[("Authorization".to_owned(), "Bearer test-key".to_owned())]
            );
            assert_eq!(body, r#"{"input":["same input"]}"#);
            self.events.lock().expect("events").push(Post);
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("unexpected extra POST")
        }

        fn get_json(&self, _url: &str, _headers: &[(String, String)]) -> Result<TransportResponse> {
            panic!("retrying a POST must not send a GET");
        }
    }

    fn response(status: u16, retry_after: Option<Duration>) -> TransportResponse {
        TransportResponse {
            status,
            body: format!("body {status}"),
            retry_after,
        }
    }

    fn wait(ms: u64) -> RetryEvent {
        Sleep(Duration::from_millis(ms))
    }

    #[test]
    fn retry_after_zero_nonzero_cap_and_fallback() {
        for (retry_after, expected_ms) in [
            (None, 257),
            (Some(Duration::ZERO), 7),
            (Some(Duration::from_secs(2)), 2007),
            (Some(Duration::from_secs(29)), 29007),
            (Some(Duration::from_secs(30)), 30007),
            (Some(Duration::from_secs(31)), 30007),
            (Some(Duration::MAX), 30007),
        ] {
            let success = response(200, None);
            let transport =
                ScriptedTransport::new([Ok(response(429, retry_after)), Ok(success.clone())]);
            assert_eq!(transport.run(&[7]).expect("retry succeeds"), success);
            transport.assert_events(&[Post, Jitter, wait(expected_ms), Post]);
        }
    }

    #[test]
    fn retry_backoff_advances_through_header_and_transport_failure() {
        let success = response(201, None);
        let transport = ScriptedTransport::new([
            Ok(response(429, Some(Duration::from_secs(2)))),
            Err(Error::transport("connection reset")),
            Ok(response(503, None)),
            Ok(success.clone()),
        ]);
        assert_eq!(
            transport.run(&[7, 11, 13]).expect("retry succeeds"),
            success
        );
        transport.assert_events(&[
            Post,
            Jitter,
            wait(2007),
            Post,
            Jitter,
            wait(511),
            Post,
            Jitter,
            wait(1013),
            Post,
        ]);
    }

    #[test]
    fn retryable_statuses_retry_and_only_429_uses_retry_after() {
        for status in [429, 500, 502, 503, 524, 529] {
            let success = response(200, None);
            let transport = ScriptedTransport::new([
                Ok(response(status, Some(Duration::from_secs(2)))),
                Ok(success.clone()),
            ]);
            assert_eq!(transport.run(&[7]).expect("retry succeeds"), success);
            let expected_ms = if status == 429 { 2007 } else { 257 };
            transport.assert_events(&[Post, Jitter, wait(expected_ms), Post]);
        }
    }

    #[test]
    fn retry_success_and_nonretryable_statuses_finish_without_waiting() {
        for status in [200, 201, 204, 299, 400, 401, 402, 403, 404, 413] {
            let reply = response(status, Some(Duration::from_secs(2)));
            let transport = ScriptedTransport::new([Ok(reply.clone())]);
            let result = transport.run(&[]);
            if status < 300 {
                assert_eq!(result.expect("success"), reply);
            } else {
                let error = result.expect_err("nonretryable status");
                assert!(matches!(error, Error::Internal { .. }));
                assert_eq!(
                    error.to_string(),
                    error_for_status(status, &reply.body).to_string()
                );
            }
            transport.assert_events(&[Post]);
        }
    }

    #[test]
    fn retry_http_exhaustion_returns_last_error_without_final_wait() {
        let transport =
            ScriptedTransport::new([500, 502, 503, 529].map(|status| Ok(response(status, None))));
        let error = transport
            .run(&[1, 2, 3])
            .expect_err("four failures exhaust budget");
        assert!(matches!(error, Error::Internal { .. }));
        assert_eq!(error.inner_message(), "OpenRouter error 529: body 529");
        transport.assert_events(&[
            Post,
            Jitter,
            wait(251),
            Post,
            Jitter,
            wait(502),
            Post,
            Jitter,
            wait(1003),
            Post,
        ]);
    }

    #[test]
    fn retry_transport_exhaustion_preserves_last_error_and_source() {
        let transport = ScriptedTransport::new([
            Err(Error::transport("first failure")),
            Err(Error::transport("second failure")),
            Err(Error::transport("third failure")),
            Err(Error::transport_with_source(
                "terminal transport failure",
                std::io::Error::other("last cause"),
            )),
        ]);
        let error = transport
            .run(&[1, 2, 3])
            .expect_err("four failures exhaust budget");
        assert!(matches!(error, Error::Transport { .. }));
        assert_eq!(error.inner_message(), "terminal transport failure");
        assert_eq!(
            std::error::Error::source(&error)
                .expect("source retained")
                .to_string(),
            "last cause"
        );
        transport.assert_events(&[
            Post,
            Jitter,
            wait(251),
            Post,
            Jitter,
            wait(502),
            Post,
            Jitter,
            wait(1003),
            Post,
        ]);
    }

    fn texts(count: usize, bytes: usize) -> Vec<String> {
        (0..count).map(|_| "x".repeat(bytes)).collect()
    }

    #[test]
    fn batches_split_on_count_boundary() {
        let texts = texts(EMBED_BATCH_TEXTS + 5, 1);
        let batches = embed_batches(&texts);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], 0..EMBED_BATCH_TEXTS);
        assert_eq!(batches[1], EMBED_BATCH_TEXTS..texts.len());
    }

    #[test]
    fn batches_split_on_byte_budget() {
        // Two chunks fit; the third crosses the byte budget.
        let half = EMBED_BATCH_BYTES / 2;
        let texts = texts(3, half);
        let batches = embed_batches(&texts);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], 0..2);
        assert_eq!(batches[1], 2..3);
    }

    #[test]
    fn an_oversized_single_text_goes_alone() {
        let texts = vec!["a".repeat(EMBED_BATCH_BYTES + 1), "small".to_owned()];
        let batches = embed_batches(&texts);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], 0..1);
        assert_eq!(batches[1], 1..2);
    }

    #[test]
    fn empty_input_produces_no_batches() {
        assert_eq!(embed_batches(&[]).len(), 0);
    }

    #[test]
    fn error_message_prefers_the_provider_envelope_and_truncates() {
        let envelope = r#"{"error":{"message":"Input is empty"}}"#;
        assert_eq!(error_message(envelope), "Input is empty");
        let raw = "x".repeat(1000);
        let message = error_message(&raw);
        assert!(message.ends_with('…'), "raw bodies are truncated");
        assert!(message.len() < 400);
    }
}
