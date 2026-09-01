-- Version 52 was briefly assigned to this probe table before the PRD-077
-- delivery migration reached main with the same number. Some installations
-- may therefore have recorded 52 while applying these bytes. Re-applying the
-- delivery-table rebuild is harmless on the canonical history and repairs
-- that collision history before 53 is recorded.
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

CREATE TABLE IF NOT EXISTS review_capability_probes (
    spec_identity TEXT PRIMARY KEY REFERENCES worker_specs(spec_identity),
    structured_output INTEGER NOT NULL CHECK(structured_output IN (0,1)),
    native_tool_calling INTEGER NOT NULL CHECK(native_tool_calling IN (0,1)),
    protocol TEXT NOT NULL,
    runtime_version TEXT NOT NULL,
    provenance TEXT NOT NULL CHECK(provenance IN ('probed','observed')),
    probed_at TEXT NOT NULL
);
