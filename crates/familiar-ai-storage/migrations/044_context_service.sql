-- PRD-070: append-only execution classification used to partition PRD-051 rows.
CREATE TABLE context_service_executions (
    execution_id TEXT PRIMARY KEY REFERENCES execution_history(execution_id),
    project_id TEXT,
    injection_enabled INTEGER NOT NULL CHECK(injection_enabled IN (0,1)),
    configured_at TEXT NOT NULL,
    audit_reason TEXT NOT NULL CHECK(length(trim(audit_reason)) > 0)
);
CREATE INDEX idx_context_service_project_state ON context_service_executions(project_id,injection_enabled);
