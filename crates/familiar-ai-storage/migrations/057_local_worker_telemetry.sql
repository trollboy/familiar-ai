-- PRD-063 local worker telemetry. Local inference has no provider invoice;
-- these rows carry typed, timestamped observations against the full spec
-- identity and never a USD cost. Operator-configured allocation estimates
-- (electricity, amortized hardware, hosted local servers) are a distinct,
-- explicitly opt-in class that never mixes with provider invoices without
-- explicit grouping — kept in dedicated tables, never mingled with the
-- PRD-051 `usage_observations`/`cost_estimates` ledger.

CREATE TABLE local_worker_telemetry (
    telemetry_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    attempt_id TEXT NOT NULL,
    stage TEXT NOT NULL,
    spec_identity TEXT NOT NULL,
    empirical_version TEXT NOT NULL,
    worker_identity TEXT NOT NULL,
    runtime_id TEXT NOT NULL,
    model_artifact_id TEXT,
    artifact_verification_state TEXT NOT NULL CHECK(artifact_verification_state IN ('verified','degraded-unverified','mismatch')),
    uncached_input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_output_tokens INTEGER,
    wall_time_ms INTEGER,
    time_to_first_token_ms INTEGER,
    tokens_per_second REAL,
    load_time_ms INTEGER,
    peak_memory_mb INTEGER,
    accelerator_utilization_pct REAL,
    cpu_utilization_pct REAL,
    retries INTEGER NOT NULL DEFAULT 0,
    failure_kind TEXT,
    energy_wh REAL,
    energy_measurement_provenance TEXT,
    recorded_at TEXT NOT NULL,
    CHECK (energy_wh IS NULL OR energy_measurement_provenance IS NOT NULL)
);

CREATE INDEX idx_local_worker_telemetry_execution ON local_worker_telemetry(execution_id, attempt_id);
CREATE INDEX idx_local_worker_telemetry_spec ON local_worker_telemetry(spec_identity, recorded_at);

-- Disabled-by-default operator allocation policy. `enabled` must be
-- explicitly flipped on; a policy row existing is never itself an
-- activation.
CREATE TABLE local_allocation_policies (
    policy_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('electricity','amortized-hardware','hosted-local-server')),
    currency TEXT NOT NULL,
    declared_assumptions_json TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
    created_at TEXT NOT NULL,
    PRIMARY KEY(policy_id, policy_version)
);

-- A `local-estimate`-shaped, but never provider-mixed, allocation cost.
-- `cost_category` and `estimated_authority_label` are closed to a single
-- honest value each: this is never a subscription declaration and never an
-- authoritative figure.
CREATE TABLE local_allocation_estimates (
    estimate_id TEXT PRIMARY KEY,
    telemetry_id TEXT NOT NULL REFERENCES local_worker_telemetry(telemetry_id),
    policy_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    cost_category TEXT NOT NULL CHECK(cost_category = 'operator-allocation'),
    amount_nanocurrency INTEGER NOT NULL,
    currency TEXT NOT NULL,
    effective_period_start TEXT NOT NULL,
    effective_period_end TEXT NOT NULL,
    input_measurements_json TEXT NOT NULL,
    declared_assumptions_json TEXT NOT NULL,
    provenance TEXT NOT NULL,
    estimated_authority_label TEXT NOT NULL CHECK(estimated_authority_label = 'estimated'),
    created_at TEXT NOT NULL,
    FOREIGN KEY(policy_id, policy_version) REFERENCES local_allocation_policies(policy_id, policy_version),
    CHECK (effective_period_start <= effective_period_end)
);

CREATE INDEX idx_local_allocation_estimates_telemetry ON local_allocation_estimates(telemetry_id);

CREATE TRIGGER local_worker_telemetry_no_update BEFORE UPDATE ON local_worker_telemetry BEGIN SELECT RAISE(ABORT, 'local worker telemetry is append-only'); END;
CREATE TRIGGER local_worker_telemetry_no_delete BEFORE DELETE ON local_worker_telemetry BEGIN SELECT RAISE(ABORT, 'local worker telemetry is append-only'); END;
CREATE TRIGGER local_allocation_estimates_no_update BEFORE UPDATE ON local_allocation_estimates BEGIN SELECT RAISE(ABORT, 'local allocation estimates are append-only'); END;
CREATE TRIGGER local_allocation_estimates_no_delete BEFORE DELETE ON local_allocation_estimates BEGIN SELECT RAISE(ABORT, 'local allocation estimates are append-only'); END;
