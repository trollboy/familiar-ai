CREATE TABLE worker_selections (
    selection_id TEXT PRIMARY KEY,
    execution_id TEXT NULL,
    stage TEXT NOT NULL,
    rule TEXT NOT NULL,
    selected_identity TEXT NOT NULL,
    candidates_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE INDEX worker_selections_execution_idx ON worker_selections(execution_id, stage);
