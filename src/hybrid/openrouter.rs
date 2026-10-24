//! `OpenRouter` client: `OpenAI`-compatible embeddings and the rerank router
//! (FR-1, FR-2, FR-5, FR-8, FR-9). Recorded response fixtures live in
//! `tests/fixtures/openrouter/` (T1 spike, 2026-10-14).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::vector_cache::Embedder;

use super::config::{EmbeddingCfg, RerankerCfg, embedding_model_id};
use super::wire::{self, Transport, TransportResponse};
use super::{Reranker, blocking};

/// The only provider origin/path the allowlist accepts (NFR-3).
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

/// App attribution headers (FR-9; values are OQ-2, finalized here).
const HTTP_REFERER: &str = "https://github.com/punkhomov/docsbase-memory-mcp";
pub(crate) const X_TITLE: &str = "docsbase-memory-mcp";

/// Input type for chunk (document) embeddings; query side would use
/// `search_query` when a dense stage arrives (iteration 2).
const INPUT_TYPE_DOCUMENT: &str = "search_document";

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

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_owned(),
                format!("Bearer {}", self.api_key.0),
            ),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("HTTP-Referer".to_owned(), HTTP_REFERER.to_owned()),
            ("X-Title".to_owned(), X_TITLE.to_owned()),
        ]
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
        ensure_context()?;
        let url = format!("{}/embeddings", self.base_url);
        let mut vectors = Vec::with_capacity(texts.len());
        for range in wire::embed_batches(texts) {
            let batch = &texts[range];
            let request = EmbeddingsRequest {
                model,
                input: batch,
                dimensions,
                input_type: INPUT_TYPE_DOCUMENT,
            };
            let body = serde_json::to_string(&request)
                .map_err(|err| Error::internal_with_source("encode embeddings request", err))?;
            let response = wire::post_with_retry(&*self.transport, &url, &self.headers(), &body)?;
            let parsed: EmbeddingsResponse =
                serde_json::from_str(&response.body).map_err(|err| {
                    Error::internal_with_source(
                        format!("incompatible OpenRouter embeddings schema: {err}"),
                        err,
                    )
                })?;
            if parsed.data.len() != batch.len() {
                return Err(Error::internal(format!(
                    "OpenRouter returned {} vectors for {} inputs",
                    parsed.data.len(),
                    batch.len()
                )));
            }
            // The provider must place each input exactly once when it sends
            // indices (design §5: count + dim + index-range validation) -
            // duplicated, missing or out-of-range indices mean schema drift
            // and must never persist a vector under the wrong content hash
            // (I1/A1). The index field is advisory (FR-1): a response without
            // indices is read as input order; mixing both forms is drift.
            let any_index = parsed.data.iter().any(|datum| datum.index.is_some());
            let mut slots: Vec<Option<Vec<f32>>> = (0..batch.len()).map(|_| None).collect();
            for (position, datum) in parsed.data.into_iter().enumerate() {
                let index = match datum.index {
                    Some(index) => index,
                    None if !any_index => position,
                    None => {
                        return Err(Error::internal(
                            "OpenRouter mixed indexed and unindexed embeddings",
                        ));
                    }
                };
                if index >= batch.len() {
                    return Err(Error::internal(format!(
                        "OpenRouter returned index {index} for {} inputs",
                        batch.len()
                    )));
                }
                if slots[index].is_some() {
                    return Err(Error::internal(format!(
                        "OpenRouter returned index {index} twice"
                    )));
                }
                slots[index] = Some(datum.embedding);
            }
            for slot in slots {
                let embedding =
                    slot.ok_or_else(|| Error::internal("OpenRouter skipped an input index"))?;
                if embedding.is_empty() {
                    return Err(Error::internal("OpenRouter returned an empty vector"));
                }
                if embedding.len() != dimensions as usize {
                    return Err(Error::internal(format!(
                        "OpenRouter returned a {}-dimensional vector, expected {dimensions}",
                        embedding.len()
                    )));
                }
                let vector = if normalize {
                    l2_normalize(embedding)
                } else {
                    embedding
                };
                vectors.push(vector);
            }
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
        let url = format!("{}/rerank", self.base_url);
        let request = RerankRequest {
            model,
            query,
            documents: docs,
            top_n,
        };
        let body = serde_json::to_string(&request)
            .map_err(|err| Error::internal_with_source("encode rerank request", err))?;
        let response = wire::post_with_retry(&*self.transport, &url, &self.headers(), &body)?;
        let parsed: RerankResponse = serde_json::from_str(&response.body).map_err(|err| {
            Error::internal_with_source(
                format!("incompatible OpenRouter rerank schema: {err}"),
                err,
            )
        })?;
        if parsed.results.is_empty() {
            // Empty results for non-empty documents are schema drift, not an
            // implicit fallback to BM25 order (A1, FR-11).
            return Err(Error::internal(
                "OpenRouter rerank returned no results for non-empty documents",
            ));
        }
        let mut ranked: Vec<(usize, f32)> = Vec::with_capacity(parsed.results.len());
        for result in parsed.results {
            if result.index >= docs.len() {
                return Err(Error::internal(format!(
                    "OpenRouter rerank returned index {} for {} documents",
                    result.index,
                    docs.len()
                )));
            }
            ranked.push((result.index, result.relevance_score));
        }
        // The provider sorts best-first; re-sort defensively for determinism.
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.truncate(top_n);
        Ok(ranked)
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

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [String],
    dimensions: u32,
    input_type: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    /// Advisory per the provider docs: absent means "input order" (FR-1).
    #[serde(default)]
    index: Option<usize>,
    embedding: Vec<f32>,
}

#[derive(Serialize)]
struct RerankRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    top_n: usize,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f32,
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
