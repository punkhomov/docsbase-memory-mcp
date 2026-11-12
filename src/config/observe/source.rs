//! Bounded regular-file reads. These observations are not cross-file snapshots.
use std::io::Read;

use serde::Serialize;

use crate::config::schema::SourceId;
use crate::config::source::SourceLayer;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SourceState {
    Missing,
    Loaded { revision: String, version: u32 },
    Unavailable(SourceFailure),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFailure {
    Io,
    NonRegular,
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
        Ok(Some(layer)) => (
            SourceState::Loaded {
                revision: revision(&layer.original),
                version: match layer.version() {
                    crate::config::source::ConfigVersion::V1 => 1,
                    crate::config::source::ConfigVersion::V2 => 2,
                },
            },
            Some(layer),
        ),
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
fn read(source: &SourceId, bytes_left: &mut usize) -> Result<Option<SourceLayer>, SourceFailure> {
    read_with(source, bytes_left, || {}, || {})
}
fn read_with(
    source: &SourceId,
    bytes_left: &mut usize,
    before_open: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<Option<SourceLayer>, SourceFailure> {
    let (SourceId::Global(path) | SourceId::Project(path)) = source else {
        return Err(SourceFailure::Invalid);
    };
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SourceFailure::Io),
        Ok(metadata) if !crate::platform::fs::is_regular_nofollow(&metadata) => {
            return Err(SourceFailure::NonRegular);
        }
        Ok(_) => {}
    }
    let parent = path.parent().ok_or(SourceFailure::Invalid)?;
    let directory = std::fs::canonicalize(parent).map_err(|_| SourceFailure::Io)?;
    let name = path.file_name().ok_or(SourceFailure::Invalid)?;
    let resolved = directory.join(name);
    before_open();
    let file =
        crate::platform::fs::open_readonly_nofollow(&resolved).map_err(|_| SourceFailure::Io)?;
    let before = file.metadata().map_err(|_| SourceFailure::Io)?;
    if !crate::platform::fs::is_regular_nofollow(&before) {
        return Err(SourceFailure::NonRegular);
    }
    let cap = crate::limits::MAX_STATE_BYTES.min(*bytes_left as u64);
    if before.len() > cap {
        return Err(SourceFailure::LimitExceeded);
    }
    let mut bytes = Vec::new();
    (&file)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SourceFailure::Io)?;
    if bytes.len() as u64 > cap {
        return Err(SourceFailure::LimitExceeded);
    }
    *bytes_left -= bytes.len();
    after_read();
    let after = file.metadata().map_err(|_| SourceFailure::Changed)?;
    let current = std::fs::symlink_metadata(&resolved).map_err(|_| SourceFailure::Changed)?;
    if !crate::platform::fs::is_regular_nofollow(&current)
        || before.len() != bytes.len() as u64
        || !same_metadata(&before, &after)
        || !same_metadata(&after, &current)
        || std::fs::canonicalize(parent).map_err(|_| SourceFailure::Changed)? != directory
    {
        return Err(SourceFailure::Changed);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| SourceFailure::Invalid)?;
    SourceLayer::parse(source.clone(), text)
        .map(Some)
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
