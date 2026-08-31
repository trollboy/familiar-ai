-- PRD-055 durable project registry and provider-neutral historical series.
-- Raw facts remain the source of truth; rollups are disposable projections.
CREATE TABLE durable_projects (
    project_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    forked_from_project_id TEXT REFERENCES durable_projects(project_id),
    fork_boundary TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE project_registry_bindings (
    binding_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES durable_projects(project_id),
    evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('declaration','repository','wave-one-project','configured-path')),
    evidence_value TEXT NOT NULL,
    worktree_split TEXT NOT NULL DEFAULT '',
    actor TEXT NOT NULL,
    bound_at TEXT NOT NULL,
    UNIQUE(evidence_kind, evidence_value, worktree_split)
);

CREATE TABLE provider_attribution_bindings (
    binding_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES durable_projects(project_id),
    provider TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('organization','workspace','project','credential-profile')),
    scope_value TEXT NOT NULL,
    confidence TEXT NOT NULL CHECK(confidence IN ('declared','exact','inferred')),
    actor TEXT NOT NULL,
    bound_at TEXT NOT NULL,
    UNIQUE(provider, scope_kind, scope_value)
);

CREATE TABLE accounting_corrections (
    correction_id TEXT PRIMARY KEY,
    observation_id TEXT,
    provider_revision_id TEXT,
    correction_kind TEXT NOT NULL CHECK(correction_kind IN ('reattribution','supersession','reconciliation')),
    prior_project_id TEXT,
    project_id TEXT,
    related_fact_id TEXT,
    reason TEXT NOT NULL,
    actor TEXT NOT NULL,
    effective_at TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    CHECK(observation_id IS NOT NULL OR provider_revision_id IS NOT NULL)
);

CREATE TABLE usage_rollups (
    definition_version INTEGER NOT NULL,
    bucket_kind TEXT NOT NULL,
    bucket_start TEXT NOT NULL,
    dimensions_json TEXT NOT NULL,
    metrics_json TEXT NOT NULL,
    observation_ids_json TEXT NOT NULL,
    rebuilt_at TEXT NOT NULL,
    PRIMARY KEY(definition_version, bucket_kind, bucket_start, dimensions_json)
);

CREATE INDEX idx_usage_observations_covered ON usage_observations(period_start, period_end, project_id);
CREATE INDEX idx_project_registry_project ON project_registry_bindings(project_id, evidence_kind);
CREATE INDEX idx_provider_attribution_scope ON provider_attribution_bindings(provider, scope_kind, scope_value);
CREATE INDEX idx_accounting_corrections_observation ON accounting_corrections(observation_id, effective_at, correction_id);

CREATE TRIGGER durable_projects_no_delete BEFORE DELETE ON durable_projects BEGIN SELECT RAISE(ABORT, 'durable projects are retained'); END;
CREATE TRIGGER project_registry_bindings_no_update BEFORE UPDATE ON project_registry_bindings BEGIN SELECT RAISE(ABORT, 'project bindings are append-only'); END;
CREATE TRIGGER project_registry_bindings_no_delete BEFORE DELETE ON project_registry_bindings BEGIN SELECT RAISE(ABORT, 'project bindings are append-only'); END;
CREATE TRIGGER provider_attribution_bindings_no_update BEFORE UPDATE ON provider_attribution_bindings BEGIN SELECT RAISE(ABORT, 'attribution bindings are append-only'); END;
CREATE TRIGGER provider_attribution_bindings_no_delete BEFORE DELETE ON provider_attribution_bindings BEGIN SELECT RAISE(ABORT, 'attribution bindings are append-only'); END;
CREATE TRIGGER accounting_corrections_no_update BEFORE UPDATE ON accounting_corrections BEGIN SELECT RAISE(ABORT, 'accounting corrections are append-only'); END;
CREATE TRIGGER accounting_corrections_no_delete BEFORE DELETE ON accounting_corrections BEGIN SELECT RAISE(ABORT, 'accounting corrections are append-only'); END;
