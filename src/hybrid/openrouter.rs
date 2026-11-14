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
    transport: Option<Box<dyn Transport>>,
    execution: Option<Arc<super::remote::RemoteExecution>>,
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
            transport: Some(transport),
            execution: None,
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

    /// Build an async-capable client on an explicit execution owner, without a sync fallback.
    ///
    /// # Errors
    /// Invalid allowlisted endpoint or empty credential.
    pub fn with_execution(
        base_url: &str,
        api_key: &str,
        execution: Arc<super::remote::RemoteExecution>,
    ) -> Result<Self> {
        if base_url.trim_end_matches('/') != BASE_URL || api_key.trim().is_empty() {
            return Err(Error::Admission {
                message: "invalid OpenRouter endpoint or empty credential".to_owned(),
            });
        }
        Ok(Self {
            base_url: BASE_URL.to_owned(),
            api_key: ApiKey(api_key.to_owned()),
            transport: None,
            execution: Some(execution),
        })
    }

    /// Read the existing credential once while composing explicit execution.
    ///
    /// # Errors
    /// Missing/empty credential or invalid client policy.
    pub fn from_env_with_execution(execution: Arc<super::remote::RemoteExecution>) -> Result<Self> {
        let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
            message: "OPENROUTER_API_KEY is not set; export it or disable hybrid".to_owned(),
        })?;
        Self::with_execution(BASE_URL, &key, execution)
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
                self.sync_transport()?,
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
            self.sync_transport()?,
            model,
            query,
            docs,
            top_n,
        )
    }

    fn sync_transport(&self) -> Result<&dyn Transport> {
        self.transport.as_deref().ok_or_else(|| Error::Admission {
            message: "this remote stage requires async orchestration".to_owned(),
        })
    }

    async fn embed_async(
        self: &Arc<Self>,
        model: String,
        dimensions: u32,
        normalize: bool,
        texts: Arc<[String]>,
        input_type: &'static str,
        call: &super::remote::RemoteCall,
    ) -> Result<Vec<Vec<f32>>> {
        let execution = self.execution.as_ref().ok_or_else(|| Error::Admission {
            message: "remote execution owner is required".to_owned(),
        })?;
        let stage = execution.admit(call).await?;
        let mut vectors = Vec::new();
        for range in wire::embed_batches_with_settings(&texts, execution.settings()) {
            let client = Arc::clone(self);
            let input = Arc::clone(&texts);
            let model = model.clone();
            let prepared = execution
                .cpu(call, move || {
                    super::rig_codec::OpenRouterCodec::new(&client.api_key.0).prepare_embedding(
                        &model,
                        dimensions,
                        &input[range],
                        input_type,
                    )
                })
                .await?;
            let response = stage.post(call, &prepared.request).await?;
            let batch = execution
                .cpu(call, move || {
                    prepared.decode(&response.body).map(|vectors| {
                        vectors
                            .into_iter()
                            .map(|vector| {
                                if normalize {
                                    l2_normalize(vector)
                                } else {
                                    vector
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .await?;
            vectors.extend(batch);
        }
        Ok(vectors)
    }

    async fn rerank_async(
        self: &Arc<Self>,
        model: String,
        query: Arc<str>,
        documents: Arc<[String]>,
        top_n: usize,
        call: &super::remote::RemoteCall,
    ) -> Result<Vec<(usize, f32)>> {
        if documents.is_empty() || top_n == 0 {
            return Ok(Vec::new());
        }
        let execution = self.execution.as_ref().ok_or_else(|| Error::Admission {
            message: "remote execution owner is required".to_owned(),
        })?;
        let stage = execution.admit(call).await?;
        let client = Arc::clone(self);
        let prepared = execution
            .cpu(call, move || {
                super::rig_codec::OpenRouterCodec::new(&client.api_key.0).prepare_rerank(
                    &model,
                    &query,
                    &documents,
                    top_n.min(documents.len()),
                )
            })
            .await?;
        let response = stage.post(call, &prepared.request).await?;
        execution
            .cpu(call, move || prepared.decode(&response.body))
            .await
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
    fn async_remote(&self) -> Option<&dyn super::remote::AsyncEmbedder> {
        self.client
            .execution
            .as_ref()
            .map(|_| self as &dyn super::remote::AsyncEmbedder)
    }
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

impl super::remote::AsyncEmbedder for OpenRouterEmbedder {
    fn embed<'a>(
        &'a self,
        texts: Arc<[String]>,
        call: &'a super::remote::RemoteCall,
    ) -> super::remote::StageFuture<'a, Vec<Vec<f32>>> {
        Box::pin(self.client.embed_async(
            self.cfg.model.clone(),
            self.cfg.dimensions,
            self.cfg.normalize,
            texts,
            INPUT_TYPE_DOCUMENT,
            call,
        ))
    }

    fn embed_query<'a>(
        &'a self,
        text: Arc<str>,
        call: &'a super::remote::RemoteCall,
    ) -> super::remote::StageFuture<'a, Vec<f32>> {
        Box::pin(async move {
            let mut vectors = self
                .client
                .embed_async(
                    self.cfg.model.clone(),
                    self.cfg.dimensions,
                    self.cfg.normalize,
                    Arc::from([text.to_string()]),
                    INPUT_TYPE_QUERY,
                    call,
                )
                .await?;
            vectors
                .pop()
                .ok_or_else(|| Error::internal("embed returned no vector for one input"))
        })
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
    fn async_remote(&self) -> Option<&dyn super::remote::AsyncReranker> {
        self.client
            .execution
            .as_ref()
            .map(|_| self as &dyn super::remote::AsyncReranker)
    }
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let effective = top_n.clamp(1, docs.len());
        self.client.rerank(&self.cfg.model, query, docs, effective)
    }
}

impl super::remote::AsyncReranker for OpenRouterReranker {
    fn rerank<'a>(
        &'a self,
        query: Arc<str>,
        documents: Arc<[String]>,
        top_n: usize,
        call: &'a super::remote::RemoteCall,
    ) -> super::remote::StageFuture<'a, Vec<(usize, f32)>> {
        Box::pin(
            self.client
                .rerank_async(self.cfg.model.clone(), query, documents, top_n, call),
        )
    }
}

/// Runtime-free reqwest transport with an explicitly bounded DNS resolver.
pub struct ReqwestTransport {
    http: reqwest::Client,
    max_response_bytes: u64,
    attempt_timeout: Duration,
    sync_bridge: Option<Arc<crate::daemon::remote::SyncBridge>>,
}

impl ReqwestTransport {
    /// Resolve standalone compatibility policy, then build a runtime-free client.
    ///
    /// # Errors
    /// Invalid owner policy or HTTP client construction failure.
    pub fn new() -> Result<Self> {
        let settings = super::remote_settings::standalone()?;
        let daemon = crate::daemon::settings::standalone()?;
        let resolver = Arc::new(crate::daemon::remote::BoundedResolver::with_settings(
            settings.settings(),
            daemon.settings(),
        )?);
        let mut transport = Self::with_settings(settings.settings(), resolver)?;
        transport.sync_bridge = blocking::bridge();
        Ok(transport)
    }

    /// Construct a runtime-free client with explicitly owned policy and resolver.
    ///
    /// # Errors
    /// Invalid policy or HTTP client construction failure.
    pub fn with_settings<R: reqwest::dns::Resolve + 'static>(
        settings: &super::remote_settings::RemoteSettings,
        resolver: Arc<R>,
    ) -> Result<Self> {
        use crate::config::schema::{Capabilities, SettingsSpec};
        super::remote_settings::RemoteSpec::validate(settings, &Capabilities::current())?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(settings.attempt_timeout_ms))
            .pool_max_idle_per_host(settings.interactive_active)
            .dns_resolver(resolver)
            .build()
            .map_err(|err| Error::internal_with_source("build OpenRouter http client", err))?;
        Ok(Self {
            http,
            max_response_bytes: settings.max_response_bytes,
            attempt_timeout: Duration::from_millis(settings.attempt_timeout_ms),
            sync_bridge: None,
        })
    }

    fn block_on<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        if !blocking::in_context() {
            return Err(Error::Admission {
                message: "synchronous remote transport requires a sanctioned blocking context"
                    .to_owned(),
            });
        }
        self.sync_bridge
            .as_ref()
            .ok_or_else(|| Error::Admission {
                message: "synchronous remote transport requires an explicit command runtime"
                    .to_owned(),
            })?
            .block_on(future)
    }
}

