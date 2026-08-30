CREATE TABLE project_config_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_key TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('approve', 'revoke')),
    actor TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_project_config_decisions_repository
ON project_config_decisions(repository_key, id DESC);
