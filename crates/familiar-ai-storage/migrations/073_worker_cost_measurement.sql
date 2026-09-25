CREATE TABLE worker_cost_bases (
    basis_id TEXT PRIMARY KEY,
    worker_identity TEXT NOT NULL,
    basis_kind TEXT NOT NULL CHECK (basis_kind IN ('published-token-rates','operator-declared','measured-accepted-executions','local')),
    estimate_microusd INTEGER,
    unit TEXT NOT NULL,
    provenance_json TEXT NOT NULL CHECK (json_valid(provenance_json)),
    recorded_at TEXT NOT NULL
);

CREATE INDEX worker_cost_bases_worker_idx
ON worker_cost_bases(worker_identity, recorded_at, basis_id);

CREATE TABLE worker_cost_supersessions (
    supersession_id TEXT PRIMARY KEY,
    worker_identity TEXT NOT NULL,
    declared_basis_id TEXT NOT NULL REFERENCES worker_cost_bases(basis_id),
    measured_basis_id TEXT NOT NULL REFERENCES worker_cost_bases(basis_id),
    accepted_executions INTEGER NOT NULL,
    known_cost_executions INTEGER NOT NULL,
    unknown_cost_executions INTEGER NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE TRIGGER worker_cost_bases_immutable_update
BEFORE UPDATE ON worker_cost_bases BEGIN SELECT RAISE(ABORT, 'worker cost bases are immutable'); END;
CREATE TRIGGER worker_cost_bases_immutable_delete
BEFORE DELETE ON worker_cost_bases BEGIN SELECT RAISE(ABORT, 'worker cost bases are immutable'); END;
CREATE TRIGGER worker_cost_supersessions_immutable_update
BEFORE UPDATE ON worker_cost_supersessions BEGIN SELECT RAISE(ABORT, 'worker cost supersessions are immutable'); END;
CREATE TRIGGER worker_cost_supersessions_immutable_delete
BEFORE DELETE ON worker_cost_supersessions BEGIN SELECT RAISE(ABORT, 'worker cost supersessions are immutable'); END;

ALTER TABLE worker_selections ADD COLUMN cost_decision_reason TEXT
CHECK (cost_decision_reason IS NULL OR cost_decision_reason IN ('capability','declared-risk-tier','probation','explicit-pin','cost-could-not-rank','cost-ranked-and-lost'));
