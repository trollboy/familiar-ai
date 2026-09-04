-- PRD-071: durable batch-tier review lifecycle. One row per review attempt
-- submitted to a provider batch interface, keyed by review_id so a re-driven
-- review cycle never resubmits a still-pending or already-resolved attempt.
-- The daemon owns transitions across restarts: submitted -> completed ->
-- applied (disposition consumed the result exactly once), or
-- submitted -> expired_fallback (bounded wait exceeded; interactive review
-- decided instead, with the reason recorded).
CREATE TABLE batch_reviews (
    batch_review_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL UNIQUE,
    cycle_id TEXT NOT NULL,
    repository_key TEXT NOT NULL,
    prd_id TEXT NOT NULL,
    risk_class TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_batch_id TEXT NOT NULL,
    provider_request_id TEXT,
    state TEXT NOT NULL CHECK(state IN ('submitted','completed','applied','expired_fallback')),
    max_wait_ms INTEGER NOT NULL CHECK(max_wait_ms > 0),
    submitted_at TEXT NOT NULL,
    deadline_at TEXT NOT NULL,
    polled_at TEXT,
    completed_at TEXT,
    result_json TEXT,
    provider_cost_lexical TEXT,
    fallback_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_batch_reviews_state ON batch_reviews(state);
CREATE INDEX idx_batch_reviews_repository ON batch_reviews(repository_key, prd_id);
