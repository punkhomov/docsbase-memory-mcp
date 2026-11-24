-- Fresh-format DDL: execute control-state-v1.sql and this extension in one
-- transaction on a new private database. Existing journals cannot be upgraded.
CREATE TEMP TABLE require_empty_v1 (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO require_empty_v1 SELECT
    (SELECT application_id FROM pragma_application_id) = 1145193286
    AND (SELECT user_version FROM pragma_user_version) = 1
    AND NOT EXISTS (SELECT 1 FROM namespace)
    AND NOT EXISTS (SELECT 1 FROM operation)
    AND NOT EXISTS (SELECT 1 FROM source);
DROP TABLE require_empty_v1;

-- Fixed-size basic native evidence. A qualified publisher must separately
-- establish whether this profile covers all attributes on the target storage.
CREATE TABLE source_native (
    ordinal INTEGER PRIMARY KEY REFERENCES source(ordinal),
    profile TEXT NOT NULL CHECK (profile IN ('unix-stat-v1', 'windows-attributes-v1')),
    before_object BLOB CHECK (length(before_object) = 24 AND substr(before_object, 9) != zeroblob(16)),
    before_attributes BLOB,
    after_attributes BLOB,
    CHECK ((before_attributes IS NULL) = (before_object IS NULL)),
    CHECK (length(before_attributes) = CASE profile WHEN 'unix-stat-v1' THEN 12 ELSE 4 END),
    CHECK (length(after_attributes) = CASE profile WHEN 'unix-stat-v1' THEN 12 ELSE 4 END)
) STRICT;

CREATE TRIGGER native_insert BEFORE INSERT ON source_native
WHEN EXISTS (SELECT 1 FROM source_native WHERE ordinal = NEW.ordinal)
    OR NOT EXISTS (SELECT 1 FROM source JOIN operation USING(operation_id)
        WHERE ordinal = NEW.ordinal AND state = 'preparing' AND sealed = 0
          AND before_exists = (NEW.before_object IS NOT NULL)
          AND after_exists = (NEW.after_attributes IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'unexpected native evidence'); END;
CREATE TRIGGER native_frozen BEFORE UPDATE ON source_native
BEGIN SELECT RAISE(ABORT, 'immutable native evidence'); END;
CREATE TRIGGER native_delete BEFORE DELETE ON source_native
WHEN NOT EXISTS (SELECT 1 FROM operation WHERE state = 'restored_legacy')
BEGIN SELECT RAISE(ABORT, 'native evidence still needed'); END;
CREATE TRIGGER source_no_replace BEFORE INSERT ON source
WHEN EXISTS (SELECT 1 FROM source WHERE ordinal = NEW.ordinal OR target_path = NEW.target_path)
BEGIN SELECT RAISE(ABORT, 'source already exists'); END;
-- An INTEGER PRIMARY KEY can also be addressed through SQLite rowid aliases.
CREATE TRIGGER source_ordinal_frozen BEFORE UPDATE ON source
WHEN NEW.ordinal != OLD.ordinal
BEGIN SELECT RAISE(ABORT, 'immutable source ordinal'); END;
CREATE TRIGGER native_seal BEFORE UPDATE OF sealed ON operation
WHEN NEW.sealed = 1 AND EXISTS (SELECT 1 FROM source LEFT JOIN source_native USING(ordinal)
    WHERE source_native.ordinal IS NULL OR progress != 'pending')
BEGIN SELECT RAISE(ABORT, 'incomplete native evidence'); END;

CREATE TABLE stage_intent (
    ordinal INTEGER NOT NULL REFERENCES source_native(ordinal),
    direction TEXT NOT NULL CHECK (direction IN ('apply', 'restore')),
    object_id BLOB CHECK (length(object_id) = 24 AND substr(object_id, 9) != zeroblob(16)),
    published INTEGER NOT NULL DEFAULT 0 CHECK (published IN (0, 1)),
    PRIMARY KEY(ordinal, direction)
) STRICT, WITHOUT ROWID;
CREATE UNIQUE INDEX one_outstanding_stage ON stage_intent(published) WHERE published = 0;

-- Explicit guard also prevents REPLACE evicting either the active slot or a
-- retained published identity, even with recursive_triggers disabled.
CREATE TRIGGER stage_insert BEFORE INSERT ON stage_intent
WHEN NEW.object_id IS NOT NULL OR NEW.published != 0
    OR EXISTS (SELECT 1 FROM stage_intent WHERE published = 0
               OR (ordinal = NEW.ordinal AND direction = NEW.direction))
    OR NOT EXISTS (SELECT 1 FROM source JOIN operation USING(operation_id)
        WHERE ordinal = NEW.ordinal AND sealed = 1 AND (
           (NEW.direction = 'apply' AND state = 'applying' AND after_exists AND progress != 'new_verified')
        OR (NEW.direction = 'restore' AND state = 'restoring' AND before_exists AND progress != 'old_verified'
            AND (after_exists = 0 OR EXISTS (SELECT 1 FROM stage_intent
                 WHERE ordinal = NEW.ordinal AND direction = 'apply' AND published = 1)))))
BEGIN SELECT RAISE(ABORT, 'unexpected stage intent'); END;
CREATE TRIGGER stage_binding BEFORE UPDATE OF ordinal, direction ON stage_intent
BEGIN SELECT RAISE(ABORT, 'immutable stage binding'); END;
CREATE TRIGGER stage_object BEFORE UPDATE OF object_id ON stage_intent
WHEN OLD.object_id IS NOT NULL OR NEW.object_id IS NULL OR OLD.published = 1
BEGIN SELECT RAISE(ABORT, 'stage identity is recorded once'); END;
CREATE TRIGGER stage_publish BEFORE UPDATE OF published ON stage_intent
WHEN OLD.published != 0 OR NEW.published != 1 OR OLD.object_id IS NULL
BEGIN SELECT RAISE(ABORT, 'stage identity required before publication'); END;
-- Caller has completed the physical checks. Commit both database facts in
-- a single statement, including rollback if the source progress update fails.
CREATE TRIGGER stage_published_progress AFTER UPDATE OF published ON stage_intent
WHEN NEW.published = 1
BEGIN
    UPDATE source SET progress = CASE NEW.direction
        WHEN 'apply' THEN 'new_verified' ELSE 'old_verified' END
    WHERE ordinal = NEW.ordinal;
END;
-- A pending row may only be explicitly withdrawn by the caller after namespace
-- reconciliation. Published IDs survive for restore and terminal verification.
CREATE TRIGGER stage_delete BEFORE DELETE ON stage_intent
WHEN OLD.published = 1 AND NOT EXISTS (SELECT 1 FROM operation WHERE state = 'restored_legacy')
BEGIN SELECT RAISE(ABORT, 'published identity still needed'); END;
CREATE TRIGGER stage_phase BEFORE UPDATE OF state ON operation
WHEN OLD.state != NEW.state AND EXISTS (SELECT 1 FROM stage_intent WHERE published = 0)
BEGIN SELECT RAISE(ABORT, 'stage reconciliation required'); END;
CREATE TRIGGER stage_progress BEFORE UPDATE OF progress ON source
WHEN EXISTS (SELECT 1 FROM stage_intent WHERE ordinal = NEW.ordinal AND published = 0)
BEGIN SELECT RAISE(ABORT, 'stage reconciliation required'); END;

-- Private recovery inputs. The generated basename is fixed for every retry of
-- this source and direction. It never includes caller-supplied path fragments.
CREATE VIEW outstanding_stage AS SELECT
    s.operation_id, i.ordinal, i.direction, s.target_path, s.parent_identity, n.profile,
    '.docsbase-stage-' || lower(hex(s.operation_id)) || '-' || i.ordinal || '-' || i.direction AS stage_name,
    i.object_id,
    CASE i.direction WHEN 'apply' THEN s.after_hash ELSE s.before_hash END AS expected_hash,
    CASE i.direction WHEN 'apply' THEN n.after_attributes ELSE n.before_attributes END AS expected_attributes,
    CASE i.direction WHEN 'apply' THEN s.before_exists ELSE s.after_exists END AS previous_exists,
    CASE i.direction WHEN 'apply' THEN n.before_object ELSE applied.object_id END AS previous_object,
    CASE i.direction WHEN 'apply' THEN s.before_hash ELSE s.after_hash END AS previous_hash,
    CASE i.direction WHEN 'apply' THEN n.before_attributes ELSE n.after_attributes END AS previous_attributes
FROM stage_intent i
JOIN source s USING(ordinal)
JOIN source_native n USING(ordinal)
LEFT JOIN stage_intent applied ON applied.ordinal = i.ordinal AND applied.direction = 'apply' AND applied.published = 1
WHERE i.published = 0;

PRAGMA user_version = 2;
