-- PRD-053: cost reconciliation between PRD-051 local estimates and PRD-052
-- authoritative provider revisions. Reconciliation rows are new append-only
-- facts; raw provider revisions (039/040) and local cost_estimates (030) are
-- never edited, overwritten, or deleted.
CREATE TABLE reconciliation_runs (
    run_id TEXT PRIMARY KEY,
    billing_source TEXT NOT NULL,
    window_start TEXT NOT NULL,
    window_end TEXT NOT NULL,
    invoked_by TEXT NOT NULL CHECK(invoked_by IN ('collect','explicit')),
    tolerance_nanousd INTEGER NOT NULL,
    settlement_horizon_days INTEGER NOT NULL,
    actor TEXT NOT NULL,
    started_at TEXT NOT NULL,
    now_reference TEXT NOT NULL
);

-- One row per (billing_source, day, match_key) reconciliation fact. A
-- superseding provider revision or a newly-arrived local estimate re-opens
-- the window by appending a new row with `supersedes_row_id` pointing at the
-- prior current-effective row for that key; the prior row remains as
-- history. `match_key` is `project:<durable-project-id>` for a resolved
-- Familiar project or the literal `unattributed` for provider spend that
-- cannot be traced to a project.
CREATE TABLE reconciliation_rows (
    row_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES reconciliation_runs(run_id),
    billing_source TEXT NOT NULL,
    day_start TEXT NOT NULL,
    day_end TEXT NOT NULL,
    match_key TEXT NOT NULL,
    project_id TEXT,
    status TEXT NOT NULL CHECK(status IN ('reconciled','reconciled-with-variance','pending','mismatch','unattributed-provider-spend')),
    local_estimate_nanousd INTEGER,
    authoritative_nanousd INTEGER,
    variance_nanousd INTEGER,
    tolerance_nanousd INTEGER NOT NULL,
    provider_revision_ids TEXT NOT NULL DEFAULT '[]',
    observation_ids TEXT NOT NULL DEFAULT '[]',
    reservation_evidence_count INTEGER NOT NULL DEFAULT 0,
    reservation_evidence_nanousd INTEGER,
    supersedes_row_id TEXT REFERENCES reconciliation_rows(row_id),
    created_at TEXT NOT NULL
);

CREATE INDEX idx_reconciliation_rows_key ON reconciliation_rows(billing_source, day_start, match_key, created_at);
CREATE INDEX idx_reconciliation_rows_run ON reconciliation_rows(run_id);
CREATE INDEX idx_reconciliation_rows_project ON reconciliation_rows(project_id, day_start);

-- The current-effective projection: the reconciliation fact for a
-- (billing_source, day, match_key) with no newer superseding row.
CREATE VIEW current_reconciliation AS
SELECT row.* FROM reconciliation_rows row
WHERE NOT EXISTS (
  SELECT 1 FROM reconciliation_rows newer
  WHERE newer.supersedes_row_id = row.row_id
);

CREATE TRIGGER reconciliation_runs_no_update BEFORE UPDATE ON reconciliation_runs BEGIN SELECT RAISE(ABORT, 'reconciliation runs are append-only'); END;
CREATE TRIGGER reconciliation_runs_no_delete BEFORE DELETE ON reconciliation_runs BEGIN SELECT RAISE(ABORT, 'reconciliation runs are append-only'); END;
CREATE TRIGGER reconciliation_rows_no_update BEFORE UPDATE ON reconciliation_rows BEGIN SELECT RAISE(ABORT, 'reconciliation rows are append-only'); END;
CREATE TRIGGER reconciliation_rows_no_delete BEFORE DELETE ON reconciliation_rows BEGIN SELECT RAISE(ABORT, 'reconciliation rows are append-only'); END;
