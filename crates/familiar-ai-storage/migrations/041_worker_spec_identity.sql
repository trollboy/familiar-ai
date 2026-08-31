-- PRD-057 is additive: historical aliases stay readable and retain the
-- identity vocabulary that was in force when they were recorded.
CREATE TABLE worker_specs (
    spec_identity TEXT PRIMARY KEY,
    worker_alias TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    runtime_id TEXT NOT NULL,
    model_state TEXT NOT NULL CHECK(model_state IN ('known','unknown','runtime-selected')),
    model_id TEXT,
    model_artifact_id TEXT,
    auth_profile_id TEXT,
    capability_profile_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    CHECK ((model_state = 'known') = (model_id IS NOT NULL)),
    CHECK (model_artifact_id IS NULL OR model_artifact_id GLOB 'sha256:*'),
    UNIQUE(provider_id,runtime_id,model_state,model_id,model_artifact_id,auth_profile_id,capability_profile_id)
);

CREATE TABLE worker_spec_versions (
    empirical_version TEXT PRIMARY KEY,
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    material_parameters_json TEXT NOT NULL,
    adapter_schema_version TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE worker_capabilities (
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    capability TEXT NOT NULL CHECK(capability IN (
      'edits-files','executes-commands','reads-repository','native-tool-calling',
      'mcp-client','structured-output','streaming','resumable-sessions',
      'context-compaction','prompt-caching','image-input','max-context',
      'reasoning-controls','sandbox-behavior','remote-or-local',
      'usage-reporting-categories','cost-reporting-mode','parallel-tool-calls',
      'deterministic-seed')),
    provenance TEXT NOT NULL CHECK(provenance IN ('declared','probed','observed','unknown')),
    recorded_at TEXT NOT NULL,
    PRIMARY KEY(spec_identity,capability)
);

ALTER TABLE worker_selections ADD COLUMN selected_spec_identity TEXT;
ALTER TABLE worker_selections ADD COLUMN selected_empirical_version TEXT;
ALTER TABLE usage_observations ADD COLUMN spec_identity TEXT;
ALTER TABLE usage_observations ADD COLUMN empirical_version TEXT;
ALTER TABLE price_schedules ADD COLUMN provider_id TEXT;
ALTER TABLE price_schedules ADD COLUMN runtime_id TEXT;
ALTER TABLE price_schedules ADD COLUMN model_or_artifact_id TEXT;
ALTER TABLE price_schedules ADD COLUMN billing_mode TEXT;

CREATE INDEX worker_specs_legacy_alias_idx ON worker_specs(provider_id,model_id);
CREATE INDEX usage_observations_spec_idx ON usage_observations(spec_identity,empirical_version,period_start);

CREATE TRIGGER worker_specs_no_update BEFORE UPDATE ON worker_specs BEGIN SELECT RAISE(ABORT, 'worker specs are immutable'); END;
CREATE TRIGGER worker_specs_no_delete BEFORE DELETE ON worker_specs BEGIN SELECT RAISE(ABORT, 'worker specs are immutable'); END;
CREATE TRIGGER worker_spec_versions_no_update BEFORE UPDATE ON worker_spec_versions BEGIN SELECT RAISE(ABORT, 'worker spec versions are immutable'); END;
CREATE TRIGGER worker_spec_versions_no_delete BEFORE DELETE ON worker_spec_versions BEGIN SELECT RAISE(ABORT, 'worker spec versions are immutable'); END;