impl super::remote::AsyncTransport for ReqwestTransport {
    fn post_json<'a>(
        &'a self,
        request: &'a super::remote::WireRequest,
        timeout: Duration,
    ) -> super::remote::StageFuture<'a, TransportResponse> {
        Box::pin(async move {
            let mut send = self
                .http
                .post(&request.url)
                .timeout(timeout.min(self.attempt_timeout))
                .body(request.body.clone());
            for (name, value) in &request.headers {
                send = send.header(name, value);
            }
            let response = send.send().await.map_err(|error| {
                Error::internal_with_source("OpenRouter request failed", error.without_url())
            })?;
            read_response(response, self.max_response_bytes).await
        })
    }
}

async fn read_response(mut response: reqwest::Response, maximum: u64) -> Result<TransportResponse> {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        Error::internal_with_source("read OpenRouter response", error.without_url())
    })? {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| Error::internal("remote response byte overflow"))?;
        if u64::try_from(next).map_or(true, |length| length > maximum) {
            return Err(Error::Admission {
                message: "remote response byte limit exceeded".to_owned(),
            });
        }
        bytes
            .try_reserve_exact(chunk.len())
            .map_err(|error| Error::internal_with_source("reserve remote response", error))?;
        bytes.extend_from_slice(&chunk);
    }
    let body =
        String::from_utf8(bytes).map_err(|_| Error::internal("remote response is not UTF-8"))?;
    Ok(TransportResponse {
        status,
        body,
        retry_after,
    })
}

impl Transport for ReqwestTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<TransportResponse> {
        let request = super::remote::WireRequest {
            url: url.to_owned(),
            headers: headers.to_vec(),
            body: body.to_owned(),
        };
        self.block_on(super::remote::AsyncTransport::post_json(
            self,
            &request,
            self.attempt_timeout,
        ))
    }

    fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<TransportResponse> {
        self.block_on(async {
            let mut request = self.http.get(url);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request.send().await.map_err(|error| {
                Error::internal_with_source("OpenRouter request failed", error.without_url())
            })?;
            read_response(response, self.max_response_bytes).await
        })
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
