CREATE TABLE review_tasks (
    task_id TEXT PRIMARY KEY,
    task_json TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE review_artifacts (
    content_hash TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
    content BLOB NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE review_cycles (
    cycle_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK(attempt >= 0),
    state TEXT NOT NULL,
    disposition TEXT NOT NULL,
    cycle_json TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    FOREIGN KEY(task_id) REFERENCES review_tasks(task_id)
);

CREATE TABLE review_stage_executions (
    cycle_id TEXT NOT NULL,
    stage_id TEXT NOT NULL,
    stage_kind TEXT NOT NULL,
    observation_json TEXT NOT NULL,
    PRIMARY KEY(cycle_id, stage_id),
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);

CREATE TABLE review_findings (
    finding_id TEXT PRIMARY KEY,
    cycle_id TEXT NOT NULL,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    blocking INTEGER NOT NULL CHECK(blocking IN (0,1)),
    status TEXT NOT NULL,
    finding_json TEXT NOT NULL,
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);
CREATE TABLE review_finding_events (
    cycle_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    review_attempt INTEGER NOT NULL,
    status TEXT NOT NULL,
    finding_json TEXT NOT NULL,
    PRIMARY KEY(cycle_id, finding_id, review_attempt, status),
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);

CREATE TABLE review_verification_evidence (
    cycle_id TEXT NOT NULL,
    check_id TEXT NOT NULL,
    phase TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    PRIMARY KEY(cycle_id, check_id, phase),
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);

CREATE TABLE lesson_proposals (
    lesson_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    classification TEXT NOT NULL,
    status TEXT NOT NULL,
    proposal_json TEXT NOT NULL,
    proposed_at TEXT NOT NULL,
    FOREIGN KEY(finding_id) REFERENCES review_findings(finding_id)
);
CREATE TABLE lesson_proposal_events (
    lesson_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    status TEXT NOT NULL,
    actor_json TEXT,
    occurred_at TEXT NOT NULL,
    PRIMARY KEY(lesson_id, sequence),
    FOREIGN KEY(lesson_id) REFERENCES lesson_proposals(lesson_id)
);

CREATE INDEX review_cycles_task_idx ON review_cycles(task_id, attempt);
CREATE INDEX review_findings_cycle_idx ON review_findings(cycle_id, blocking, status);
