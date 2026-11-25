-- Installed by the staging owner alongside the selected project's metadata rows.
CREATE TABLE generation_state (
    singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
    project_id       INTEGER NOT NULL CHECK (project_id > 0),
    generation_id    INTEGER NOT NULL CHECK (generation_id > 0),
    content_revision INTEGER NOT NULL CHECK (content_revision >= 0)
) STRICT;
