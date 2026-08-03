CREATE TABLE backlog_bootstrap_runs (
    run_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    canonical_manifest_hash TEXT NOT NULL CHECK(length(canonical_manifest_hash) = 64),
    raw_manifest_hash TEXT NOT NULL CHECK(length(raw_manifest_hash) = 64),
    manifest_path TEXT NOT NULL,
    manifest_version INTEGER NOT NULL CHECK(manifest_version = 1),
    status TEXT NOT NULL CHECK(status IN ('applied', 'rolled_back', 'applying', 'failed', 'interrupted')),
    item_count INTEGER NOT NULL CHECK(item_count > 0),
    applied_at TEXT NULL,
    rolled_back_at TEXT NULL,
    rollback_run_id TEXT NULL UNIQUE,
    reapplies_run_id TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(reapplies_run_id) REFERENCES backlog_bootstrap_runs(run_id)
);

CREATE TABLE backlog_bootstrap_items (
    run_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal > 0),
    repository_key TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    prd_number INTEGER NOT NULL,
    declared_content_hash TEXT NOT NULL CHECK(length(declared_content_hash) = 64),
    observed_content_hash TEXT NOT NULL CHECK(length(observed_content_hash) = 64),
    old_status TEXT NOT NULL CHECK(old_status = 'pending'),
    new_status TEXT NOT NULL CHECK(new_status = 'completed'),
    status_event_id INTEGER NOT NULL UNIQUE,
    PRIMARY KEY(run_id, ordinal),
    UNIQUE(run_id, prd_path),
    FOREIGN KEY(run_id) REFERENCES backlog_bootstrap_runs(run_id),
    FOREIGN KEY(repository_key, prd_path) REFERENCES backlog_prds(repository_key, prd_path),
    FOREIGN KEY(status_event_id) REFERENCES backlog_status_events(event_id)
);

CREATE TABLE backlog_bootstrap_rollbacks (
    rollback_run_id TEXT PRIMARY KEY,
    bootstrap_run_id TEXT NOT NULL UNIQUE,
    repository_key TEXT NOT NULL,
    actor TEXT NOT NULL CHECK(length(trim(actor)) > 0),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    item_count INTEGER NOT NULL CHECK(item_count > 0),
    created_at TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    FOREIGN KEY(bootstrap_run_id) REFERENCES backlog_bootstrap_runs(run_id)
);

CREATE TABLE backlog_bootstrap_rollback_items (
    rollback_run_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal > 0),
    prd_path TEXT NOT NULL,
    old_status TEXT NOT NULL CHECK(old_status = 'completed'),
    restored_status TEXT NOT NULL CHECK(restored_status = 'pending'),
    status_event_id INTEGER NOT NULL UNIQUE,
    PRIMARY KEY(rollback_run_id, ordinal),
    FOREIGN KEY(rollback_run_id) REFERENCES backlog_bootstrap_rollbacks(rollback_run_id),
    FOREIGN KEY(status_event_id) REFERENCES backlog_status_events(event_id)
);

CREATE INDEX backlog_bootstrap_runs_repository ON backlog_bootstrap_runs(repository_key, created_at, run_id);
CREATE INDEX backlog_bootstrap_items_repository_path ON backlog_bootstrap_items(repository_key, prd_path);
