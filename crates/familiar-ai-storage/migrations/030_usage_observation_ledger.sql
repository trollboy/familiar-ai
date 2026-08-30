-- PRD-051 append-only accounting facts. UPDATE/DELETE are deliberately denied
-- by triggers; reconciliation adds referencing rows instead.
-- Rebuild execution history explicitly to widen the adapter vocabulary while
-- preserving every existing column constraint and primary key.
ALTER TABLE execution_history RENAME TO execution_history_v28;
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
INSERT INTO execution_history SELECT * FROM execution_history_v28;
DROP TABLE execution_history_v28;
CREATE INDEX idx_execution_history_recent
    ON execution_history(started_at DESC, execution_id DESC);

CREATE TABLE project_identities (
    resolution_evidence TEXT PRIMARY KEY,
    project_id TEXT NOT NULL UNIQUE,
    issued_at TEXT NOT NULL
);

CREATE TABLE accounting_evidence (
    evidence_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    adapter TEXT NOT NULL,
    cli_version TEXT,
    model_identity TEXT,
    provider_session_id TEXT,
    provider_request_id TEXT,
    usage_json TEXT NOT NULL,
    provider_cost_lexical TEXT,
    observed_at TEXT NOT NULL,
    terminal_status TEXT NOT NULL,
    source_event_hash TEXT NOT NULL UNIQUE,
    CHECK(length(usage_json) <= 16384)
);

CREATE TABLE usage_observations (
    observation_id TEXT PRIMARY KEY,
    evidence_id TEXT NOT NULL REFERENCES accounting_evidence(evidence_id),
    project_id TEXT,
    degraded_identity TEXT,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    attempt_id TEXT NOT NULL,
    stage TEXT NOT NULL,
    session_id TEXT,
    worker_identity TEXT NOT NULL,
    adapter TEXT NOT NULL,
    model_identity TEXT,
    service_tier TEXT,
    provider_request_id TEXT,
    uncached_input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_output_tokens INTEGER,
    unknown_reason TEXT,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    CHECK ((project_id IS NOT NULL) != (degraded_identity IS NOT NULL)),
    CHECK (period_start <= period_end),
    CHECK (uncached_input_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL OR cache_write_tokens IS NOT NULL OR output_tokens IS NOT NULL OR reasoning_output_tokens IS NOT NULL OR unknown_reason IS NOT NULL)
);

CREATE TABLE price_schedules (
    schedule_id TEXT PRIMARY KEY,
    effective_at TEXT NOT NULL,
    currency TEXT NOT NULL CHECK(currency = 'USD'),
    calculation_version TEXT NOT NULL,
    rates_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE cost_estimates (
    estimate_id TEXT PRIMARY KEY,
    observation_id TEXT NOT NULL REFERENCES usage_observations(observation_id),
    billing_mode TEXT NOT NULL CHECK(billing_mode IN ('local-estimate','subscription-declaration','authoritative-report','external-billing')),
    provenance TEXT NOT NULL CHECK(provenance IN ('vendor-reported','configured-rate','known-zero')),
    unit TEXT NOT NULL CHECK(unit IN ('nanoUSD','provider-credit')),
    amount INTEGER,
    lexical_amount TEXT,
    unknown_reason TEXT,
    schedule_id TEXT REFERENCES price_schedules(schedule_id),
    calculation_version TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(observation_id, provenance),
    CHECK ((amount IS NOT NULL) != (unknown_reason IS NOT NULL)),
    CHECK (unit != 'nanoUSD' OR amount IS NULL OR amount >= 0),
    CHECK (provenance != 'configured-rate' OR (schedule_id IS NOT NULL AND calculation_version IS NOT NULL))
);

CREATE TABLE subscription_declarations (
    declaration_id TEXT PRIMARY KEY,
    worker_identity TEXT NOT NULL,
    available INTEGER NOT NULL CHECK(available IN (0,1)),
    price_nanousd INTEGER,
    actor TEXT NOT NULL,
    declared_at TEXT NOT NULL
);

CREATE INDEX idx_usage_observations_execution ON usage_observations(execution_id, period_start);
CREATE INDEX idx_cost_estimates_execution ON cost_estimates(observation_id, provenance);

CREATE TRIGGER usage_observations_no_update BEFORE UPDATE ON usage_observations BEGIN SELECT RAISE(ABORT, 'usage observations are append-only'); END;
CREATE TRIGGER usage_observations_no_delete BEFORE DELETE ON usage_observations BEGIN SELECT RAISE(ABORT, 'usage observations are append-only'); END;
CREATE TRIGGER accounting_evidence_no_update BEFORE UPDATE ON accounting_evidence BEGIN SELECT RAISE(ABORT, 'accounting evidence is append-only'); END;
CREATE TRIGGER accounting_evidence_no_delete BEFORE DELETE ON accounting_evidence BEGIN SELECT RAISE(ABORT, 'accounting evidence is append-only'); END;
CREATE TRIGGER cost_estimates_no_update BEFORE UPDATE ON cost_estimates BEGIN SELECT RAISE(ABORT, 'cost estimates are append-only'); END;
CREATE TRIGGER cost_estimates_no_delete BEFORE DELETE ON cost_estimates BEGIN SELECT RAISE(ABORT, 'cost estimates are append-only'); END;
