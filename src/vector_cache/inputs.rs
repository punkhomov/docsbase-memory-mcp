//! Artifact assessment for the vector cache. Recipe evidence is not coverage.
use crate::config::change::{
    Action, Assessment, Effect, ExpectedState, Grant, Observation, Reason, ResourceId, Target,
    policy_identity,
};
use crate::config::schema::ArtifactId;
use crate::error::Result;

use super::EmbeddingRecipe;

/// Inputs of one document-vector family, independent of the lexical index.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DocumentVectorInputs {
    pub(crate) chunks: [u8; 32],
    pub(crate) embedding: EmbeddingRecipe,
}

pub(crate) fn assess_replacement(
    project_id: i64,
    before: Option<DocumentVectorInputs>,
    after: DocumentVectorInputs,
    remote: bool,
) -> Result<Assessment> {
    let artifact = |name: &str| {
        ResourceId::Artifact(ArtifactId {
            component: name.into(),
            project_id: Some(project_id),
        })
    };
    let identity = |inputs| policy_identity("document-vector-inputs-v2", inputs);
    let mut assessment = Assessment {
        expected: vec![Observation {
            resource: artifact("document_vector_recipe"),
            state: before
                .as_ref()
                .map(identity)
                .transpose()?
                .map_or(ExpectedState::Unknown, ExpectedState::KnownIdentity),
        }],
        ..Assessment::default()
    };
    if before == Some(after) {
        return Ok(assessment);
    }
    let vectors = artifact("document_vectors");
    // A generation seal proves the previous recipe, not which vectors are
    // still resident in the independently evictable cache.
    assessment.expected.push(Observation {
        resource: vectors.clone(),
        state: ExpectedState::Unknown,
    });
    assessment.actions.push(Action {
        resource: vectors,
        effect: Effect::Rebuild,
        target: Some(Target::Identity(identity(&after)?)),
        reasons: vec![Reason::SettingsChanged],
        dependencies: Vec::new(),
        required_grants: if remote {
            vec![Grant::Remote]
        } else {
            Vec::new()
        },
    });
    Ok(assessment)
}
