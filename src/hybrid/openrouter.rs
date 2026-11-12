//! `OpenRouter` client: `OpenAI`-compatible embeddings and the rerank router
//! (FR-1, FR-2, FR-5, FR-8, FR-9). Recorded response fixtures live in
//! `tests/fixtures/openrouter/` (T1 spike, 2026-10-14). Runtime codecs use
//! Rig through `rig_codec` (ADR-RIG1); transport/retries remain owned here.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::vector_cache::Embedder;

use super::config::{EmbeddingCfg, RerankerCfg, embedding_model_id};
use super::wire::{self, Transport, TransportResponse};
use super::{Reranker, blocking};

/// The only provider origin/path the allowlist accepts (NFR-3).
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

/// App attribution headers (FR-9; values are OQ-2, finalized here).
pub(crate) const HTTP_REFERER: &str = "https://github.com/punkhomov/docsbase-memory-mcp";
pub(crate) const X_TITLE: &str = "docsbase-memory-mcp";

/// Input type for chunk (document) embeddings.
const INPUT_TYPE_DOCUMENT: &str = "search_document";

/// Input type for query embeddings (FR-10 role-aware path; `embed_query`
/// uses this so the provider embeds queries as queries, not documents).
const INPUT_TYPE_QUERY: &str = "search_query";

/// API key that never leaks through `Debug` (FR-4, I2).
#[derive(Clone)]
struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// Thin client over the two `OpenRouter` endpoints.
pub struct OpenRouterClient {
    base_url: String,
    api_key: ApiKey,
    transport: Box<dyn Transport>,
}

impl std::fmt::Debug for OpenRouterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouterClient")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .finish_non_exhaustive()
    }
}

impl OpenRouterClient {
    /// Builds a client with an explicit transport (wire tests use a mock).
    ///
    /// # Errors
    /// Returns [`Error::Admission`] when `base_url` leaves the allowlist or
    /// the key is empty (NFR-3).
    pub fn new(base_url: &str, api_key: &str, transport: Box<dyn Transport>) -> Result<Self> {
        if base_url.trim_end_matches('/') != BASE_URL {
            return Err(Error::Admission {
                message: format!(
                    "hybrid base_url must be {BASE_URL} (got \"{base_url}\"); other providers are iteration 2+"
                ),
            });
        }
        if api_key.trim().is_empty() {
            return Err(Error::Admission {
                message: "OPENROUTER_API_KEY is empty".to_owned(),
            });
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: ApiKey(api_key.to_owned()),
            transport,
        })
    }

    /// Builds a client for the real endpoint using `OPENROUTER_API_KEY`.
    ///
    /// # Errors
    /// Returns [`Error::Admission`] when the environment variable is missing.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
            message: "OPENROUTER_API_KEY is not set; export it or disable hybrid".to_owned(),
        })?;
        Self::new(BASE_URL, &key, Box::new(ReqwestTransport::new()?))
    }

    /// Embeds `texts` in bounded batches, preserving input order.
    ///
    /// # Errors
    /// Provider/transport failures; a response with a wrong count or
    /// inconsistent dimensions is an explicit [`Error::Internal`] (I1).
    pub fn embed_texts(
        &self,
        model: &str,
        dimensions: u32,
        normalize: bool,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        self.embed_texts_with_type(model, dimensions, normalize, texts, INPUT_TYPE_DOCUMENT)
    }

    /// Like [`embed_texts`], but with an explicit `input_type` (the query
    /// role uses `search_query` — FR-10).
    ///
    /// # Errors
    /// Same as [`embed_texts`] (provider/transport failures; wrong count or
    /// inconsistent dimensions → [`Error::Internal`]).
    pub fn embed_texts_with_type(
        &self,
        model: &str,
        dimensions: u32,
        normalize: bool,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>> {
        ensure_context()?;
        let codec = super::rig_codec::OpenRouterCodec::new(&self.api_key.0);
        let mut vectors = Vec::with_capacity(texts.len());
        for range in wire::embed_batches(texts) {
            let batch = codec.embed(
                &*self.transport,
                model,
                dimensions,
                &texts[range],
                input_type,
            )?;
            vectors.extend(batch.into_iter().map(|vector| {
                if normalize {
                    l2_normalize(vector)
                } else {
                    vector
                }
            }));
        }
        Ok(vectors)
    }

    /// Reranks `docs` against `query`; returns `(original_index, score)`
    /// best-first, capped at `top_n` (FR-2, FR-11).
    ///
    /// # Errors
    /// Provider/transport failures; an out-of-range index from the provider
    /// is an explicit [`Error::Internal`].
    pub fn rerank(
        &self,
        model: &str,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> Result<Vec<(usize, f32)>> {
        ensure_context()?;
        if docs.is_empty() || top_n == 0 {
            return Ok(Vec::new());
        }
        super::rig_codec::OpenRouterCodec::new(&self.api_key.0).rerank(
            &*self.transport,
            model,
            query,
            docs,
            top_n,
        )
    }
}

/// Rejects calls outside [`blocking::enter`] with an explicit error instead
/// of panicking inside `Handle::block_on` (I3).
fn ensure_context() -> Result<()> {
    if blocking::in_context() {
        return Ok(());
    }
    Err(Error::internal(
        "hybrid calls must run inside hybrid::blocking::enter (I3)",
    ))
}

/// L2-normalizes `vector` to unit length; an all-zero vector is returned
/// unchanged.
fn l2_normalize(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

/// `Embedder` adapter over [`OpenRouterClient`] (FR-1, FR-5).
pub struct OpenRouterEmbedder {
    client: Arc<OpenRouterClient>,
    cfg: EmbeddingCfg,
    model_id: String,
}

impl OpenRouterEmbedder {
    /// Builds the stage with an explicit client (shared with the reranker).
    #[must_use]
    pub fn with_client(cfg: EmbeddingCfg, client: Arc<OpenRouterClient>) -> Self {
        let model_id = embedding_model_id(&cfg);
        Self {
            client,
            cfg,
            model_id,
        }
    }

    /// Builds the stage with a real client from the environment.
    ///
    /// # Errors
    /// Propagates client construction (missing key, allowlist).
    pub fn new(cfg: EmbeddingCfg) -> Result<Self> {
        let client = Arc::new(OpenRouterClient::from_env()?);
        Ok(Self::with_client(cfg, client))
    }
}

impl Embedder for OpenRouterEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.client.embed_texts(
            &self.cfg.model,
            self.cfg.dimensions,
            self.cfg.normalize,
            texts,
        )
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut vectors = self.client.embed_texts_with_type(
            &self.cfg.model,
            self.cfg.dimensions,
            self.cfg.normalize,
            &[text.to_owned()],
            INPUT_TYPE_QUERY,
        )?;
        vectors
            .pop()
            .ok_or_else(|| Error::internal("embed returned no vector for one input"))
    }
}

