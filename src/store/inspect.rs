//! No-write observation of metadata schema identity, not a SQL row snapshot.

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use crate::config::schema::{
    ArtifactFact, ArtifactFacts, ArtifactId, ArtifactState, Capabilities, Resolved, SettingsSpec,
};
use crate::error::Result;

use super::settings::{DbSettings, DbSpec};

pub(super) fn schema(cache: &Path, settings: &Resolved<DbSettings>) -> Result<ArtifactFacts> {
    schema_with(cache, settings, || {})
}

fn schema_with(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    before_recheck: impl FnOnce(),
) -> Result<ArtifactFacts> {
    DbSpec::validate(settings.settings(), &Capabilities::current())?;
    let path = cache.join(super::DB_FILE);
    let sidecars = sidecars_present(cache);
    if matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        before_recheck();
        let still_missing = matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        let state = match (still_missing, sidecars, sidecars_present(cache)) {
            (true, Some(false), Some(false)) => ArtifactState::Missing,
            (true, _, Some(true)) => ArtifactState::NeedsRecovery,
            _ => ArtifactState::Unknown,
        };
        return Ok(facts(state, None));
    }
    if sidecars != Some(false) {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    let Some(first) = observe(&path) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    before_recheck();
    let Some(second) = observe(&path) else {
        return Ok(facts(ArtifactState::Unknown, None));
    };
    if first != second || sidecars_present(cache) != Some(false) {
        return Ok(facts(ArtifactState::Unknown, None));
    }
    // SQLite file-format offsets, not runtime policy: schema cookie at40,
    // user_version at60. No table/row/query-readiness claim is made here.
    let cookie = u32::from_be_bytes([
        first.header[40],
        first.header[41],
        first.header[42],
        first.header[43],
    ]);
    let version = u32::from_be_bytes([
        first.header[60],
        first.header[61],
        first.header[62],
        first.header[63],
    ]);
    Ok(facts(
        ArtifactState::Ready,
        Some(format!("sqlite-schema-v1:{version}:{cookie}")),
    ))
}

fn sidecars_present(cache: &Path) -> Option<bool> {
    for suffix in ["-wal", "-shm", "-journal"] {
        match std::fs::symlink_metadata(cache.join(format!("{}{suffix}", super::DB_FILE))) {
            Ok(_) => return Some(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(false)
}

#[derive(PartialEq, Eq)]
struct Observation {
    // Fixed SQLite database-file header length (file format, not a budget).
    header: [u8; 100],
    length: u64,
    modified: SystemTime,
}

fn observe(path: &Path) -> Option<Observation> {
    let mut file = crate::platform::fs::open_readonly_nofollow(path).ok()?;
    let before = file.metadata().ok()?;
    if !before.is_file() || before.file_type().is_symlink() {
        return None;
    }
    let mut header = [0; 100];
    file.read_exact(&mut header).ok()?;
    let after = file.metadata().ok()?;
    if before.len() != after.len() || before.modified().ok()? != after.modified().ok()? {
        return None;
    }
    let encoded_size = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded_size == 1 {
        65_536
    } else {
        u64::from(encoded_size)
    };
    if &header[..16] != b"SQLite format 3\0"
        || !(512..=65_536).contains(&page_size)
        || !page_size.is_power_of_two()
        || before.len() < page_size
        || before.len() % page_size != 0
        || !matches!(header[18], 1 | 2)
        || !matches!(header[19], 1 | 2)
        || header[21..24] != [64, 32, 32]
    {
        return None;
    }
    Some(Observation {
        header,
        length: after.len(),
        modified: after.modified().ok()?,
    })
}

fn facts(state: ArtifactState, identity: Option<String>) -> ArtifactFacts {
    ArtifactFacts {
        artifacts: vec![ArtifactFact {
            id: ArtifactId {
                component: "metadata_db_schema".to_owned(),
                project_id: None,
            },
            state,
            identity,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_db() -> (tempfile::TempDir, Resolved<DbSettings>) {
        let cache = tempfile::tempdir().expect("cache");
        let settings = crate::store::settings::standalone().expect("settings");
        drop(crate::store::Db::open_with_settings(cache.path(), &settings).expect("db"));
        assert_eq!(
            schema(cache.path(), &settings).expect("stable").artifacts[0].state,
            ArtifactState::Ready
        );
        (cache, settings)
    }

    #[test]
    fn sidecar_appearing_during_inspection_is_unknown() {
        let (cache, settings) = closed_db();
        let wal = cache.path().join(format!("{}-wal", super::super::DB_FILE));
        let facts = schema_with(cache.path(), &settings, || {
            std::fs::write(&wal, b"new recovery data").expect("concurrent WAL");
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
        assert_eq!(std::fs::read(wal).expect("retained"), b"new recovery data");
    }

    #[test]
    fn changed_header_is_unknown() {
        let (cache, settings) = closed_db();
        let facts = schema_with(cache.path(), &settings, || {
            let conn = rusqlite::Connection::open(cache.path().join(super::super::DB_FILE))
                .expect("concurrent writer");
            conn.pragma_update(
                None,
                "user_version",
                crate::store::migrations::SCHEMA_VERSION + 1,
            )
            .expect("changed schema");
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }

    #[test]
    fn db_appearing_during_missing_inspection_is_unknown() {
        let cache = tempfile::tempdir().expect("cache");
        let settings = crate::store::settings::standalone().expect("settings");
        let facts = schema_with(cache.path(), &settings, || {
            drop(crate::store::Db::open_with_settings(cache.path(), &settings).expect("new db"));
        })
        .expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn schema_inspection_does_not_follow_a_symlink() {
        let (target, settings) = closed_db();
        let cache = tempfile::tempdir().expect("cache");
        std::os::unix::fs::symlink(
            target.path().join(super::super::DB_FILE),
            cache.path().join(super::super::DB_FILE),
        )
        .expect("symlink");
        let facts = schema(cache.path(), &settings).expect("facts");
        assert_eq!(facts.artifacts[0].state, ArtifactState::Unknown);
        assert!(facts.artifacts[0].identity.is_none());
    }
}
