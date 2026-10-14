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
}

/// Minimal synchronous HTTP seam; the real implementation owns the runtime.
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
}

/// Maximum input texts per embeddings request (FR-6).
pub const EMBED_BATCH_TEXTS: usize = 128;

/// Byte budget per embeddings request; a single text may exceed it and goes
/// alone in its own request (FR-6).
pub const EMBED_BATCH_BYTES: usize = 512 * 1024;

/// Total attempts (first try plus retries) for retryable failures (FR-7).
pub const RETRY_ATTEMPTS: u32 = 4;

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
                .map(str::to_owned)
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
    let mut delay = Duration::from_millis(250);
    let mut last: Option<Error> = None;
    for attempt in 1..=RETRY_ATTEMPTS {
        match transport.post_json(url, headers, body) {
            Ok(response) if (200..300).contains(&response.status) => return Ok(response),
            Ok(response) if is_retryable(response.status) => {
                if attempt == RETRY_ATTEMPTS {
                    return Err(error_for_status(response.status, &response.body));
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
        std::thread::sleep(delay.saturating_add(jitter()));
        delay = delay.saturating_mul(2).min(Duration::from_secs(4));
    }
    Err(last.unwrap_or_else(|| Error::internal("OpenRouter retry loop exhausted")))
}

/// Up to 50 ms of deterministic-from-time jitter to avoid synchronized bursts.
fn jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    Duration::from_millis(u64::from(nanos % 50))
}

/// Splits input texts into request batches bounded by count and bytes; one
/// oversized text forms a batch on its own (FR-6).
#[must_use]
pub fn embed_batches(texts: &[String]) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0usize;
    for (index, text) in texts.iter().enumerate() {
        let size = text.len();
        let count_full = index - start >= EMBED_BATCH_TEXTS;
        let bytes_full = index > start && bytes.saturating_add(size) > EMBED_BATCH_BYTES;
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
