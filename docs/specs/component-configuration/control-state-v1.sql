-- CF5b1 research contract, not a product migration or an activation API.
-- Run only in a new, private, isolated database. Connection PRAGMAs are separate.
PRAGMA application_id = 1145193286; -- ASCII DBCF
PRAGMA user_version = 1;

CREATE TABLE namespace (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    namespace_id BLOB NOT NULL CHECK (length(namespace_id) = 32),
    cache_root TEXT NOT NULL CHECK (length(CAST(cache_root AS BLOB)) BETWEEN 1 AND 1048576),
    config_root TEXT NOT NULL CHECK (length(CAST(config_root AS BLOB)) BETWEEN 1 AND 1048576),
    data_root TEXT NOT NULL CHECK (length(CAST(data_root AS BLOB)) BETWEEN 1 AND 1048576),
    root_identities BLOB NOT NULL CHECK (length(root_identities) BETWEEN 1 AND 1048576)
) STRICT;

CREATE TABLE operation (
    singleton INTEGER PRIMARY KEY REFERENCES namespace(singleton) CHECK (singleton = 1),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 32),
    state TEXT NOT NULL CHECK (state IN ('preparing', 'applying', 'strict', 'restoring', 'restored_legacy')),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1)),
    plan_id TEXT NOT NULL CHECK (length(CAST(plan_id AS BLOB)) BETWEEN 1 AND 256),
    observation_id TEXT NOT NULL CHECK (length(CAST(observation_id AS BLOB)) BETWEEN 1 AND 256),
    consent_id TEXT NOT NULL CHECK (length(CAST(consent_id AS BLOB)) BETWEEN 1 AND 256),
    registry_snapshot BLOB NOT NULL CHECK (length(registry_snapshot) BETWEEN 1 AND 1048576),
    source_count INTEGER NOT NULL CHECK (source_count BETWEEN 1 AND 257),
    before_total INTEGER NOT NULL CHECK (before_total BETWEEN 0 AND 16777216),
    after_total INTEGER NOT NULL CHECK (after_total BETWEEN 0 AND 16777216),
    vector_max_entries INTEGER NOT NULL CHECK (vector_max_entries > 0),
    vector_max_bytes INTEGER NOT NULL CHECK (vector_max_bytes > 0),
    vector_busy_timeout_ms INTEGER NOT NULL CHECK (vector_busy_timeout_ms BETWEEN 0 AND 2147483647)
) STRICT;

CREATE TABLE source (
    ordinal INTEGER PRIMARY KEY CHECK (ordinal BETWEEN 0 AND 256),
    operation_id BLOB NOT NULL REFERENCES operation(operation_id),
    role TEXT NOT NULL CHECK (role IN ('global', 'project')),
    project_id INTEGER,
    target_path TEXT NOT NULL UNIQUE CHECK (length(CAST(target_path AS BLOB)) BETWEEN 1 AND 1048576),
    parent_identity BLOB NOT NULL CHECK (length(parent_identity) BETWEEN 1 AND 1048576),
    before_exists INTEGER NOT NULL CHECK (before_exists IN (0, 1)),
    before_bytes BLOB CHECK (length(before_bytes) <= 16777216),
    before_hash BLOB,
    after_exists INTEGER NOT NULL CHECK (after_exists IN (0, 1)),
    after_bytes BLOB CHECK (length(after_bytes) <= 16777216),
    after_hash BLOB,
    progress TEXT NOT NULL CHECK (progress IN ('pending', 'new_verified', 'old_verified')),
    CHECK ((role = 'global' AND project_id IS NULL AND ordinal = 0)
        OR (role = 'project' AND project_id IS NOT NULL AND project_id > 0 AND ordinal > 0)),
    CHECK ((before_exists = 0 AND before_bytes IS NULL AND before_hash IS NULL)
        OR (before_exists = 1 AND before_bytes IS NOT NULL AND before_hash IS NOT NULL AND length(before_hash) = 32)),
    CHECK ((after_exists = 0 AND after_bytes IS NULL AND after_hash IS NULL)
        OR (after_exists = 1 AND after_bytes IS NOT NULL AND after_hash IS NOT NULL AND length(after_hash) = 32))
) STRICT;

