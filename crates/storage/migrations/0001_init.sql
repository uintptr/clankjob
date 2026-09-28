-- Initial schema (design §16). Timestamps are Unix milliseconds, UTC.

CREATE TABLE cases (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    goal        TEXT NOT NULL,
    owner       TEXT,
    profile     TEXT,
    llm         TEXT NOT NULL,
    model       TEXT,
    state       TEXT NOT NULL,
    budgets     TEXT NOT NULL,
    usage       TEXT NOT NULL,
    result      TEXT,
    outcome     TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX cases_state ON cases (state, id);

-- Append-only. `seq` orders events globally and doubles as the polling cursor.
CREATE TABLE events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    case_id        TEXT NOT NULL REFERENCES cases (id),
    activation_id  TEXT,
    kind           TEXT NOT NULL,
    payload        TEXT NOT NULL,
    created_at     INTEGER NOT NULL
);
CREATE INDEX events_case ON events (case_id, seq);

CREATE TABLE activations (
    id             TEXT PRIMARY KEY,
    case_id        TEXT NOT NULL REFERENCES cases (id),
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    end_state      TEXT,
    usage          TEXT,
    prompt_hashes  TEXT NOT NULL
);
CREATE INDEX activations_case ON activations (case_id, started_at);

-- At most one row per case: its presence means "this case should run".
-- `rerun` records a wake that arrived while the case was already running.
CREATE TABLE work_queue (
    case_id       TEXT PRIMARY KEY REFERENCES cases (id),
    available_at  INTEGER NOT NULL,
    lease_until   INTEGER,
    rerun         INTEGER NOT NULL DEFAULT 0,
    attempts      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX work_queue_available ON work_queue (available_at);

CREATE TABLE wait_conditions (
    id             TEXT PRIMARY KEY,
    case_id        TEXT NOT NULL REFERENCES cases (id),
    kind           TEXT NOT NULL,
    params         TEXT NOT NULL,
    next_check_at  INTEGER,
    deadline_at    INTEGER,
    status         TEXT NOT NULL,
    created_at     INTEGER NOT NULL
);
CREATE INDEX wait_conditions_case ON wait_conditions (case_id, status);
CREATE INDEX wait_conditions_next_check ON wait_conditions (status, next_check_at);
CREATE INDEX wait_conditions_deadline ON wait_conditions (status, deadline_at);

CREATE TABLE case_notes (
    case_id     TEXT NOT NULL REFERENCES cases (id),
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (case_id, key)
);

CREATE TABLE human_requests (
    id            TEXT PRIMARY KEY,
    case_id       TEXT NOT NULL REFERENCES cases (id),
    question      TEXT NOT NULL,
    status        TEXT NOT NULL,
    answer        TEXT,
    answered_via  TEXT,
    responder     TEXT,
    created_at    INTEGER NOT NULL,
    resolved_at   INTEGER
);
CREATE INDEX human_requests_case ON human_requests (case_id, status);
CREATE INDEX human_requests_status ON human_requests (status, created_at);
