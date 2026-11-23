//! Bounded, explicitly requested documentation answers (ADR-ASK1).
use crate::error::{Error, Result};
use serde::Deserialize;

#[cfg(feature = "hybrid-remote")]
pub mod runner;

/// Protocol limits owned by the first documentation harness.
pub struct AskLimits;
impl AskLimits {
    /// Maximum natural-language question bytes.
    pub const QUESTION_BYTES: usize = 8192;
    /// Maximum provider/model address bytes.
    pub const MODEL_BYTES: usize = 512;
    /// Search hits per model call.
    pub const SEARCH_HITS: usize = 5;
    /// Neighboring chunks on each side of a hit.
    pub const NEIGHBORS: u64 = 1;
    /// Whole operation, including queueing and document tools.
    pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    /// Bound model turns, including the final answer.
    pub const TURNS: usize = 6;
    /// Bound executed/invalid tool calls across all turns.
    pub const CALLS: usize = 16;
    /// Accumulated serialized conversation cap.
    pub const CONTEXT: usize = 128 * 1024;
    /// Maximum one tool's encoded result.
    pub const TOOL_BYTES: usize = 32 * 1024;
    /// Maximum one provider reply, including reasoning/native data.
    pub const RESPONSE_BYTES: u64 = 256 * 1024;
    /// Output cap includes provider reasoning.
    pub const OUTPUT_TOKENS: u64 = 4096;
    /// Conservative retained allowance for owned copies/codec/read phases.
    pub const RETAINED_BYTES: u64 = 8 * 1024 * 1024;
}

/// Explicit opt-in: a model address is required; no default paid model.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskRequest {
    /// Natural-language question.
    pub question: String,
    /// Existing `<provider>/<model>` address.
    pub model: String,
    /// Optional lower turn cap.
    pub max_turns: Option<usize>,
}
impl AskRequest {
    /// Validate before accessing documents or contacting a provider.
    /// # Errors
    /// Invalid shape, model address or request limits.
    pub fn parse(value: serde_json::Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value).map_err(|_| Error::Admission {
            message: "ask_docs requires question, model and optional max_turns".into(),
        })?;
        request.validate()?;
        Ok(request)
    }

    /// Validate explicit construction and deserialization at every execution boundary.
    /// # Errors
    /// Malformed question/model or a turn cap outside the protocol range.
    pub fn validate(&self) -> Result<()> {
        if self.question.trim().is_empty()
            || self.question.len() > AskLimits::QUESTION_BYTES
            || self.model.len() > AskLimits::MODEL_BYTES
            || !self
                .max_turns
                .is_none_or(|n| (1..=AskLimits::TURNS).contains(&n))
        {
            return Err(Error::Admission { message: "ask_docs: question must be 1..=8192 bytes, model <=512 bytes and max_turns 1..=6".into() });
        }
        crate::hybrid::ModelAddress::parse(&self.model)?;
        Ok(())
    }
}
