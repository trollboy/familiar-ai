use familiar_ai_storage::{
    CheckpointRepository, Database, DeliveryRepository, ExecutionCheckpoint,
};

fn checkpoint() -> ExecutionCheckpoint {
    ExecutionCheckpoint {
        checkpoint_id: "cp-security".into(),
        repository_key: "/repo/.git".into(),
        prd_id: "PRD-037".into(),
        prd_path: "docs/prds/PRD-037.md".into(),
        execution_id: Some("exec-security".into()),
        phase: "implemented".into(),
        base_revision: "deadbeef".into(),
        worktree_path: "/tmp/worktree".into(),
        branch_name: Some("familiar/security".into()),
        diff_hash: "sha256:fixture".into(),
        changed_files_json: "[]".into(),
        agent_identity: "fixture".into(),
        usage_json: r#"{"status":"unknown"}"#.into(),
        test_evidence_json: r#"{"status":"unknown"}"#.into(),
        invalid_reason: None,
    }
}

fn database() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

#[test]
fn checkpoint_replay_is_idempotent() {
    let db = database();
    let repository = CheckpointRepository::new(db.conn());
    repository.put(&checkpoint()).unwrap();
    repository
        .transition("cp-security", "verified", "checks_passed")
        .unwrap();
    repository
        .transition("cp-security", "verified", "checks_passed")
        .unwrap();
    assert_eq!(repository.events("cp-security").unwrap().len(), 2);
}

#[test]
fn failed_phase_transaction_cannot_fabricate_completion() {
    let db = database();
    let repository = CheckpointRepository::new(db.conn());
    repository.put(&checkpoint()).unwrap();
    db.conn()
        .execute_batch("DROP TABLE execution_checkpoint_events;")
        .unwrap();
    assert!(repository
        .transition("cp-security", "completed", "injected_disk_fault")
        .is_err());
    assert_eq!(
        repository
            .get("/repo/.git", "PRD-037")
            .unwrap()
            .unwrap()
            .phase,
        "implemented"
    );
}

#[test]
fn external_effect_intent_is_idempotent() {
    let db = database();
    let repository = DeliveryRepository::new(db.conn());
    repository
        .begin_effect(
            "effect-1",
            "/repo/.git",
            "session",
            "PRD-037",
            "publish",
            "stable-key",
        )
        .unwrap();
    repository
        .begin_effect(
            "attacker-id",
            "/other/.git",
            "other",
            "PRD-X",
            "publish",
            "stable-key",
        )
        .unwrap();
    let effect = repository.effect("stable-key").unwrap().unwrap();
    assert_eq!(effect.status, "intent");
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM delivery_external_effects",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn corrupt_database_is_reported_not_reinitialized() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    std::fs::write(&path, b"not a sqlite database").unwrap();
    let result = Database::open(&path).and_then(|db| db.run_migrations());
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a sqlite database");
}
