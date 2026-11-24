//! Bounded regular-file reads. These observations are not cross-file snapshots.
use serde::Serialize;

use crate::config::schema::SourceId;
use crate::config::source::SourceLayer;
use crate::platform::{FileIdentity, basic_attributes, fs};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SourceState {
    Missing,
    Loaded {
        revision: String,
        precondition: String,
        version: u32,
    },
    Unavailable(SourceFailure),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFailure {
    Io,
    NonRegular,
    MultipleLinks,
    IdentityUnavailable,
    Changed,
    LimitExceeded,
    Invalid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceObservation {
    pub source: SourceId,
    pub state: SourceState,
}
pub(super) struct Captured {
    pub observation: SourceObservation,
    pub layer: Option<SourceLayer>,
}
pub(super) fn capture(source: SourceId, bytes_left: &mut usize) -> Captured {
    let result = read(&source, bytes_left);
    let (state, layer) = match result {
        Ok(None) => (SourceState::Missing, None),
        Ok(Some((layer, evidence))) => {
            let revision = revision(&layer.original);
            (
                SourceState::Loaded {
                    precondition: precondition(&evidence, &revision),
                    revision,
                    version: match layer.version() {
                        crate::config::source::ConfigVersion::V1 => 1,
                        crate::config::source::ConfigVersion::V2 => 2,
                    },
                },
                Some(layer),
            )
        }
        Err(reason) => (SourceState::Unavailable(reason), None),
    };
    Captured {
        observation: SourceObservation { source, state },
        layer,
    }
}
pub(super) fn revision(text: &str) -> String {
    format!("source-bytes-v1:{}", blake3::hash(text.as_bytes()).to_hex())
}
#[derive(PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
struct FileEvidence {
    identity: FileIdentity,
    attributes: basic_attributes::Encoded,
}

fn precondition(evidence: &FileEvidence, revision: &str) -> String {
    let mut hash = blake3::Hasher::new_derive_key("docsbase.observed-source.v2");
    hash.update(basic_attributes::PROFILE.as_bytes());
    hash.update(&evidence.identity.volume.to_le_bytes());
    hash.update(&evidence.identity.object.to_le_bytes());
    hash.update(evidence.attributes.as_flattened());
    hash.update(revision.as_bytes());
    format!("observed-source-v2:{}", hash.finalize().to_hex())
}

fn inspect(file: &std::fs::File) -> Result<(std::fs::Metadata, FileEvidence), SourceFailure> {
    let metadata = file.metadata().map_err(|_| SourceFailure::Io)?;
    if !fs::is_regular_nofollow(&metadata) {
        return Err(SourceFailure::NonRegular);
    }
    let (identity, links) = fs::file_identity_and_links(file, &metadata)
        .map_err(|_| SourceFailure::IdentityUnavailable)?;
    match links {
        1 => {
            let attributes = basic_attributes::encode(&metadata);
            Ok((
                metadata,
                FileEvidence {
                    identity,
                    attributes,
                },
            ))
        }
        0 => Err(SourceFailure::Changed),
        _ => Err(SourceFailure::MultipleLinks),
    }
}

fn read(
    source: &SourceId,
    bytes_left: &mut usize,
) -> Result<Option<(SourceLayer, FileEvidence)>, SourceFailure> {
    read_with(source, bytes_left, || {}, || {})
}
fn read_with(
    source: &SourceId,
    bytes_left: &mut usize,
    before_open: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<Option<(SourceLayer, FileEvidence)>, SourceFailure> {
    let (SourceId::Global(path) | SourceId::Project(path)) = source else {
        return Err(SourceFailure::Invalid);
    };
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SourceFailure::Io),
        Ok(metadata) if !fs::is_regular_nofollow(&metadata) => {
            return Err(SourceFailure::NonRegular);
        }
        Ok(_) => {}
    }
    let parent = path.parent().ok_or(SourceFailure::Invalid)?;
    let directory = std::fs::canonicalize(parent).map_err(|_| SourceFailure::Io)?;
    let name = path.file_name().ok_or(SourceFailure::Invalid)?;
    let resolved = directory.join(name);
    before_open();
    let file = fs::open_readonly_nofollow(&resolved).map_err(|_| SourceFailure::Io)?;
    let (before, evidence) = inspect(&file)?;
    let cap = crate::limits::MAX_STATE_BYTES.min(*bytes_left as u64);
    if before.len() > cap {
        return Err(SourceFailure::LimitExceeded);
    }
    let bytes = crate::config::capture::read_bounded(&file, cap).map_err(|error| match error {
        crate::config::capture::ReadFailure::Io(_) => SourceFailure::Io,
        crate::config::capture::ReadFailure::LimitExceeded
        | crate::config::capture::ReadFailure::BudgetOverflow => SourceFailure::LimitExceeded,
    })?;
    *bytes_left -= bytes.len();
    after_read();
    // Pin the first object until comparison so its ID cannot be recycled.
    let current = fs::open_readonly_nofollow(&resolved).map_err(|_| SourceFailure::Changed)?;
    let (after, current_evidence) = inspect(&current).map_err(|failure| match failure {
        SourceFailure::IdentityUnavailable => failure,
        _ => SourceFailure::Changed,
    })?;
    if evidence != current_evidence
        || before.len() != bytes.len() as u64
        || !same_metadata(&before, &after)
        || std::fs::canonicalize(parent).map_err(|_| SourceFailure::Changed)? != directory
    {
        return Err(SourceFailure::Changed);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| SourceFailure::Invalid)?;
    SourceLayer::parse(source.clone(), text)
        .map(|layer| Some((layer, evidence)))
        .map_err(|_| SourceFailure::Invalid)
}
fn same_metadata(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    // A conservative race check, not proof against ABA/in-place concurrent writes.
    a.len() == b.len()
        && a.modified()
            .ok()
            .zip(b.modified().ok())
            .is_some_and(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacing_file_with_matching_length_and_mtime_during_read_is_detected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let replacement = root.path().join("replacement");
        std::fs::write(&path, "auto_index=true").unwrap();
        std::fs::write(&replacement, "auto_index=true").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&replacement).unwrap().modified().unwrap(),
            modified
        );
        let result = read_with(
            &SourceId::Global(path.clone()),
            &mut 1024,
            || {},
            || {
                std::fs::rename(&path, root.path().join("old")).unwrap();
                std::fs::rename(&replacement, &path).unwrap();
            },
        );
        assert_eq!(result.unwrap_err(), SourceFailure::Changed);
    }

    #[test]
    fn basic_attributes_changing_during_read_block_the_observation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, "auto_index=true").unwrap();
        let original = std::fs::metadata(&path).unwrap().permissions();
        let result = read_with(
            &SourceId::Global(path.clone()),
            &mut 1024,
            || {},
            || {
                let mut changed = original.clone();
                changed.set_readonly(true);
                std::fs::set_permissions(&path, changed).unwrap();
            },
        );
        std::fs::set_permissions(&path, original).unwrap();
        assert_eq!(result.unwrap_err(), SourceFailure::Changed);
    }

    #[test]
    fn mutation_during_read_is_not_published_as_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let source = SourceId::Global(path.clone());
        let result = read_with(
            &source,
            &mut 1024,
            || {},
            || {
                std::fs::write(path, "hybrid=true").unwrap();
            },
        );
        assert_eq!(result.unwrap_err(), SourceFailure::Changed);
    }
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    fn replaced_symlink_is_rejected(link: impl FnOnce(&Path, &Path)) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let target = root.path().join("target");
        std::fs::write(&path, "").unwrap();
        std::fs::write(&target, "auto_index=true").unwrap();
        let result = read_with(
            &SourceId::Global(path.clone()),
            &mut 1024,
            || {
                std::fs::remove_file(&path).unwrap();
                link(&target, &path);
            },
            || {},
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "auto_index=true");
    }
    use std::path::Path;
    #[cfg(unix)]
    #[test]
    fn unix_symlink_replacing_regular_file_before_open_is_rejected() {
        replaced_symlink_is_rejected(|target, path| {
            std::os::unix::fs::symlink(target, path).unwrap();
        });
    }
    #[cfg(windows)]
    #[test]
    fn windows_symlink_replacing_regular_file_before_open_is_rejected() {
        replaced_symlink_is_rejected(|target, path| {
            std::os::windows::fs::symlink_file(target, path).unwrap();
        });
    }
}
