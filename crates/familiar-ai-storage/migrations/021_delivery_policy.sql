CREATE TABLE delivery_authority_decisions (
    decision_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    session_id TEXT NOT NULL,
    prd_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    actor TEXT NOT NULL,
    decision TEXT NOT NULL,
    assurance_label TEXT NULL,
    findings_json TEXT NOT NULL,
    stop_reasons_json TEXT NOT NULL,
    warrant_json TEXT NULL,
    warrant_consumed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX delivery_authority_subject_idx
ON delivery_authority_decisions(repository_key, session_id, prd_id, decision);

CREATE TABLE delivery_external_effects (
    effect_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    session_id TEXT NOT NULL,
    prd_id TEXT NOT NULL,
    effect_kind TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK(status IN ('intent','succeeded','failed')),
    external_reference TEXT NULL,
    detail TEXT NULL,
    updated_at TEXT NOT NULL
);

