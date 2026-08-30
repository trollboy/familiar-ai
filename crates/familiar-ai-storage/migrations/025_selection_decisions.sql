-- PRD-065: durable scheduling decisions. Every ready-set selection or
-- deferral records a machine-readable reason so an operator can always
-- answer why a PRD did or did not run in a session.
CREATE TABLE driver_selection_decisions (
    decision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES driver_sessions(session_id),
    prd_id TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN (
        'ready_selected',
        'deferred_scope_overlap',
        'deferred_width',
        'deferred_dependency_undelivered',
        'deferred_scope_unavailable',
        'excluded_allowlist'
    )),
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE INDEX idx_selection_decisions_session
    ON driver_selection_decisions(session_id, prd_id);

-- PRD-065: approve-and-complete — one transactional operation that completes
-- a reviewed retained checkpoint under recorded human authority, binding the
-- approved candidate hash and resulting commit.
CREATE TABLE backlog_recovery_events_v3 (
    status_event_id INTEGER PRIMARY KEY,
    action TEXT NOT NULL CHECK(action IN ('release', 'manual_complete_override', 'recorded_complete', 'approve_and_complete')),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    FOREIGN KEY(status_event_id) REFERENCES backlog_status_events(event_id)
);

INSERT INTO backlog_recovery_events_v3 (status_event_id, action, reason)
SELECT status_event_id, action, reason FROM backlog_recovery_events;

DROP TABLE backlog_recovery_events;

ALTER TABLE backlog_recovery_events_v3 RENAME TO backlog_recovery_events;
