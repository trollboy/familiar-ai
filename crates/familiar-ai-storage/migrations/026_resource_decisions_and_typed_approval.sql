-- PRD-065 remediation (wave-fixes-review F1, F2).
--
-- F2: widen the closed selection-decision vocabulary with
-- 'deferred_resource' — a ready PRD deferred because another selected PRD
-- holds one of its declared exclusive resources.
CREATE TABLE driver_selection_decisions_v2 (
    decision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES driver_sessions(session_id),
    prd_id TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN (
        'ready_selected',
        'deferred_scope_overlap',
        'deferred_resource',
        'deferred_width',
        'deferred_dependency_undelivered',
        'deferred_scope_unavailable',
        'excluded_allowlist'
    )),
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

INSERT INTO driver_selection_decisions_v2
    (decision_id, session_id, prd_id, decision, detail, recorded_at)
SELECT decision_id, session_id, prd_id, decision, detail, recorded_at
FROM driver_selection_decisions;

DROP TABLE driver_selection_decisions;

ALTER TABLE driver_selection_decisions_v2 RENAME TO driver_selection_decisions;

CREATE INDEX idx_selection_decisions_session
    ON driver_selection_decisions(session_id, prd_id);

-- F1: the approved candidate hash and resulting commit are typed columns on
-- the checkpoint, not only free-text event detail.
ALTER TABLE execution_checkpoints ADD COLUMN approved_diff_hash TEXT;
ALTER TABLE execution_checkpoints ADD COLUMN approved_commit TEXT;
