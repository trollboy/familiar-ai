-- PRD-077: widen the closed selection-decision vocabulary with the
-- autonomous-delivery deferrals — a dependent deferred because its
-- predecessor is not integrated into the session revision, and a PRD
-- deferred because an in-flight worker holds its scope.
CREATE TABLE driver_selection_decisions_v3 (
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
        'excluded_allowlist'
    )),
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

INSERT INTO driver_selection_decisions_v3
    (decision_id, session_id, prd_id, decision, detail, recorded_at)
SELECT decision_id, session_id, prd_id, decision, detail, recorded_at
FROM driver_selection_decisions;

DROP TABLE driver_selection_decisions;

ALTER TABLE driver_selection_decisions_v3 RENAME TO driver_selection_decisions;

CREATE INDEX idx_selection_decisions_session_v3
    ON driver_selection_decisions(session_id, prd_id);
