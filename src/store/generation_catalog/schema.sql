-- Run inside the stopped registry transition with foreign keys enabled.
-- The transition inserts the epoch and legacy heads before committing v4.
CREATE TABLE generation_store (
    singleton      INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    epoch          BLOB NOT NULL CHECK (length(epoch) = 16)
) STRICT;

CREATE TABLE project_generations (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    project_id          INTEGER NOT NULL CHECK (project_id > 0)
                        REFERENCES projects (id) ON DELETE CASCADE,
    layout              TEXT NOT NULL CHECK (layout IN ('legacy', 'local')),
    phase               TEXT NOT NULL CHECK (phase IN ('building', 'sealed', 'active', 'retired', 'reclaiming')),
    base_id             INTEGER CHECK (base_id IS NULL OR base_id > 0),
    base_revision       INTEGER NOT NULL CHECK (base_revision >= 0),
    plan_id             TEXT,
    seal_digest         BLOB CHECK (seal_digest IS NULL OR length(seal_digest) = 32),
    reserved_disk_bytes INTEGER NOT NULL CHECK (reserved_disk_bytes >= 0),
    active_member       INTEGER GENERATED ALWAYS AS (CASE WHEN phase = 'active' THEN id END) VIRTUAL,
    UNIQUE (project_id, id, phase),
    CHECK (layout = 'local' OR phase IN ('active', 'retired', 'reclaiming')),
    CHECK (layout = 'local' OR seal_digest IS NULL),
    CHECK (layout = 'legacy' OR phase IN ('building', 'reclaiming') OR seal_digest IS NOT NULL),
    FOREIGN KEY (project_id, active_member)
        REFERENCES project_generation_head (project_id, active_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE UNIQUE INDEX one_active_generation ON project_generations (project_id)
    WHERE phase = 'active';
CREATE UNIQUE INDEX one_staging_generation ON project_generations (project_id)
    WHERE phase IN ('building', 'sealed');

CREATE TABLE project_generation_head (
    project_id   INTEGER PRIMARY KEY CHECK (project_id > 0)
                 REFERENCES projects (id) ON DELETE CASCADE,
    active_id    INTEGER NOT NULL CHECK (active_id > 0),
    active_phase TEXT NOT NULL DEFAULT 'active' CHECK (active_phase = 'active'),
    UNIQUE (project_id, active_id),
    FOREIGN KEY (project_id, active_id, active_phase)
        REFERENCES project_generations (project_id, id, phase) DEFERRABLE INITIALLY DEFERRED
) STRICT;
