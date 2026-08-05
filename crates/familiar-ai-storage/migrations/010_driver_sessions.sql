CREATE TABLE driver_sessions (
    session_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT NULL,
    termination_reason TEXT NULL,
    warrant_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE driver_attempts (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    prd_id TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    execution_id TEXT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT NULL,
    outcome TEXT NULL CHECK (outcome IN ('completed', 'retained')),
    retained_reason TEXT NULL,
    known_cost_microusd INTEGER NULL,
    duration_ms INTEGER NULL,
    PRIMARY KEY (session_id, sequence),
    FOREIGN KEY (session_id) REFERENCES driver_sessions(session_id)
);

CREATE INDEX driver_attempts_session_idx ON driver_attempts(session_id, prd_id);
CREATE INDEX driver_sessions_started_idx ON driver_sessions(started_at DESC);
