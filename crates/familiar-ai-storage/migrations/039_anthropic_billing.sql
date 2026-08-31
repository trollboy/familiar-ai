CREATE TABLE billing_sources (
    source_name TEXT PRIMARY KEY,
    mode TEXT NOT NULL,
    organization_id TEXT NOT NULL UNIQUE,
    organization_name TEXT NOT NULL,
    credential_reference TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE billing_collection_attempts (
    attempt_id TEXT PRIMARY KEY,
    source_name TEXT NOT NULL REFERENCES billing_sources(source_name),
    window_start TEXT NOT NULL,
    window_end TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
    cursor TEXT,
    remedy TEXT,
    CHECK((status='complete' AND completed_at IS NOT NULL AND cursor IS NULL) OR status!='complete')
);

CREATE TABLE provider_cost_revisions (
    revision_id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES billing_collection_attempts(attempt_id),
    source_name TEXT NOT NULL REFERENCES billing_sources(source_name),
    logical_identity_hash TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    predecessor_revision_id TEXT REFERENCES provider_cost_revisions(revision_id),
    bucket_start TEXT NOT NULL,
    bucket_end TEXT NOT NULL,
    workspace_id TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    charge_class TEXT NOT NULL,
    currency TEXT NOT NULL,
    amount_lexical TEXT NOT NULL,
    amount_nanousd INTEGER NOT NULL,
    provider_payload TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    UNIQUE(source_name, payload_hash)
);

CREATE INDEX provider_cost_logical_revisions ON provider_cost_revisions(source_name, logical_identity_hash, observed_at, revision_id);
CREATE INDEX billing_attempt_source_window ON billing_collection_attempts(source_name, window_start, window_end, started_at);

CREATE VIEW current_provider_costs AS
SELECT row.* FROM provider_cost_revisions row
WHERE NOT EXISTS (
  SELECT 1 FROM provider_cost_revisions newer
  WHERE newer.predecessor_revision_id = row.revision_id
);

CREATE TRIGGER billing_sources_no_update BEFORE UPDATE ON billing_sources BEGIN SELECT RAISE(ABORT, 'billing sources are append-only'); END;
CREATE TRIGGER provider_cost_revisions_no_update BEFORE UPDATE ON provider_cost_revisions BEGIN SELECT RAISE(ABORT, 'provider costs are append-only'); END;
CREATE TRIGGER provider_cost_revisions_no_delete BEFORE DELETE ON provider_cost_revisions BEGIN SELECT RAISE(ABORT, 'provider costs are append-only'); END;