-- BEFORE INSERT also catches INSERT OR REPLACE when recursive_triggers is OFF.
CREATE TRIGGER namespace_no_replace BEFORE INSERT ON namespace
WHEN EXISTS (SELECT 1 FROM namespace)
BEGIN SELECT RAISE(ABORT, 'namespace already exists'); END;
CREATE TRIGGER namespace_no_update BEFORE UPDATE ON namespace
BEGIN SELECT RAISE(ABORT, 'immutable namespace'); END;
CREATE TRIGGER namespace_no_delete BEFORE DELETE ON namespace
BEGIN SELECT RAISE(ABORT, 'permanent namespace'); END;

CREATE TRIGGER operation_no_replace BEFORE INSERT ON operation
WHEN EXISTS (SELECT 1 FROM operation)
BEGIN SELECT RAISE(ABORT, 'operation already exists'); END;
CREATE TRIGGER operation_insert BEFORE INSERT ON operation
WHEN NEW.state != 'preparing' OR NEW.sealed != 0
BEGIN SELECT RAISE(ABORT, 'new operation must prepare'); END;
CREATE TRIGGER operation_delete BEFORE DELETE ON operation
WHEN OLD.state != 'restored_legacy'
BEGIN SELECT RAISE(ABORT, 'operation is not restored'); END;
CREATE TRIGGER operation_transition BEFORE UPDATE OF state ON operation
WHEN NEW.state != OLD.state AND NOT (
       (OLD.state = 'preparing' AND OLD.sealed = 1 AND NEW.state IN ('applying', 'restoring'))
    OR (OLD.state = 'applying' AND OLD.sealed = 1 AND NEW.state IN ('strict', 'restoring'))
    OR (OLD.state = 'restoring' AND OLD.sealed = 1 AND NEW.state = 'restored_legacy'))
BEGIN SELECT RAISE(ABORT, 'invalid transition'); END;
CREATE TRIGGER operation_terminal BEFORE UPDATE OF state ON operation
WHEN (NEW.state = 'strict' AND EXISTS (SELECT 1 FROM source WHERE progress != 'new_verified'))
    OR (NEW.state = 'restored_legacy' AND EXISTS (SELECT 1 FROM source WHERE progress != 'old_verified'))
BEGIN SELECT RAISE(ABORT, 'unverified sources'); END;
CREATE TRIGGER operation_no_unseal BEFORE UPDATE OF sealed ON operation
WHEN OLD.sealed = 1 OR NEW.sealed != 1
BEGIN SELECT RAISE(ABORT, 'invalid seal'); END;
CREATE TRIGGER operation_seal BEFORE UPDATE OF sealed ON operation
WHEN NEW.sealed = 1 AND (
       NEW.source_count != (SELECT count(*) FROM source)
    OR NEW.before_total != (SELECT coalesce(sum(length(before_bytes)), 0) FROM source)
    OR NEW.after_total != (SELECT coalesce(sum(length(after_bytes)), 0) FROM source)
    OR (SELECT count(*) FROM source WHERE role = 'global') != 1)
BEGIN SELECT RAISE(ABORT, 'incomplete images'); END;
CREATE TRIGGER operation_frozen BEFORE UPDATE OF
    singleton, operation_id, plan_id, observation_id, consent_id, registry_snapshot,
    source_count, before_total, after_total,
    vector_max_entries, vector_max_bytes, vector_busy_timeout_ms ON operation
WHEN OLD.sealed = 1
BEGIN SELECT RAISE(ABORT, 'sealed operation'); END;

CREATE TRIGGER source_insert BEFORE INSERT ON source
WHEN NOT EXISTS (SELECT 1 FROM operation WHERE state = 'preparing' AND sealed = 0)
BEGIN SELECT RAISE(ABORT, 'images already sealed'); END;
CREATE TRIGGER source_delete BEFORE DELETE ON source
WHEN NOT EXISTS (SELECT 1 FROM operation WHERE state = 'restored_legacy')
BEGIN SELECT RAISE(ABORT, 'images still needed'); END;
CREATE TRIGGER source_frozen BEFORE UPDATE OF
    ordinal, operation_id, role, project_id, target_path, parent_identity,
    before_exists, before_bytes, before_hash, after_exists, after_bytes, after_hash ON source
BEGIN SELECT RAISE(ABORT, 'immutable image'); END;
CREATE TRIGGER source_progress BEFORE UPDATE OF progress ON source
WHEN NOT EXISTS (SELECT 1 FROM operation WHERE sealed = 1 AND (
       (state = 'applying' AND NEW.progress = 'new_verified')
    OR (state = 'restoring' AND NEW.progress = 'old_verified')))
BEGIN SELECT RAISE(ABORT, 'invalid progress'); END;
