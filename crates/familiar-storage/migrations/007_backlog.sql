CREATE TABLE backlog_prds (
    repository_key TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    prd_number INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'completed', 'blocked')),
    discovered_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    missing_since TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (repository_key, prd_path)
);

CREATE TABLE backlog_status_events (
    event_id INTEGER PRIMARY KEY,
    repository_key TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    old_status TEXT NOT NULL CHECK (old_status IN ('pending', 'in_progress', 'completed', 'blocked')),
    new_status TEXT NOT NULL CHECK (new_status IN ('pending', 'in_progress', 'completed', 'blocked')),
    actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
    changed_at TEXT NOT NULL,
    FOREIGN KEY (repository_key, prd_path)
        REFERENCES backlog_prds(repository_key, prd_path)
);

CREATE INDEX backlog_status_events_entry
    ON backlog_status_events(repository_key, prd_path, event_id);
