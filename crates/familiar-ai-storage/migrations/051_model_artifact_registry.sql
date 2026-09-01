CREATE TABLE model_artifacts (
    model_artifact_id TEXT PRIMARY KEY CHECK(model_artifact_id GLOB 'sha256:*'),
    verification_state TEXT NOT NULL CHECK(verification_state IN ('verified','degraded-unverified-alias')),
    manifest_json TEXT,
    provenance_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    CHECK((verification_state='verified' AND manifest_json IS NOT NULL) OR
          (verification_state='degraded-unverified-alias' AND manifest_json IS NULL))
);

CREATE TABLE model_artifact_aliases (
    alias TEXT PRIMARY KEY,
    model_artifact_id TEXT NOT NULL REFERENCES model_artifacts(model_artifact_id),
    created_at TEXT NOT NULL
);

CREATE INDEX model_artifact_alias_identity ON model_artifact_aliases(model_artifact_id);

-- PRD-057 worker identities already provide a stable SHA-256 partition key.
-- Reuse that key only for the explicitly degraded alias identity: it does not
-- claim a content digest and can be replaced only by registering new content.
INSERT OR IGNORE INTO model_artifacts
    (model_artifact_id, verification_state, manifest_json, provenance_json, created_at)
SELECT replace(spec_identity, 'wspec-sha256:', 'sha256:'),
       'degraded-unverified-alias', NULL, '{}', datetime('now')
FROM worker_specs
WHERE runtime_id = 'ollama' AND model_artifact_id IS NULL;

INSERT OR IGNORE INTO model_artifact_aliases(alias, model_artifact_id, created_at)
SELECT worker_alias, replace(spec_identity, 'wspec-sha256:', 'sha256:'), datetime('now')
FROM worker_specs
WHERE runtime_id = 'ollama' AND model_artifact_id IS NULL;

UPDATE worker_specs
SET model_artifact_id = replace(spec_identity, 'wspec-sha256:', 'sha256:')
WHERE runtime_id = 'ollama' AND model_artifact_id IS NULL;
