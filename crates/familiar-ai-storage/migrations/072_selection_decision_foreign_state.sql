-- FAM-BUG-081: two more selection decisions, `claimed_elsewhere` (a drive
-- branch on origin holds the PRD) and `archived_upstream` (the file is under
-- the archive on origin). Same rebuild shape as 071, two values wider;
-- `SELECTION_DECISIONS` in drive.rs and its regression keep the two lists
-- from drifting again.
CREATE TABLE driver_selection_decisions_v5 (
    decision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES driver_sessions(session_id),
    prd_id TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN (
        'ready_selected',
        'deferred_scope_overlap',
        'deferred_scope_held',
        'deferred_resource',
        'deferred_width',
        'deferred_dependency_undelivered',
        'dependency_not_integrated',
        'deferred_scope_unavailable',
        'excluded_allowlist',
        'front_matter_hold',
        'claimed_elsewhere',
        'archived_upstream'
    )),
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

INSERT INTO driver_selection_decisions_v5
    (decision_id, session_id, prd_id, decision, detail, recorded_at)
SELECT decision_id, session_id, prd_id, decision, detail, recorded_at
FROM driver_selection_decisions;

DROP TABLE driver_selection_decisions;

ALTER TABLE driver_selection_decisions_v5 RENAME TO driver_selection_decisions;

CREATE INDEX idx_selection_decisions_session_v5
    ON driver_selection_decisions(session_id, prd_id);