/// `Reranker` adapter over [`OpenRouterClient`] (FR-2, FR-11).
pub struct OpenRouterReranker {
    client: Arc<OpenRouterClient>,
    cfg: RerankerCfg,
}

impl OpenRouterReranker {
    /// Builds the stage with an explicit client (shared with the embedder).
    #[must_use]
    pub fn with_client(cfg: RerankerCfg, client: Arc<OpenRouterClient>) -> Self {
        Self { client, cfg }
    }

    /// Builds the stage with a real client from the environment.
    ///
    /// # Errors
    /// Propagates client construction (missing key, allowlist).
    pub fn new(cfg: RerankerCfg) -> Result<Self> {
        let client = Arc::new(OpenRouterClient::from_env()?);
        Ok(Self::with_client(cfg, client))
    }
}

impl Reranker for OpenRouterReranker {
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let effective = top_n.clamp(1, docs.len());
        self.client.rerank(&self.cfg.model, query, docs, effective)
    }
}

/// Real transport: async `reqwest` driven from the sanctioned blocking
/// context; reuses the ambient runtime when there is one (daemon blocking
/// threads) and otherwise owns a small current-thread runtime (CLI/tests).
pub struct ReqwestTransport {
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl ReqwestTransport {
    /// Builds the HTTP client and a fallback runtime.
    ///
    /// # Errors
    /// [`Error::Internal`] when the HTTP client or runtime cannot be built.
    pub fn new() -> Result<Self> {
        // 30 s per attempt keeps the worst retry chain (~2 min) bounded for
        // interactive search (NFR-1; a rerank/embed call takes ~100-500 ms).
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| Error::internal_with_source("build OpenRouter http client", err))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| Error::internal_with_source("build OpenRouter runtime", err))?;
        Ok(Self { http, runtime })
    }

    fn block_on<F: Future>(&self, future: F) -> F::Output {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(future),
            Err(_) => self.runtime.block_on(future),
        }
    }
}

