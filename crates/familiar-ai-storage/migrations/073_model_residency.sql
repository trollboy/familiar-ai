-- PRD-073 warm local model residency. The PRD-056 daemon holds configured
-- PRD-062 model artifacts loaded between executions; every lifecycle
-- transition it makes is a durable, append-only fact here. Degradation to
-- per-call loading is always one of these rows, never a silent tax.

CREATE TABLE model_residency_events (
    event_id TEXT PRIMARY KEY,
    -- The `model_residency.residents` configuration key.
    resident_key TEXT NOT NULL,
    -- The `worker_registry.workers` key this resident serves.
    worker_identity TEXT NOT NULL,
    -- The PRD-062 artifact held resident. Residency never loads a model
    -- outside the artifact registry, so this is never null.
    model_artifact_id TEXT NOT NULL,
    runtime_id TEXT NOT NULL,
    -- Identity of one resident server instance. A restart mints a new
    -- identity: the process that served call N is not the process that
    -- served call N+1, and the ledger must be able to tell them apart.
    server_identity TEXT NOT NULL,
    event TEXT NOT NULL CHECK(event IN (
        'loaded','stopped','evicted','restarted','failed','health-failed'
    )),
    -- A named reason, required for every event that takes capacity away or
    -- reports trouble. Only 'loaded' carries none: this table records what
    -- the daemon did and what went wrong, never a row per healthy interval.
    reason TEXT,
    -- 1-based attempt number on a 'restarted' row; null elsewhere.
    restart_attempt INTEGER,
    -- The resident's declared footprint at the time of the event, when the
    -- operator declared one. Never an invented measurement.
    memory_mb INTEGER,
    recorded_at TEXT NOT NULL,
    CHECK (event NOT IN ('stopped','evicted','restarted','failed','health-failed')
           OR (reason IS NOT NULL AND length(trim(reason)) > 0)),
    CHECK ((event = 'restarted') = (restart_attempt IS NOT NULL))
);

CREATE INDEX idx_model_residency_events_resident
    ON model_residency_events(resident_key, recorded_at);
CREATE INDEX idx_model_residency_events_server
    ON model_residency_events(server_identity, recorded_at);

CREATE TRIGGER model_residency_events_no_update BEFORE UPDATE ON model_residency_events
BEGIN SELECT RAISE(ABORT, 'model residency events are append-only'); END;
CREATE TRIGGER model_residency_events_no_delete BEFORE DELETE ON model_residency_events
BEGIN SELECT RAISE(ABORT, 'model residency events are append-only'); END;

-- Residency attribution for PRD-063 local telemetry: latency, load time and
-- utilization partition by whether the serving process was already warm.
-- Existing rows keep NULL — honestly unknown, never backfilled as 'cold'.
ALTER TABLE local_worker_telemetry ADD COLUMN residency_state TEXT
    CHECK(residency_state IS NULL OR residency_state IN ('cold','warm'));
ALTER TABLE local_worker_telemetry ADD COLUMN resident_server_identity TEXT;
ALTER TABLE local_worker_telemetry ADD COLUMN cache_evidence TEXT
    CHECK(cache_evidence IS NULL OR cache_evidence IN
        ('cold-load','warm-miss','warm-hit','unknown'));

-- Residency attribution for the PRD-051 ledger. A separate append-only
-- table rather than three perpetually-null columns on `usage_observations`:
-- residency is a local-execution-only fact, and the ledger's own insert is
-- shared by every hosted adapter in the workspace. Exactly like
-- `cost_estimates`, this attaches to an observation by id.
CREATE TABLE usage_observation_residency (
    observation_id TEXT PRIMARY KEY REFERENCES usage_observations(observation_id),
    residency_state TEXT NOT NULL CHECK(residency_state IN ('cold','warm')),
    -- Null exactly when no resident server served the call (a cold,
    -- per-call load); non-null names the process that did.
    resident_server_identity TEXT,
    -- What the serving runtime actually reported about its prefix cache.
    -- 'unknown' is the honest record for a runtime that reports nothing —
    -- cache behavior is never inferred from residency alone.
    cache_evidence TEXT NOT NULL CHECK(cache_evidence IN
        ('cold-load','warm-miss','warm-hit','unknown')),
    cache_evidence_reason TEXT,
    recorded_at TEXT NOT NULL,
    CHECK (residency_state = 'cold' OR resident_server_identity IS NOT NULL),
    CHECK (cache_evidence <> 'unknown'
           OR (cache_evidence_reason IS NOT NULL AND length(trim(cache_evidence_reason)) > 0))
);

CREATE TRIGGER usage_observation_residency_no_update BEFORE UPDATE ON usage_observation_residency
BEGIN SELECT RAISE(ABORT, 'usage observation residency is append-only'); END;
CREATE TRIGGER usage_observation_residency_no_delete BEFORE DELETE ON usage_observation_residency
BEGIN SELECT RAISE(ABORT, 'usage observation residency is append-only'); END;
