//! Conformance tests for the private control format, using disposable databases.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use rusqlite::types::Null;
use rusqlite::{Connection, named_params, params};
use tempfile::tempdir;

const BASE: &str = include_str!("../../docs/specs/component-configuration/control-state-v1.sql");
const STAGES: &str = include_str!("../../docs/specs/component-configuration/control-stages-v2.sql");
const GLOBAL: &str = "config.toml";
const OLD: &[u8] = b"original";
const NEW: &[u8] = b"proposed";

fn open(path: &Path) -> Connection {
    let db = Connection::open(path.join("control.db")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA synchronous=EXTRA; PRAGMA journal_mode=DELETE;").unwrap();
    db
}

fn unsealed(path: &Path, extension: &str, before: Option<&[u8]>) -> Connection {
    let mut db = open(path);
    let tx = db.transaction().unwrap();
    tx.execute_batch(BASE).unwrap();
    tx.execute_batch(extension).unwrap();
    tx.execute(
        "INSERT INTO namespace VALUES(1, zeroblob(32), '/cache','/config','/data', x'01')",
        [],
    )
    .unwrap();
    let target = path.join(GLOBAL);
    let old_len = before.map_or(0, <[u8]>::len);
    tx.execute(
        "INSERT INTO operation VALUES (1, zeroblob(32), 'preparing', 0, 'plan', 'observation',
         'consent', x'02', 1, ?1, ?2, 10, 2048, 0)",
        params![
            i64::try_from(old_len).unwrap(),
            i64::try_from(NEW.len()).unwrap()
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO source VALUES (0, zeroblob(32), 'global', NULL, ?1, x'03',
         ?2, ?3, ?4, 1, ?5, ?6, 'pending')",
        params![
            target.to_str().unwrap(),
            before.is_some(),
            before,
            before.map(|data| *blake3::hash(data).as_bytes()),
            NEW,
            blake3::hash(NEW).as_bytes()
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    if let Some(data) = before {
        fs::write(target, data).unwrap();
    }
    db
}

fn fixture(path: &Path) -> Connection {
    unsealed(path, STAGES, Some(OLD))
}

fn object(file: &File) -> Vec<u8> {
    let (id, links) =
        crate::platform::fs::file_identity_and_links(file, &file.metadata().unwrap()).unwrap();
    assert_eq!(links, 1);
    [id.volume.to_le_bytes().as_slice(), &id.object.to_le_bytes()].concat()
}

#[cfg(unix)]
fn attributes(file: &File) -> (&'static str, Vec<u8>) {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().unwrap();
    (
        "unix-stat-v1",
        [m.uid(), m.gid(), m.mode() & 0o7777]
            .iter()
            .flat_map(|n| n.to_le_bytes())
            .collect(),
    )
}

#[cfg(windows)]
fn attributes(file: &File) -> (&'static str, Vec<u8>) {
    use std::os::windows::fs::MetadataExt;
    (
        "windows-attributes-v1",
        file.metadata()
            .unwrap()
            .file_attributes()
            .to_le_bytes()
            .to_vec(),
    )
}

fn record_images(db: &Connection, ordinal: i64, path: &Path) {
    let old = File::open(path).unwrap();
    let (profile, attrs) = attributes(&old);
    db.execute(
        "INSERT INTO source_native VALUES (?1,?2,?3,?4,?4)",
        params![ordinal, profile, object(&old), attrs],
    )
    .unwrap();
}

const SEAL: &str = "UPDATE operation SET sealed=1";
const APPLY: &str = "UPDATE operation SET state='applying'";
const INTENT: &str = "INSERT INTO stage_intent(ordinal,direction) VALUES(0,'apply')";

#[test]
fn seal_requires_native_images_and_outstanding_intent_blocks_transitions() {
    let root = tempdir().unwrap();
    let db = fixture(root.path());
    assert!(
        db.execute(SEAL, []).is_err(),
        "byte-only images cannot seal a v2 journal"
    );
    record_images(&db, 0, &root.path().join(GLOBAL));
    db.execute(SEAL, []).unwrap();
    assert!(db.execute(INTENT, []).is_err());
    db.execute(APPLY, []).unwrap();
    db.execute(INTENT, []).unwrap();
    for sql in [
        "INSERT OR REPLACE INTO stage_intent(ordinal,direction) VALUES(0,'apply')",
        "UPDATE source_native SET after_attributes=before_attributes",
        "UPDATE operation SET state='restoring'",
        "UPDATE source SET progress='new_verified'",
        "UPDATE operation SET state='strict'",
        "UPDATE stage_intent SET object_id=zeroblob(24)",
    ] {
        assert!(db.execute(sql, []).is_err(), "unexpected success: {sql}");
    }
    let id = object(&File::create_new(root.path().join("fixture-stage")).unwrap());
    db.execute("UPDATE stage_intent SET object_id=?1", [&id])
        .unwrap();
    assert!(
        db.execute("UPDATE stage_intent SET object_id=?1", [&id])
            .is_err()
    );
    assert!(
        db.execute("UPDATE stage_intent SET object_id=NULL", [])
            .is_err()
    );
    db.execute("UPDATE stage_intent SET published=1", [])
        .unwrap();
    drop(db);
    let db = open(root.path());
    let progress: String = db
        .query_row("SELECT progress FROM source WHERE ordinal=0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(progress, "new_verified");
    assert!(db.execute("DELETE FROM stage_intent", []).is_err());
    db.execute("UPDATE operation SET state='restoring'", [])
        .unwrap();
    db.execute(
        "INSERT INTO stage_intent(ordinal,direction) VALUES(0,'restore')",
        [],
    )
    .unwrap();
    let (previous, digest): (Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT previous_object, expected_hash FROM outstanding_stage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(previous, id);
    assert_eq!(digest, blake3::hash(OLD).as_bytes());
    let restore = object(&File::create_new(root.path().join("restore-stage")).unwrap());
    db.execute(
        "UPDATE stage_intent SET object_id=?1 WHERE direction='restore'",
        [restore],
    )
    .unwrap();
    db.execute(
        "UPDATE stage_intent SET published=1 WHERE direction='restore'",
        [],
    )
    .unwrap();
    db.execute("UPDATE operation SET state='restored_legacy'", [])
        .unwrap();
}

#[test]
fn populated_v1_is_preserved_and_missing_requires_absent_before_evidence() {
    let root = tempdir().unwrap();
    let mut db = unsealed(root.path(), "", None);
    {
        let tx = db.transaction().unwrap();
        assert!(tx.execute_batch(STAGES).is_err());
    }
    let version = |db: &Connection| {
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(version(&db), 1);
    db.execute(SEAL, []).unwrap();

    let root = tempdir().unwrap();
    let db = unsealed(root.path(), STAGES, None);
    assert_eq!(version(&db), 2);
    let template = File::create_new(root.path().join("template")).unwrap();
    let (profile, attrs) = attributes(&template);
    let insert = "INSERT INTO source_native VALUES(0,?1,?2,?3,?4)";
    assert!(
        db.execute(insert, params![profile, object(&template), &attrs, &attrs])
            .is_err()
    );
    assert!(
        db.execute(insert, params![profile, Null, Null, [0_u8; 1]])
            .is_err()
    );
    db.execute(insert, params![profile, Null, Null, attrs])
        .unwrap();
    db.execute(SEAL, []).unwrap();
    db.execute(APPLY, []).unwrap();
    db.execute(INTENT, []).unwrap();
    let missing = Sample::of(&root.path().join(GLOBAL));
    assert_eq!(
        inspect(&db, "apply", profile, &[3], &missing, &missing),
        "previous_target_stage_absent"
    );
    fs::write(root.path().join(GLOBAL), NEW).unwrap();
    assert_eq!(
        inspect(
            &db,
            "apply",
            profile,
            &[3],
            &Sample::of(&root.path().join(GLOBAL)),
            &missing
        ),
        "inspect_required"
    );
    db.execute("DELETE FROM stage_intent", []).unwrap();
    db.execute("UPDATE operation SET state='restoring'", [])
        .unwrap();
    assert!(
        db.execute(
            "INSERT INTO stage_intent(ordinal,direction) VALUES(0,'restore')",
            []
        )
        .is_err()
    );
}

#[test]
fn interrupted_stage_child() {
    let Some(root) = std::env::var_os("DOCSBASE_TEST_STAGE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let step = std::env::var("DOCSBASE_TEST_STAGE_STOP").unwrap();
    let db = open(root);
    db.execute(INTENT, []).unwrap();
    if step == "intent" {
        std::process::exit(71)
    }
    let name: String = db
        .query_row("SELECT stage_name FROM outstanding_stage", [], |r| r.get(0))
        .unwrap();
    let stage = root.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage)
        .unwrap();
    file.write_all(NEW).unwrap();
    file.sync_all().unwrap();
    if step == "created" {
        std::process::exit(72)
    }
    db.execute("UPDATE stage_intent SET object_id=?1", [object(&file)])
        .unwrap();
    drop(file);
    if step == "identified" {
        std::process::exit(73)
    }
    assert_eq!(step, "renamed");
    fs::rename(stage, root.join(GLOBAL)).unwrap();
    std::process::exit(74)
}

const INSPECT: &str = include_str!("../../docs/specs/component-configuration/inspect-stage-v2.sql");

struct Sample {
    kind: &'static str,
    object: Option<Vec<u8>>,
    hash: Option<[u8; 32]>,
    attributes: Option<Vec<u8>>,
}

impl Sample {
    // Only used with private, small synthetic files; the real publisher needs
    // namespace admission, bounded reads and qualified native attribute capture.
    fn of(path: &Path) -> Self {
        match File::open(path) {
            Ok(mut file) => {
                let mut contents = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut contents).unwrap();
                Self {
                    kind: "file",
                    object: Some(object(&file)),
                    hash: Some(*blake3::hash(&contents).as_bytes()),
                    attributes: Some(attributes(&file).1),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self {
                kind: "missing",
                object: None,
                hash: None,
                attributes: None,
            },
            Err(e) => panic!("unexpected fixture error: {e}"),
        }
    }
}

fn inspect(
    db: &Connection,
    direction: &str,
    profile: &str,
    parent: &[u8],
    target: &Sample,
    stage: &Sample,
) -> String {
    db.query_row(INSPECT, named_params! {
        ":operation": [0u8;32], ":ordinal": 0, ":direction": direction,
        ":parent": parent, ":profile": profile,
        ":target_kind": target.kind, ":target_id": target.object, ":target_hash": target.hash,
        ":target_attrs": target.attributes, ":stage_kind": stage.kind, ":stage_id": stage.object,
        ":stage_hash": stage.hash, ":stage_attrs": stage.attributes,
    }, |row| row.get(0)).unwrap()
}

#[test]
fn restart_preserves_intent_across_creation_identity_and_rename() {
    for (step, code) in [
        ("intent", 71),
        ("created", 72),
        ("identified", 73),
        ("renamed", 74),
    ] {
        let root = tempdir().unwrap();
        let db = fixture(root.path());
        record_images(&db, 0, &root.path().join(GLOBAL));
        db.execute(SEAL, []).unwrap();
        db.execute(APPLY, []).unwrap();
        drop(db);
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::stage_journal_tests::interrupted_stage_child",
            ])
            .env("DOCSBASE_TEST_STAGE_ROOT", root.path())
            .env("DOCSBASE_TEST_STAGE_STOP", step)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        let db = Connection::open_with_flags(
            root.path().join("control.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (name, recorded, hash, attrs): (String, Option<Vec<u8>>, Vec<u8>, Vec<u8>) = db.query_row(
            "SELECT stage_name, object_id, expected_hash, expected_attributes FROM outstanding_stage", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        let stage = root.path().join(&name);
        assert_eq!(name, format!(".docsbase-stage-{}-0-apply", "00".repeat(32)));
        assert_eq!(hash, blake3::hash(NEW).as_bytes());
        assert_eq!(
            attrs,
            attributes(&File::open(root.path().join(GLOBAL)).unwrap()).1
        );
        assert_eq!(
            fs::read(root.path().join(GLOBAL)).unwrap(),
            if step == "renamed" { NEW } else { OLD }
        );
        assert_eq!(stage.exists(), matches!(step, "created" | "identified"));
        let (profile, _) = attributes(&File::open(root.path().join(GLOBAL)).unwrap());
        let target_sample = Sample::of(&root.path().join(GLOBAL));
        let stage_sample = Sample::of(&stage);
        let observed = inspect(&db, "apply", profile, &[3], &target_sample, &stage_sample);
        let expected = match step {
            "intent" => "previous_target_stage_absent",
            "identified" => "previous_target_stage_matches",
            "renamed" => "target_matches_stage_absent",
            _ => "inspect_required",
        };
        assert_eq!(observed, expected);
        assert_eq!(
            inspect(&db, "apply", profile, &[4], &target_sample, &stage_sample),
            "inspect_required"
        );
        match step {
            "identified" => assert_eq!(recorded, Some(object(&File::open(stage).unwrap()))),
            "renamed" => assert_eq!(
                recorded,
                Some(object(&File::open(root.path().join(GLOBAL)).unwrap()))
            ),
            _ => assert!(recorded.is_none()),
        }
    }
}

#[test]
fn a_foreign_object_or_changed_stage_attributes_or_bytes_need_inspection() {
    let root = tempdir().unwrap();
    let db = fixture(root.path());
    record_images(&db, 0, &root.path().join(GLOBAL));
    db.execute(SEAL, []).unwrap();
    db.execute(APPLY, []).unwrap();
    db.execute(INTENT, []).unwrap();
    let name = db
        .query_row("SELECT stage_name FROM outstanding_stage", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    let stage = root.path().join(name);
    fs::write(&stage, NEW).unwrap();
    let file = File::open(&stage).unwrap();
    let (profile, _) = attributes(&file);
    let recorded = object(&file);
    drop(file);
    db.execute("UPDATE stage_intent SET object_id=?1", [recorded])
        .unwrap();
    let target = Sample::of(&root.path().join(GLOBAL));
    let diagnose = || inspect(&db, "apply", profile, &[3], &target, &Sample::of(&stage));
    assert_eq!(diagnose(), "previous_target_stage_matches");
    let original = fs::metadata(&stage).unwrap().permissions();
    let mut changed = original.clone();
    changed.set_readonly(true);
    fs::set_permissions(&stage, changed).unwrap();
    assert_eq!(diagnose(), "inspect_required");
    fs::set_permissions(&stage, original).unwrap();
    fs::write(&stage, b"diverged").unwrap();
    assert_eq!(diagnose(), "inspect_required");
    fs::write(&stage, NEW).unwrap();
    assert_eq!(diagnose(), "previous_target_stage_matches");
    fs::rename(&stage, root.path().join("original-stage")).unwrap();
    fs::write(&stage, NEW).unwrap();
    assert_eq!(diagnose(), "inspect_required");
    assert_eq!(fs::read(&stage).unwrap(), NEW);
}

#[test]
fn a_second_source_cannot_evict_the_slot_and_unmodified_sources_need_no_restore_stage() {
    let root = tempdir().unwrap();
    let db = fixture(root.path());
    record_images(&db, 0, &root.path().join(GLOBAL));
    let second = root.path().join("project").join(".docsbase.toml");
    fs::create_dir(root.path().join("project")).unwrap();
    fs::write(&second, OLD).unwrap();
    db.execute(
        "INSERT INTO source SELECT 1,operation_id,'project',1,?1,x'04',
         before_exists,before_bytes,before_hash,after_exists,after_bytes,after_hash,progress
         FROM source WHERE ordinal=0",
        [second.to_str().unwrap()],
    )
    .unwrap();
    assert!(
        db.execute("UPDATE source SET rowid=2 WHERE ordinal=1", [])
            .is_err(),
        "the rowid alias cannot rebind a project source"
    );
    record_images(&db, 1, &second);
    db.execute("UPDATE operation SET source_count=2, before_total=2*before_total, after_total=2*after_total", []).unwrap();
    db.execute(SEAL, []).unwrap();
    db.execute(APPLY, []).unwrap();
    db.execute(INTENT, []).unwrap();
    assert!(
        db.execute(
            "INSERT OR REPLACE INTO stage_intent(ordinal,direction) VALUES(1,'apply')",
            []
        )
        .is_err()
    );
    let active: i64 = db
        .query_row("SELECT ordinal FROM outstanding_stage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(active, 0);
    db.execute("DELETE FROM stage_intent", []).unwrap();
    db.execute("UPDATE operation SET state='restoring'", [])
        .unwrap();
    assert!(
        db.execute(
            "INSERT INTO stage_intent(ordinal,direction) VALUES(1,'restore')",
            []
        )
        .is_err()
    );
}

#[test]
fn restore_checks_previous_publication_and_original_attributes() {
    let root = tempdir().unwrap();
    let db = fixture(root.path());
    let target = root.path().join(GLOBAL);
    let old = File::open(&target).unwrap();
    let (profile, before_attrs) = attributes(&old);
    let before_object = object(&old);
    drop(old);
    let applied = root.path().join("applied");
    fs::write(&applied, NEW).unwrap();
    let original_permissions = fs::metadata(&applied).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&applied, readonly).unwrap();
    let applied_file = File::open(&applied).unwrap();
    let (_, after_attrs) = attributes(&applied_file);
    assert_ne!(before_attrs, after_attrs);
    db.execute(
        "INSERT INTO source_native VALUES (0,?1,?2,?3,?4)",
        params![profile, before_object, before_attrs, after_attrs],
    )
    .unwrap();
    db.execute(SEAL, []).unwrap();
    db.execute(APPLY, []).unwrap();
    db.execute(INTENT, []).unwrap();
    db.execute(
        "UPDATE stage_intent SET object_id=?1",
        [object(&applied_file)],
    )
    .unwrap();
    drop(applied_file);
    fs::rename(&applied, &target).unwrap();
    db.execute("UPDATE stage_intent SET published=1", [])
        .unwrap();
    db.execute("UPDATE operation SET state='restoring'", [])
        .unwrap();
    db.execute(
        "INSERT INTO stage_intent(ordinal,direction) VALUES(0,'restore')",
        [],
    )
    .unwrap();
    let name: String = db
        .query_row("SELECT stage_name FROM outstanding_stage", [], |r| r.get(0))
        .unwrap();
    let stage = root.path().join(name);
    fs::write(&stage, OLD).unwrap();
    let restored = File::open(&stage).unwrap();
    db.execute(
        "UPDATE stage_intent SET object_id=?1 WHERE direction='restore'",
        [object(&restored)],
    )
    .unwrap();
    drop(restored);
    let diagnose = || {
        inspect(
            &db,
            "restore",
            profile,
            &[3],
            &Sample::of(&target),
            &Sample::of(&stage),
        )
    };
    assert_eq!(diagnose(), "previous_target_stage_matches");
    fs::set_permissions(&target, original_permissions).unwrap();
    assert_eq!(diagnose(), "inspect_required");
}
