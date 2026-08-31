-- PRD-032: raw observations and policy-bound decisions are immutable evidence.
CREATE TABLE probation_policies (
    policy_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(policy_id, policy_version)
);

CREATE TABLE worker_probation_observations (
    observation_id TEXT PRIMARY KEY,
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    empirical_version TEXT NOT NULL REFERENCES worker_spec_versions(empirical_version),
    execution_id TEXT REFERENCES execution_history(execution_id),
    accepted INTEGER NOT NULL CHECK(accepted IN (0,1)),
    verification_passed INTEGER NOT NULL CHECK(verification_passed IN (0,1)),
    independent_review_passed INTEGER CHECK(independent_review_passed IN (0,1)),
    remediation_required INTEGER NOT NULL CHECK(remediation_required IN (0,1)),
    failed INTEGER NOT NULL CHECK(failed IN (0,1)),
    latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0),
    evidence_json TEXT NOT NULL,
    observed_at TEXT NOT NULL
);

CREATE TABLE worker_score_snapshots (
    score_id TEXT PRIMARY KEY,
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    empirical_version TEXT NOT NULL REFERENCES worker_spec_versions(empirical_version),
    policy_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    metrics_json TEXT NOT NULL,
    score_json TEXT NOT NULL,
    observation_ids_json TEXT NOT NULL,
    cost_observation_ids_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(policy_id,policy_version) REFERENCES probation_policies(policy_id,policy_version)
);

CREATE TABLE worker_standing_events (
    event_id TEXT PRIMARY KEY,
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    empirical_version TEXT NOT NULL REFERENCES worker_spec_versions(empirical_version),
    standing TEXT NOT NULL CHECK(standing IN ('probation','promoted','quarantined','retired')),
    source TEXT NOT NULL CHECK(source IN ('policy','operator-pin','operator-quarantine','operator-retire')),
    score_id TEXT REFERENCES worker_score_snapshots(score_id),
    actor TEXT NOT NULL,
    reason TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE TABLE subscription_availability_declarations (
    declaration_id TEXT PRIMARY KEY,
    spec_identity TEXT NOT NULL REFERENCES worker_specs(spec_identity),
    empirical_version TEXT NOT NULL REFERENCES worker_spec_versions(empirical_version),
    available INTEGER NOT NULL CHECK(available IN (0,1)),
    headroom_json TEXT NOT NULL,
    actor TEXT NOT NULL,
    declared_at TEXT NOT NULL
);

CREATE INDEX worker_probation_observation_idx ON worker_probation_observations(spec_identity,empirical_version,observed_at);
CREATE INDEX worker_standing_event_idx ON worker_standing_events(spec_identity,empirical_version,occurred_at,event_id);

CREATE TRIGGER worker_probation_observations_no_update BEFORE UPDATE ON worker_probation_observations BEGIN SELECT RAISE(ABORT,'probation observations are append-only'); END;
CREATE TRIGGER worker_probation_observations_no_delete BEFORE DELETE ON worker_probation_observations BEGIN SELECT RAISE(ABORT,'probation observations are append-only'); END;
CREATE TRIGGER worker_score_snapshots_no_update BEFORE UPDATE ON worker_score_snapshots BEGIN SELECT RAISE(ABORT,'worker scores are append-only'); END;
CREATE TRIGGER worker_score_snapshots_no_delete BEFORE DELETE ON worker_score_snapshots BEGIN SELECT RAISE(ABORT,'worker scores are append-only'); END;
CREATE TRIGGER worker_standing_events_no_update BEFORE UPDATE ON worker_standing_events BEGIN SELECT RAISE(ABORT,'worker standing events are append-only'); END;
CREATE TRIGGER worker_standing_events_no_delete BEFORE DELETE ON worker_standing_events BEGIN SELECT RAISE(ABORT,'worker standing events are append-only'); END;
