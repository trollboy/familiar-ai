CREATE TABLE execution_checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    repository_key TEXT NOT NULL,
    prd_id TEXT NOT NULL,
    prd_path TEXT NOT NULL,
    execution_id TEXT NULL,
    phase TEXT NOT NULL CHECK (phase IN (
        'claimed','implemented','implemented_pending_review','verified','reviewed',
        'approved','integrated','completed','blocked','invalid_checkpoint'
    )),
    base_revision TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch_name TEXT NULL,
    diff_hash TEXT NOT NULL,
    changed_files_json TEXT NOT NULL,
    agent_identity TEXT NOT NULL,
    usage_json TEXT NOT NULL,
    test_evidence_json TEXT NOT NULL,
    invalid_reason TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(repository_key, prd_id)
);

CREATE INDEX execution_checkpoints_resume_idx
    ON execution_checkpoints(repository_key, phase, prd_id);

CREATE TABLE execution_checkpoint_events (
    event_id TEXT PRIMARY KEY,
    checkpoint_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    prior_phase TEXT NULL,
    resulting_phase TEXT NOT NULL,
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    FOREIGN KEY(checkpoint_id) REFERENCES execution_checkpoints(checkpoint_id)
);

