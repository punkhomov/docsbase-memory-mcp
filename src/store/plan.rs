//! Pure schema-owner assessment of the bounded no-write inspector's output.
use crate::config::change::{
    Action, Assessment, Blocker, BlockerCode, Effect, ExpectedState, Grant, Observation, Reason,
    ResourceId, Target, invalid,
};
use crate::config::schema::{ArtifactFacts, ArtifactId, ArtifactState};
use crate::error::Result;

pub(super) fn assess_schema(facts: &ArtifactFacts) -> Result<Assessment> {
    let id = ArtifactId {
        component: "metadata_db_schema".into(),
        project_id: None,
    };
    let resource = ResourceId::Artifact(id.clone());
    let mut matching = facts.artifacts.iter().filter(|fact| fact.id == id);
    let first = matching.next();
    if matching.any(|fact| Some(fact) != first) {
        return Err(invalid("conflicting metadata schema observations"));
    }
    let mut assessment = Assessment::default();
    let (state, action, blocker) = match first {
        None => (
            ExpectedState::Unknown,
            None,
            Some(BlockerCode::ObservationUnavailable),
        ),
        Some(fact) => match fact.state {
            ArtifactState::Missing => (
                ExpectedState::Missing,
                Some((Effect::InitializeStore, Reason::MissingArtifact)),
                None,
            ),
            ArtifactState::NeedsRecovery => (
                ExpectedState::NeedsRecovery,
                Some((Effect::RecoverStore, Reason::RecoveryRequired)),
                None,
            ),
            ArtifactState::Unknown => (
                ExpectedState::Unknown,
                None,
                Some(BlockerCode::ObservationUnavailable),
            ),
            ArtifactState::Ready => match fact.identity.as_deref().and_then(schema_version) {
                Some(version) if version <= super::migrations::SCHEMA_VERSION => (
                    ExpectedState::KnownIdentity(
                        fact.identity
                            .clone()
                            .ok_or_else(|| invalid("missing schema identity"))?,
                    ),
                    (version < super::migrations::SCHEMA_VERSION)
                        .then_some((Effect::MigrateSchema, Reason::SchemaVersionChanged)),
                    None,
                ),
                _ => (
                    ExpectedState::Unknown,
                    None,
                    Some(BlockerCode::UnsupportedIdentity),
                ),
            },
        },
    };
    assessment.expected.push(Observation {
        resource: resource.clone(),
        state,
    });
    if let Some(code) = blocker {
        assessment.blockers.push(Blocker {
            resource: Some(resource.clone()),
            code,
        });
    }
    if let Some((effect, reason)) = action {
        assessment.actions.push(Action {
            resource,
            effect,
            // Recovery cannot promise a resulting schema identity.
            target: (effect != Effect::RecoverStore).then_some(Target::MetadataSchemaVersion(
                super::migrations::SCHEMA_VERSION,
            )),
            reasons: vec![reason],
            dependencies: vec![],
            required_grants: if effect == Effect::InitializeStore {
                vec![]
            } else {
                vec![Grant::Destructive]
            },
        });
    }
    Ok(assessment)
}
fn schema_version(identity: &str) -> Option<u32> {
    let mut fields = identity.strip_prefix("sqlite-schema-v1:")?.split(':');
    let version_text = fields.next()?;
    let cookie_text = fields.next()?;
    let version: u32 = version_text.parse().ok()?;
    let cookie: u32 = cookie_text.parse().ok()?;
    if fields.next().is_some()
        || version.to_string() != version_text
        || cookie.to_string() != cookie_text
    {
        return None;
    }
    Some(version)
}
