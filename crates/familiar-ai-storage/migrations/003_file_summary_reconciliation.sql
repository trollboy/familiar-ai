CREATE TABLE file_summary_reconciliation_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('in_progress', 'completed', 'failed', 'interrupted')),
    total_examined INTEGER NOT NULL,
    canonical_unchanged INTEGER NOT NULL,
    converted INTEGER NOT NULL,
    conflicts INTEGER NOT NULL,
    unresolved INTEGER NOT NULL,
    previously_reconciled INTEGER NOT NULL,
    failed INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT NOT NULL
);

CREATE INDEX idx_file_summary_reconciliation_runs_project
    ON file_summary_reconciliation_runs(project_id, id);

CREATE TABLE file_summary_reconciliation_run_reasons (
    run_id INTEGER NOT NULL REFERENCES file_summary_reconciliation_runs(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (run_id, reason)
);

CREATE TABLE file_summary_reconciliation_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES file_summary_reconciliation_runs(id) ON DELETE RESTRICT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    original_file_summary_id INTEGER NOT NULL,
    classification TEXT NOT NULL CHECK (classification IN ('converted', 'conflict', 'unresolved')),
    unresolved_reason TEXT CHECK (unresolved_reason IS NULL OR unresolved_reason IN (
        'missing_project',
        'non_absolute_noncanonical',
        'registered_root_mismatch',
        'lexical_or_traversal_failure',
        'empty_relative_identity',
        'lossless_representation_failure',
        'unsupported_host_path_form',
        'internal_persistence_or_validation_failure'
    )),
    mapped_canonical_path TEXT,
    resulting_active_id INTEGER,
    original_path TEXT NOT NULL,
    original_summary TEXT NOT NULL,
    original_tags_json TEXT NOT NULL,
    original_extracted_symbols_json TEXT,
    original_last_known_mtime INTEGER,
    original_last_known_size INTEGER,
    original_last_updated_at TEXT NOT NULL,
    original_created_at TEXT NOT NULL,
    original_updated_at TEXT NOT NULL,
    reconciled_at TEXT NOT NULL,
    rolled_back_at TEXT,
    CHECK ((classification = 'unresolved') = (unresolved_reason IS NOT NULL)),
    CHECK ((classification = 'unresolved') OR mapped_canonical_path IS NOT NULL)
);

CREATE INDEX idx_file_summary_reconciliation_records_project
    ON file_summary_reconciliation_records(project_id, id);

CREATE UNIQUE INDEX idx_file_summary_reconciliation_records_active_original
    ON file_summary_reconciliation_records(
        project_id,
        original_file_summary_id,
        original_path,
        original_updated_at
    )
    WHERE rolled_back_at IS NULL;

CREATE TABLE file_summary_reconciliation_rollbacks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES file_summary_reconciliation_runs(id) ON DELETE RESTRICT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'conflict')),
    conflict_reason TEXT,
    recorded_at TEXT NOT NULL
);

CREATE INDEX idx_file_summary_reconciliation_rollbacks_run
    ON file_summary_reconciliation_rollbacks(run_id, id);
