//! Machine-local `FastEmbed` inference settings and their configuration owner.

use std::num::NonZeroUsize;

use crate::config::schema::{FieldSpec, Provenance, Scope, SourceId, Unit};
use crate::config::view::{ViewBuilder, capability};
use crate::error::{Error, Result};

use super::config::{PROVIDER_FASTEMBED, ProviderRecord, ProvidersCfg, type_error};

/// Default inference batch size.
pub const DEFAULT_BATCH_SIZE: usize = 64;
/// Default and maximum input length of the pinned embedding models.
pub const DEFAULT_MAX_LENGTH: usize = 512;
// Accommodates both special tokens and the longest registry role prefix,
// with room for document/query text after the prefix.
const MIN_EMBEDDING_LENGTH: i64 = 16;
pub(crate) const INFERENCE_KEYS: [&str; 3] =
    ["batch_size", "intra_threads", "embedding_max_length"];
pub(crate) const INFERENCE_FIELDS: &[FieldSpec] = &[
    FieldSpec::new(
        "providers.fastembed.batch_size",
        Scope::Machine,
        Unit::Count,
    ),
    FieldSpec::new(
        "providers.fastembed.intra_threads",
        Scope::Machine,
        Unit::Count,
    ),
    FieldSpec::new(
        "providers.fastembed.embedding_max_length",
        Scope::Machine,
        Unit::Count,
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InvalidCount {
    key: &'static str,
    description: &'static str,
}

impl From<InvalidCount> for Error {
    fn from(error: InvalidCount) -> Self {
        type_error("providers.fastembed", error.key, error.description)
    }
}

pub(super) struct InferencePatch {
    batch_size: Option<usize>,
    intra_threads: Option<usize>,
    embedding_max_length: Option<usize>,
}

impl InferencePatch {
    pub(super) fn from_record(record: &ProviderRecord) -> std::result::Result<Self, InvalidCount> {
        Ok(Self {
            batch_size: bounded(
                record,
                INFERENCE_KEYS[0],
                1..=i64::MAX,
                "a positive integer that fits usize",
            )?,
            // ORT's session builder forwards a signed native int.
            intra_threads: bounded(
                record,
                INFERENCE_KEYS[1],
                1..=i64::from(i32::MAX),
                "a positive 32-bit signed integer",
            )?,
            embedding_max_length: bounded(
                record,
                INFERENCE_KEYS[2],
                MIN_EMBEDDING_LENGTH..=512,
                "an integer between 16 and 512 tokens",
            )?,
        })
    }

    pub(super) fn resolve(self, defaults: InferenceSettings) -> InferenceSettings {
        InferenceSettings {
            batch_size: self.batch_size.unwrap_or(defaults.batch_size),
            intra_threads: self.intra_threads.unwrap_or(defaults.intra_threads),
            embedding_max_length: self
                .embedding_max_length
                .unwrap_or(defaults.embedding_max_length),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InferenceSettings {
    pub(super) batch_size: usize,
    pub(super) intra_threads: usize,
    pub(super) embedding_max_length: usize,
}

impl InferenceSettings {
    pub(super) fn defaults(cpu_count: NonZeroUsize) -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            intra_threads: cpu_count.get().min(4),
            embedding_max_length: DEFAULT_MAX_LENGTH,
        }
    }
}

pub(crate) fn set_origins(
    providers: &ProvidersCfg,
    origins: &mut Provenance,
    source: &SourceId,
) -> Result<()> {
    let record = providers.get(PROVIDER_FASTEMBED);
    for (field, parameter) in INFERENCE_FIELDS.iter().zip(INFERENCE_KEYS) {
        let origin = if record.is_some_and(|record| record.params.contains_key(parameter)) {
            source
        } else {
            &SourceId::Defaults
        };
        origins.set(field.key, origin)?;
    }
    Ok(())
}

pub(crate) fn view_fields(providers: &ProvidersCfg, view: &mut ViewBuilder<'_>) -> Result<()> {
    let inference = providers.fastembed_inference()?;
    let provider = Some(capability(PROVIDER_FASTEMBED)?);
    view.push(
        INFERENCE_FIELDS[0].key,
        inference.batch_size.into(),
        provider.clone(),
    )?;
    view.push(
        INFERENCE_FIELDS[1].key,
        inference.intra_threads.into(),
        provider.clone(),
    )?;
    view.push(
        INFERENCE_FIELDS[2].key,
        inference.embedding_max_length.into(),
        provider,
    )
}

fn bounded(
    record: &ProviderRecord,
    key: &'static str,
    range: std::ops::RangeInclusive<i64>,
    description: &'static str,
) -> std::result::Result<Option<usize>, InvalidCount> {
    record
        .params
        .get(key)
        .map(|value| {
            value
                .as_integer()
                .filter(|value| range.contains(value))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(InvalidCount { key, description })
        })
        .transpose()
}
