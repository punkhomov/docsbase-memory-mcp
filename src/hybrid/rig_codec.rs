//! Rig wire codecs on docsbase's transport (ADR-RIG1).
//!
//! Rig owns provider request/response shapes. This boundary preserves the
//! retrieval contract Rig does not express: input roles, indexed embeddings,
//! finite f32 values and unique rerank indices. It starts no runtime and owns
//! no retry policy. The caller must enter the sanctioned blocking context.

use rig_core::operation::RerankRequest;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Embeddings, OPENROUTER, Rerank};
use rig_core::wire::{Body, Encoded, Mode, Wire, WireFrame, decode_reply};
use serde_json::Value;

use super::openrouter::{HTTP_REFERER, X_TITLE};
use super::wire::{Transport, post_with_retry};
use crate::error::{Error, Result};

pub(super) struct OpenRouterCodec(OpenAIConfig);

impl OpenRouterCodec {
    pub(super) fn new(key: &str) -> Self {
        // Rig 0.45's OpenRouter dialect inherits "no rerank". The existing
        // docsbase endpoint/fixtures establish this gateway's rerank contract.
        let mut dialect = OPENROUTER;
        dialect.quirks.rerank.path = "/rerank";
        Self(OpenAIConfig::with_key(&dialect, key.to_owned()))
    }

    pub(super) fn prepare_embedding(
        &self,
        model: &str,
        dimensions: u32,
        texts: &[String],
        input_type: &str,
    ) -> Result<PreparedEmbedding> {
        let codec = Embeddings::new(self.0.clone(), model, Some(dimensions as usize));
        let input = texts.to_vec();
        let encoded = codec
            .encode(input.clone(), Mode::Unary)
            .map_err(|error| Error::internal_with_source("encode Rig embeddings request", error))?;
        let request = encode_request(encoded, |body| {
            body["input_type"] = input_type.into();
            body["dimensions"] = dimensions.into();
        })?;
        Ok(PreparedEmbedding {
            request,
            codec,
            input,
            dimensions,
        })
    }

    pub(super) fn embed(
        &self,
        transport: &dyn Transport,
        model: &str,
        dimensions: u32,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let prepared = self.prepare_embedding(model, dimensions, texts, input_type)?;
        let body = send(transport, &prepared.request)?;
        prepared.decode(&body)
    }

    pub(super) fn prepare_rerank(
        &self,
        model: &str,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> Result<PreparedRerank> {
        let codec = Rerank::new(self.0.clone(), model).with_top_n(top_n);
        let input = RerankRequest {
            query: query.to_owned(),
            documents: docs.to_vec(),
        };
        let encoded = codec
            .encode(input.clone(), Mode::Unary)
            .map_err(|error| Error::internal_with_source("encode Rig rerank request", error))?;
        let request = encode_request(encoded, |_| {})?;
        Ok(PreparedRerank {
            request,
            codec,
            input,
            top_n,
        })
    }

    pub(super) fn rerank(
        &self,
        transport: &dyn Transport,
        model: &str,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> Result<Vec<(usize, f32)>> {
        let prepared = self.prepare_rerank(model, query, docs, top_n)?;
        let body = send(transport, &prepared.request)?;
        prepared.decode(&body)
    }
}

pub(super) struct PreparedEmbedding {
    pub(super) request: super::remote::WireRequest,
    codec: Embeddings,
    input: Vec<String>,
    dimensions: u32,
}
impl PreparedEmbedding {
    pub(super) fn decode(self, body: &str) -> Result<Vec<Vec<f32>>> {
        let response = ordered_embeddings(body, self.input.len())?;
        let decoded = decode_reply(
            &self.codec,
            &self.input,
            Mode::Unary,
            [WireFrame::Bytes(response)],
        )
        .map_err(|err| {
            Error::internal_with_source("incompatible OpenRouter embeddings schema", err)
        })?;
        decoded
            .embeddings
            .into_iter()
            .map(|embedding| {
                if embedding.vec.is_empty() || embedding.vec.len() != self.dimensions as usize {
                    return Err(Error::internal(
                        "OpenRouter returned an invalid vector width",
                    ));
                }
                embedding.vec.into_iter().map(finite_f32).collect()
            })
            .collect()
    }
}

pub(super) struct PreparedRerank {
    pub(super) request: super::remote::WireRequest,
    codec: Rerank,
    input: RerankRequest,
    top_n: usize,
}
impl PreparedRerank {
    pub(super) fn decode(self, body: &str) -> Result<Vec<(usize, f32)>> {
        let mut response: Value = serde_json::from_str(body).map_err(|err| {
            Error::internal_with_source("incompatible OpenRouter rerank schema", err)
        })?;
        let results = response
            .get_mut("results")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| Error::internal("OpenRouter rerank response has no results array"))?;
        // Only index/score belong to the docsbase contract. In recorded Cohere
        // replies document is an object; Rig expects a string. Also do not
        // impose Rig's optional metadata schemas on previously ignored fields.
        for result in results.iter_mut() {
            let object = result
                .as_object_mut()
                .ok_or_else(|| Error::internal("OpenRouter rerank result is not an object"))?;
            object.retain(|key, _| matches!(key.as_str(), "index" | "relevance_score"));
        }
        let projected = serde_json::json!({"results": results});
        let decoded = decode_reply(
            &self.codec,
            &self.input,
            Mode::Unary,
            [WireFrame::Bytes(projected.to_string().into_bytes())],
        )
        .map_err(|err| Error::internal_with_source("incompatible OpenRouter rerank schema", err))?;
        if decoded.results.is_empty() {
            return Err(Error::internal(
                "OpenRouter rerank returned no results for non-empty documents",
            ));
        }
        let mut seen = vec![false; self.input.documents.len()];
        let mut ranked = Vec::with_capacity(decoded.results.len());
        for result in decoded.results {
            let slot = seen.get_mut(result.index).ok_or_else(|| {
                Error::internal(format!(
                    "OpenRouter rerank returned index {} for {} documents",
                    result.index,
                    self.input.documents.len()
                ))
            })?;
            if std::mem::replace(slot, true) {
                return Err(Error::internal(
                    "OpenRouter rerank returned a duplicate index",
                ));
            }
            ranked.push((result.index, finite_f32(result.relevance_score)?));
        }
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.truncate(self.top_n);
        Ok(ranked)
    }
}

fn encode_request(
    encoded: Encoded,
    extend: impl FnOnce(&mut Value),
) -> Result<super::remote::WireRequest> {
    let (parts, body) = encoded.request.into_parts();
    let Body::Bytes(bytes) = body else {
        return Err(Error::internal("Rig retrieval request must be JSON"));
    };
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|err| Error::internal_with_source("decode Rig request body", err))?;
    if !body.is_object() {
        return Err(Error::internal(
            "Rig retrieval request must be a JSON object",
        ));
    }
    extend(&mut body);
    let mut headers = Vec::with_capacity(parts.headers.len() + 2);
    for (name, value) in &parts.headers {
        let value = value
            .to_str()
            .map_err(|err| Error::internal_with_source("invalid Rig request header", err))?;
        headers.push((name.to_string(), value.to_owned()));
    }
    headers.push(("HTTP-Referer".into(), HTTP_REFERER.into()));
    headers.push(("X-Title".into(), X_TITLE.into()));
    Ok(super::remote::WireRequest {
        url: parts.uri.to_string(),
        headers,
        body: body.to_string(),
    })
}

