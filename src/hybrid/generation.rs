//! Rig completion capability on the existing provider execution owner.
use super::remote::{RemoteCall, StageFuture, WireRequest};
use crate::error::{Error, Result};
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::providers::openai::wire::{Chat, OPENROUTER, OpenAIConfig};
use rig_core::wire::{Mode, Operation, Wire, WireFrame, decode_reply};
use std::sync::Arc;

/// A model capable of tool-calling completion; no private runtime or retries.
pub trait Generator: Send + Sync {
    /// Generate one complete assistant turn under the caller's operation.
    fn complete<'a>(
        &'a self,
        request: CompletionRequest,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, CompletionResponse>;
}

pub(crate) struct OpenRouterGenerator {
    pub(crate) client: Arc<super::openrouter::OpenRouterClient>,
    pub(crate) model: String,
}
impl Generator for OpenRouterGenerator {
    fn complete<'a>(
        &'a self,
        request: CompletionRequest,
        call: &'a RemoteCall,
    ) -> StageFuture<'a, CompletionResponse> {
        Box::pin(self.client.complete(self.model.clone(), request, call))
    }
}
pub(super) struct Prepared {
    pub(super) request: WireRequest,
    codec: Chat,
    input: CompletionRequest,
}
pub(super) fn prepare(key: &str, model: &str, input: CompletionRequest) -> Result<Prepared> {
    let codec = OpenAIConfig::with_key(&OPENROUTER, key.to_owned()).chat(model);
    let input = rig_core::operation::Completion::prepare(input, &codec.describe())
        .map_err(|_| Error::internal("Rig rejected generation history or options"))?;
    let encoded = codec
        .encode(input.clone(), Mode::Unary)
        .map_err(|_| Error::internal("Rig could not encode generation request"))?;
    let request = super::rig_codec::encode_request(encoded, |_| {})?;
    Ok(Prepared {
        request,
        codec,
        input,
    })
}
impl Prepared {
    pub(super) fn decode(self, body: &str) -> Result<CompletionResponse> {
        decode_reply(
            &self.codec,
            &self.input,
            Mode::Unary,
            [WireFrame::Bytes(body.as_bytes().to_vec())],
        )
        .map_err(|_| Error::internal("invalid generation response from provider"))
    }
}
