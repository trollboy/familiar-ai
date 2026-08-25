ALTER TABLE execution_history RENAME TO execution_history_v12;

CREATE TABLE execution_history (
    execution_id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    duration_ms INTEGER,
    agent TEXT NOT NULL CHECK(agent = 'codex'),
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
        'output_failed','timed_out','budget_exceeded','malformed_output'
    )),
    exit_code INTEGER,
    signal INTEGER,
    repository TEXT NOT NULL,
    git_commit TEXT,
    worktree TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    unavailable_fields TEXT NOT NULL
);

INSERT INTO execution_history SELECT * FROM execution_history_v12;
DROP TABLE execution_history_v12;
CREATE INDEX idx_execution_history_recent
    ON execution_history(started_at DESC, execution_id DESC);