fn send(transport: &dyn Transport, request: &super::remote::WireRequest) -> Result<String> {
    post_with_retry(transport, &request.url, &request.headers, &request.body)
        .map(|response| response.body)
}

/// Rig's decoder ignores indices, and defaults a missing index to zero.
/// Validate the distinction before projecting the ordered vector array.
fn ordered_embeddings(body: &str, expected: usize) -> Result<Vec<u8>> {
    let mut response: Value = serde_json::from_str(body).map_err(|err| {
        Error::internal_with_source("incompatible OpenRouter embeddings schema", err)
    })?;
    let data = response
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::internal("OpenRouter embedding response has no data array"))?;
    if data.len() != expected {
        return Err(Error::internal(format!(
            "OpenRouter returned {} vectors for {expected} inputs",
            data.len()
        )));
    }
    let indexed = data.iter().any(|datum| !datum["index"].is_null());
    let mut slots = vec![None; expected];
    for (position, datum) in std::mem::take(data).into_iter().enumerate() {
        let index = if datum["index"].is_null() {
            if indexed {
                return Err(Error::internal(
                    "OpenRouter mixed indexed and unindexed embeddings",
                ));
            }
            position
        } else {
            datum["index"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| Error::internal("OpenRouter returned an invalid embedding index"))?
        };
        let slot = slots.get_mut(index).ok_or_else(|| {
            Error::internal(format!(
                "OpenRouter returned index {index} for {expected} inputs"
            ))
        })?;
        if slot.is_some() {
            return Err(Error::internal(format!(
                "OpenRouter returned index {index} twice"
            )));
        }
        *slot = Some(serde_json::json!({"index": index, "embedding": datum["embedding"]}));
    }
    let data = slots
        .into_iter()
        .map(|slot| slot.ok_or_else(|| Error::internal("OpenRouter skipped an input index")))
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({"data": data}).to_string().into_bytes())
}

// Rig's numeric representation is f64; docsbase stores f32. The lossy cast is
// intentional, but overflow must fail before vectors enter the persistent cache.
#[allow(clippy::cast_possible_truncation)]
fn finite_f32(value: f64) -> Result<f32> {
    let narrowed = value as f32;
    if !value.is_finite() || !narrowed.is_finite() {
        return Err(Error::internal(
            "OpenRouter returned a value outside finite f32 range",
        ));
    }
    Ok(narrowed)
}
