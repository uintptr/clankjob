-- The owner's contacts, and when a case's approval-gated calls wait for the owner.

ALTER TABLE cases ADD COLUMN approvals TEXT NOT NULL DEFAULT 'default';

CREATE TABLE contacts (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    email       TEXT,
    phone       TEXT,
    note        TEXT,
    trusted     INTEGER NOT NULL DEFAULT 0,
    added_by    TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE INDEX contacts_name ON contacts (name COLLATE NOCASE);
