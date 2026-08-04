CREATE TABLE repository_observation_orders (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    last_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE repository_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    observation_order INTEGER NOT NULL,
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('created','modified','removed','renamed_from','renamed_to','ambiguous')),
    source TEXT NOT NULL CHECK(source IN ('watcher','scan','recovery')),
    related_path TEXT,
    scan_run_id INTEGER,
    outcome TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(project_id, observation_order)
);
CREATE INDEX idx_repository_observations_path ON repository_observations(project_id, path, observation_order);

CREATE TABLE pending_summary_work (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('create','modify')),
    status TEXT NOT NULL CHECK(status IN ('pending','leased','completed','superseded','failed','interrupted')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    observation_order INTEGER NOT NULL,
    dispatch_deferred INTEGER NOT NULL DEFAULT 0,
    source_mtime INTEGER,
    source_size INTEGER,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(project_id, path)
);
CREATE INDEX idx_pending_summary_status ON pending_summary_work(project_id, status, path);

CREATE TABLE file_summary_lifecycle_tombstones (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    original_file_summary_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    summary TEXT NOT NULL,
    tags_json TEXT NOT NULL,
    extracted_symbols_json TEXT,
    last_known_mtime INTEGER,
    last_known_size INTEGER,
    last_updated_at TEXT NOT NULL,
    original_created_at TEXT NOT NULL,
    original_updated_at TEXT NOT NULL,
    reason TEXT NOT NULL CHECK(reason IN ('deleted','modified','renamed','ineligible')),
    cause_source TEXT NOT NULL,
    cause_id INTEGER,
    related_path TEXT,
    observation_order INTEGER NOT NULL,
    provenance TEXT NOT NULL DEFAULT 'pre_event_lifecycle_evidence',
    retired_at TEXT NOT NULL,
    UNIQUE(project_id, original_file_summary_id, original_updated_at)
);
CREATE INDEX idx_lifecycle_tombstones_project ON file_summary_lifecycle_tombstones(project_id, path, id);

CREATE TABLE repository_scan_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    root TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    start_observation_order INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','incomplete','interrupted','failed','enumeration_complete','reconciliation_complete','blocked','superseded')),
    enumeration_status TEXT NOT NULL,
    reconciliation_status TEXT NOT NULL,
    progress_path TEXT,
    visited_count INTEGER NOT NULL DEFAULT 0,
    eligible_count INTEGER NOT NULL DEFAULT 0,
    excluded_count INTEGER NOT NULL DEFAULT 0,
    rejected_count INTEGER NOT NULL DEFAULT 0,
    failed_count INTEGER NOT NULL DEFAULT 0,
    staged_count INTEGER NOT NULL DEFAULT 0,
    unchanged_count INTEGER NOT NULL DEFAULT 0,
    pending_create_count INTEGER NOT NULL DEFAULT 0,
    pending_modify_count INTEGER NOT NULL DEFAULT 0,
    retired_delete_count INTEGER NOT NULL DEFAULT 0,
    retired_rename_count INTEGER NOT NULL DEFAULT 0,
    retired_ineligible_count INTEGER NOT NULL DEFAULT 0,
    later_watcher_wins_count INTEGER NOT NULL DEFAULT 0,
    previously_applied_count INTEGER NOT NULL DEFAULT 0,
    absence_permitted INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX idx_repository_scan_runs_project ON repository_scan_runs(project_id, id);

CREATE TABLE repository_scan_entries (
    scan_run_id INTEGER NOT NULL REFERENCES repository_scan_runs(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    classification TEXT NOT NULL CHECK(classification IN ('eligible','excluded','rejected','failed')),
    detail TEXT,
    PRIMARY KEY(scan_run_id, path)
);
