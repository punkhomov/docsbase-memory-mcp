//! References to chunks in one published catalog revision.

use serde_json::Value;

use crate::error::{Error, Result};

const PREFIX: &str = "v1.";
const BINDING_BYTES: usize = 32 + 3 * 16;
const CHUNK_BYTES: usize = 16;
// Explained results can retain an original hit and its clone at the same time.
pub(super) const RESPONSE_BYTES: usize = 2
    * (PREFIX.len()
        + BINDING_BYTES
        + CHUNK_BYTES
        + size_of::<(String, Value)>()
        + "chunk_ref".len());

#[derive(Clone, Copy)]
pub(crate) struct ChunkBinding {
    pub(crate) epoch: [u8; 16],
    pub(crate) project_id: i64,
    pub(crate) generation_id: i64,
    pub(crate) content_revision: i64,
}

impl ChunkBinding {
    pub(crate) fn reference(self, chunk: u64) -> String {
        let epoch = u128::from_be_bytes(self.epoch);
        let Self {
            project_id,
            generation_id,
            content_revision,
            ..
        } = self;
        format!(
            "{PREFIX}{epoch:032x}{project_id:016x}{generation_id:016x}{content_revision:016x}{chunk:016x}"
        )
    }

    pub(crate) fn resolve(self, args: &Value) -> Result<u64> {
        let invalid = || Error::Protocol {
            message: "read_neighbors_at requires a valid chunk_ref".into(),
        };
        let body = args
            .get("chunk_ref")
            .and_then(Value::as_str)
            .and_then(|token| token.strip_prefix(PREFIX))
            .filter(|body| {
                body.len() == BINDING_BYTES + CHUNK_BYTES
                    && body.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or_else(invalid)?;
        let chunk = u64::from_str_radix(&body[BINDING_BYTES..], 16).map_err(|_| invalid())?;
        if self.reference(chunk)[PREFIX.len()..] != *body {
            return Err(Error::Project {
                message: "chunk_ref does not match the current project generation".into(),
                instruction: Some("run search_docs again".into()),
            });
        }
        Ok(chunk)
    }
}

pub(crate) fn unavailable() -> Error {
    Error::Admission {
        message: "chunk references require a published v5 catalog".into(),
    }
}

pub(crate) fn requested(args: &Value) -> Result<bool> {
    match args.get("chunk_ref") {
        None | Some(Value::Null) => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| Error::Protocol {
            message: "search_docs: chunk_ref must be a boolean".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding() -> ChunkBinding {
        ChunkBinding {
            epoch: [21; 16],
            project_id: 3,
            generation_id: 5,
            content_revision: 8,
        }
    }

    #[test]
    fn exact_chunk_id_requires_matching_epoch_project_generation_and_revision() {
        let bound = binding();
        let argument = json!({"chunk_ref": bound.reference(u64::MAX)});
        assert_eq!(bound.resolve(&argument).unwrap(), u64::MAX);
        for changed in [
            ChunkBinding {
                epoch: [22; 16],
                ..bound
            },
            ChunkBinding {
                project_id: 2,
                ..bound
            },
            ChunkBinding {
                generation_id: 6,
                ..bound
            },
            ChunkBinding {
                content_revision: 9,
                ..bound
            },
        ] {
            assert!(matches!(
                changed.resolve(&argument),
                Err(Error::Project {
                    instruction: Some(_),
                    ..
                })
            ));
        }
    }

    #[test]
    fn malformed_chunk_references_are_protocol_errors() {
        let bound = binding();
        let token = bound.reference(42);
        for argument in [
            json!({}),
            json!({"chunk_ref": 42}),
            json!({"chunk_ref": "v1."}),
            json!({"chunk_ref": format!("{token}\n")}),
            json!({"chunk_ref": token.replacen("v1.", "v2.", 1)}),
            json!({"chunk_ref": format!("{}z", &token[..token.len()-1])}),
        ] {
            assert!(matches!(
                bound.resolve(&argument),
                Err(Error::Protocol { .. })
            ));
        }
    }
}
