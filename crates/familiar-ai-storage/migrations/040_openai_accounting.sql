-- PRD-054 OpenAI accounting. Provider facts and collection attempts are
-- append-only; current values are a projection over superseding revisions.
CREATE TABLE openai_billing_sources (
    source_id TEXT PRIMARY KEY,
    provider_name TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    project_id TEXT,
    admin_auth_env TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(organization_id, project_id),
    CHECK(admin_auth_env NOT LIKE '%=%')
);

CREATE TABLE openai_collection_attempts (
    attempt_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES openai_billing_sources(source_id),
    window_start INTEGER NOT NULL,
    window_end INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
    remedy TEXT,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(source_id, window_start, window_end, started_at),
    CHECK(window_start < window_end),
    CHECK((status = 'failed') = (remedy IS NOT NULL))
);

CREATE TABLE openai_cost_revisions (
    revision_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES openai_billing_sources(source_id),
    organization_id TEXT NOT NULL,
    project_id TEXT,
    bucket_start INTEGER NOT NULL,
    bucket_end INTEGER NOT NULL,
    line_item TEXT NOT NULL,
    classification TEXT NOT NULL,
    raw_amount_lexical TEXT NOT NULL,
    currency TEXT NOT NULL,
    amount_nanousd INTEGER,
    normalization_error TEXT,
    payload_hash TEXT NOT NULL,
    supersedes_revision_id TEXT REFERENCES openai_cost_revisions(revision_id),
    collected_at TEXT NOT NULL,
    UNIQUE(source_id, organization_id, project_id, bucket_start, bucket_end, line_item, payload_hash),
    CHECK(bucket_start < bucket_end),
    CHECK((currency = 'usd' AND amount_nanousd IS NOT NULL AND normalization_error IS NULL)
       OR (currency != 'usd' AND amount_nanousd IS NULL AND normalization_error IS NOT NULL))
);

CREATE TABLE credit_schedules (
    schedule_id TEXT PRIMARY KEY,
    source_url TEXT NOT NULL,
    effective_at TEXT NOT NULL,
    calculation_version TEXT NOT NULL,
    rates_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE credit_estimates (
    estimate_id TEXT PRIMARY KEY,
    observation_id TEXT NOT NULL REFERENCES usage_observations(observation_id),
    schedule_id TEXT NOT NULL REFERENCES credit_schedules(schedule_id),
    amount_micocredits INTEGER NOT NULL CHECK(amount_micocredits >= 0),
    calculation_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(observation_id, schedule_id)
);

CREATE TRIGGER openai_cost_revisions_no_update BEFORE UPDATE ON openai_cost_revisions BEGIN SELECT RAISE(ABORT, 'OpenAI cost revisions are append-only'); END;
CREATE TRIGGER openai_cost_revisions_no_delete BEFORE DELETE ON openai_cost_revisions BEGIN SELECT RAISE(ABORT, 'OpenAI cost revisions are append-only'); END;
CREATE TRIGGER openai_collection_attempts_no_update BEFORE UPDATE ON openai_collection_attempts BEGIN SELECT RAISE(ABORT, 'OpenAI collection attempts are append-only'); END;
CREATE TRIGGER openai_collection_attempts_no_delete BEFORE DELETE ON openai_collection_attempts BEGIN SELECT RAISE(ABORT, 'OpenAI collection attempts are append-only'); END;
