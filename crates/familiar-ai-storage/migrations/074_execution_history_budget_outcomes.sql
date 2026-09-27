-- FAM-BUG-111: the raw-model loop (local rungs, PRD-058/063) finalizes with
-- `budget_stopped` (reservation refused or budget stop) and the run gate
-- with `budget_refused` (unenforceable cost ceiling). The session rollup has
-- counted both names since PRD-064, but the execution_history CHECK still
-- listed only migration 013's set, so the first ladder attempt on a local
-- rung died in bookkeeping as `history_failed` before its real reason could
-- be recorded.
--
-- Nine tables hold foreign keys into execution_history and the runner keeps
-- foreign_keys ON. Renaming the table would rewrite their REFERENCES clauses
-- to the new name, and dropping it while child rows exist fails the implicit
-- DELETE's immediate check. So: park the rows in a plain table, defer
-- foreign-key enforcement to commit, drop and recreate the table under its
-- own name (no child clause changes), and copy the rows back so every
-- deferred violation is cleared before the transaction commits.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE execution_history_v73_rows AS SELECT * FROM execution_history;
DROP TABLE execution_history;

CREATE TABLE execution_history (
    execution_id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    duration_ms INTEGER,
    agent TEXT NOT NULL,
    agent_version TEXT,
    model TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cached_tokens INTEGER,
    total_tokens INTEGER,
    estimated_cost_microusd INTEGER,
    input_rate_microusd_per_million INTEGER,
    cached_input_rate_microusd_per_million INTEGER,
    output_rate_microusd_per_million INTEGER,
    outcome TEXT NOT NULL CHECK(outcome IN (
        'running','succeeded','failed','signaled','launch_failed','input_failed',
        'output_failed','timed_out','budget_exceeded','malformed_output',
        'budget_stopped','budget_refused'
    )),
    exit_code INTEGER,
    signal INTEGER,
    repository TEXT NOT NULL,
    git_commit TEXT,
    worktree TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    unavailable_fields TEXT NOT NULL
);

INSERT INTO execution_history SELECT * FROM execution_history_v73_rows;
DROP TABLE execution_history_v73_rows;
CREATE INDEX idx_execution_history_recent
    ON execution_history(started_at DESC, execution_id DESC);
