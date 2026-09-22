-- FAM-BUG-078: the FAM-BUG-074 fix added a new selection decision,
-- `front_matter_hold`, and the first drive that reached it (PRD-108's
-- hands-off run, 2026-09-22) died `storage_failure` on this table's CHECK
-- constraint before any PRD was attempted. The decision vocabulary lives in
-- two places, `drive.rs` and this constraint, and nothing pinned them
-- together. Same rebuild shape as 052/053, one value wider.
CREATE TABLE driver_selection_decisions_v4 (
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
        'front_matter_hold'
    )),
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

INSERT INTO driver_selection_decisions_v4
    (decision_id, session_id, prd_id, decision, detail, recorded_at)
SELECT decision_id, session_id, prd_id, decision, detail, recorded_at
FROM driver_selection_decisions;

DROP TABLE driver_selection_decisions;

ALTER TABLE driver_selection_decisions_v4 RENAME TO driver_selection_decisions;

CREATE INDEX idx_selection_decisions_session_v4
    ON driver_selection_decisions(session_id, prd_id);
