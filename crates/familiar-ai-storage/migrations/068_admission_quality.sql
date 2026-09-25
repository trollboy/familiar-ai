CREATE TABLE admission_quality_results (
    repository_key TEXT NOT NULL,
    prd_id TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    check_name TEXT NOT NULL,
    field_name TEXT NOT NULL,
    passed INTEGER NOT NULL CHECK(passed IN (0,1)),
    detail TEXT NOT NULL,
    measured_at TEXT NOT NULL,
    PRIMARY KEY(repository_key, content_hash, check_name, field_name)
);
CREATE INDEX admission_quality_prd_idx
    ON admission_quality_results(repository_key, prd_id, measured_at DESC);

CREATE TABLE reviewer_finding_outcomes (
    cycle_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    reviewer_identity TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('unresolved','confirmed','waived','invalid')),
    actor TEXT,
    reason TEXT,
    landed_revision TEXT,
    raised_at TEXT NOT NULL,
    resolved_at TEXT,
    PRIMARY KEY(cycle_id, finding_id),
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);
CREATE INDEX reviewer_finding_calibration_idx
    ON reviewer_finding_outcomes(reviewer_identity, raised_at DESC);

CREATE TRIGGER reviewer_finding_outcomes_no_delete
BEFORE DELETE ON reviewer_finding_outcomes
BEGIN SELECT RAISE(ABORT, 'reviewer finding outcomes are durable'); END;
