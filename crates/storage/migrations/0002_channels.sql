-- Human channels (design §10.3): questions and notifications delivered to chat channels
-- such as Discord, and answers polled back.

-- JSON array of channel instance names, e.g. ["discord_joe"].
ALTER TABLE cases ADD COLUMN human_channels TEXT NOT NULL DEFAULT '[]';

-- Outbox: written in the same transaction as the question or state change it is about,
-- then sent by the channel thread, with retries.
CREATE TABLE channel_deliveries (
    id                TEXT PRIMARY KEY,
    case_id           TEXT NOT NULL REFERENCES cases (id),
    human_request_id  TEXT REFERENCES human_requests (id),
    channel           TEXT NOT NULL,
    kind              TEXT NOT NULL,
    payload           TEXT NOT NULL,
    status            TEXT NOT NULL,
    external          TEXT,
    attempts          INTEGER NOT NULL DEFAULT 0,
    next_attempt_at   INTEGER NOT NULL,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX channel_deliveries_due ON channel_deliveries (status, next_attempt_at);
CREATE INDEX channel_deliveries_request ON channel_deliveries (human_request_id, channel);

-- Where each channel's poll continues (e.g. the last message seen per Discord thread).
CREATE TABLE channel_cursors (
    channel     TEXT PRIMARY KEY,
    cursor      TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
