CREATE TABLE planner_batches (
    batch_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('approved','rejected')),
    actor TEXT NOT NULL,
    reason TEXT,
    file_hashes_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE INDEX idx_planner_batches_repository
    ON planner_batches(repository_key, recorded_at DESC);
