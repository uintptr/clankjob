-- Skills: what cases learned and saved for later cases, approved by the owner, with every
-- version kept, and which cases used them.

CREATE TABLE skills (
    name        TEXT PRIMARY KEY,
    enabled     INTEGER NOT NULL DEFAULT 1,
    version     INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE skill_versions (
    skill        TEXT NOT NULL REFERENCES skills (name) ON DELETE CASCADE,
    version      INTEGER NOT NULL,
    description  TEXT NOT NULL,
    content      TEXT NOT NULL,
    -- JSON: [{ "name", "content" }]
    files        TEXT NOT NULL,
    saved_by     TEXT NOT NULL,
    case_id      TEXT REFERENCES cases (id) ON DELETE SET NULL,
    approval_id  TEXT,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (skill, version)
);

CREATE TABLE skill_uses (
    skill          TEXT NOT NULL REFERENCES skills (name) ON DELETE CASCADE,
    case_id        TEXT NOT NULL REFERENCES cases (id) ON DELETE CASCADE,
    uses           INTEGER NOT NULL,
    first_used_at  INTEGER NOT NULL,
    last_used_at   INTEGER NOT NULL,
    PRIMARY KEY (skill, case_id)
);
