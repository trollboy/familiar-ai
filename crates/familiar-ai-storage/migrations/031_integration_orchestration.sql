-- PRD-066: durable composition, allocatable migration versions, and
-- hash-bound human scope decisions.
ALTER TABLE driver_sessions ADD COLUMN base_revision TEXT;
ALTER TABLE driver_sessions ADD COLUMN integration_revision TEXT;

ALTER TABLE driver_attempts ADD COLUMN candidate_revision TEXT;
ALTER TABLE driver_attempts ADD COLUMN integrated_at TEXT;

CREATE TABLE migration_version_reservations (
    repository_key TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version > 0),
    session_id TEXT NOT NULL REFERENCES driver_sessions(session_id),
    prd_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('reserved','consumed','released')),
    reserved_at TEXT NOT NULL,
    resolved_at TEXT,
    PRIMARY KEY(repository_key, version),
    UNIQUE(session_id, prd_id)
);
CREATE INDEX migration_reservations_active_idx
    ON migration_version_reservations(repository_key,state,version);

CREATE TABLE scope_decisions (
    finding_hash TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    checkpoint_id TEXT NOT NULL REFERENCES execution_checkpoints(checkpoint_id),
    prd_id TEXT NOT NULL,
    candidate_hash TEXT NOT NULL,
    finding_json TEXT NOT NULL,
    decision TEXT CHECK(decision IN ('approved','rejected')),
    actor TEXT,
    reason TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX scope_decisions_pending_idx
    ON scope_decisions(repository_key,decision,prd_id);
