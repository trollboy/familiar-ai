CREATE TABLE review_finding_waivers (
    waiver_id TEXT PRIMARY KEY,
    cycle_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    actor TEXT NOT NULL CHECK(length(trim(actor)) > 0),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    created_at TEXT NOT NULL,
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id),
    FOREIGN KEY(finding_id) REFERENCES review_findings(finding_id),
    UNIQUE(cycle_id, finding_id)
);

ALTER TABLE review_verification_evidence ADD COLUMN repository_key TEXT;
ALTER TABLE review_findings ADD COLUMN acceptance_criterion_id TEXT;
ALTER TABLE review_verification_evidence ADD COLUMN environment_identity_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE review_verification_evidence ADD COLUMN classification TEXT NOT NULL DEFAULT 'unknown';

CREATE INDEX review_verification_identity_idx
ON review_verification_evidence(repository_key, check_id, cycle_id);