impl Transport for ReqwestTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<TransportResponse> {
        let future = async {
            let mut request = self.http.post(url).body(body.to_owned());
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request
                .send()
                .await
                .map_err(|err| Error::internal_with_source("OpenRouter request failed", err))?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response
                .text()
                .await
                .map_err(|err| Error::internal_with_source("read OpenRouter response", err))?;
            Ok(TransportResponse {
                status,
                body,
                retry_after,
            })
        };
        self.block_on(future)
    }

    fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<TransportResponse> {
        let future = async {
            let mut request = self.http.get(url);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request
                .send()
                .await
                .map_err(|err| Error::internal_with_source("OpenRouter request failed", err))?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response
                .text()
                .await
                .map_err(|err| Error::internal_with_source("read OpenRouter response", err))?;
            Ok(TransportResponse {
                status,
                body,
                retry_after,
            })
        };
        self.block_on(future)
    }
}

// ---------- stage probes (single-shot, no retry wrapper) ----------

/// One embed POST, single-shot (no retry wrapper).
#[cfg(feature = "hybrid-remote")]
pub(crate) fn probe_embedding_with(
    transport: &dyn crate::hybrid::wire::Transport,
    model: &str,
    dims: u32,
) -> crate::error::Result<()> {
    let url = format!("{BASE_URL}/embeddings");
    let request = serde_json::json!({
        "model": model,
        "input": ["docsbase probe"],
        "dimensions": dims,
        "input_type": "document",
    });
    let body = serde_json::to_string(&request)
        .map_err(|err| Error::internal_with_source("encode probe embed", err))?;
    let response = transport
        .post_json(&url, &bearer_headers()?, &body)
        .map_err(|err| Error::Admission {
            message: format!(
                "probe embed failed: {}; check the network and retry",
                err.inner_message()
            ),
        })?;
    classify_probe_status(response.status, "embed")?;
    let parsed: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|err| Error::internal_with_source("incompatible OpenRouter probe schema", err))?;
    let width_ok = parsed["data"]
        .get(0)
        .and_then(|datum| datum.get("embedding"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|values| values.len() == dims as usize);
    if !width_ok {
        return Err(Error::internal(format!(
            "probe embed returned a wrong shape for width {dims}"
        )));
    }
    Ok(())
}

/// One rerank POST, single-shot (no retry wrapper).
#[cfg(feature = "hybrid-remote")]
pub(crate) fn probe_reranker_with(
    transport: &dyn crate::hybrid::wire::Transport,
    model: &str,
) -> crate::error::Result<()> {
    let url = format!("{BASE_URL}/rerank");
    let request = serde_json::json!({
        "model": model,
        "query": "docsbase",
        "documents": ["probe"],
        "top_n": 2,
    });
    let body = serde_json::to_string(&request)
        .map_err(|err| Error::internal_with_source("encode probe rerank", err))?;
    let response = transport
        .post_json(&url, &bearer_headers()?, &body)
        .map_err(|err| Error::Admission {
            message: format!(
                "probe rerank failed: {}; check the network and retry",
                err.inner_message()
            ),
        })?;
    classify_probe_status(response.status, "rerank")?;
    let parsed: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|err| Error::internal_with_source("incompatible OpenRouter probe schema", err))?;
    let count = parsed["results"].as_array().map_or(0, Vec::len);
    if count != 2 {
        return Err(Error::internal(format!(
            "probe rerank returned {count} scores for two documents"
        )));
    }
    Ok(())
}

/// Maps probe HTTP statuses to admission errors (single-shot: no retry).
#[cfg(feature = "hybrid-remote")]
fn classify_probe_status(status: u16, stage: &str) -> crate::error::Result<()> {
    match status {
        200 => Ok(()),
        401 | 403 => Err(Error::Admission {
            message: format!(
                "probe {stage} was rejected (HTTP {status}); check OPENROUTER_API_KEY"
            ),
        }),
        other => Err(Error::Admission {
            message: format!("probe {stage} returned HTTP {other}; check the network and retry"),
        }),
    }
}

/// Bearer headers for `OpenRouter` probes; the key is never stored or logged
/// (FR-3).
#[cfg(feature = "hybrid-remote")]
pub(crate) fn bearer_headers() -> crate::error::Result<Vec<(String, String)>> {
    let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
        message: "OPENROUTER_API_KEY is not set; export it to use OpenRouter".to_owned(),
    })?;
    if key.trim().is_empty() {
        return Err(Error::Admission {
            message: "OPENROUTER_API_KEY is empty".to_owned(),
        });
    }
    Ok(vec![
        ("Authorization".to_owned(), format!("Bearer {key}")),
        ("X-Title".to_owned(), X_TITLE.to_owned()),
    ])
}
