//! PRD-109: one derived PRD lifecycle, shown on the operator surfaces.
//!
//! Seeds a repository with three PRD files and one driver session, then
//! asserts `stewardship::list_backlog` carries the lifecycle each derivation
//! row predicts: a `draft` file is Draft whatever the ledger says, a retained
//! attempt on a human-gate reason is AwaitingFeedback, a retained attempt on a
//! defect reason is Failed, and an archived file is Completed.

use std::{fs, process::Command};

use familiar_ai_core::{BacklogDiscovery, BacklogStatusStore, FilesystemBacklogDiscovery};
use familiar_ai_storage::{Database, DriverRepository, SqliteBacklogRepository};
use serde_json::Value;
use tempfile::tempdir;

fn prd(id: u32, status: &str) -> String {
    format!(
        "---\nfamiliar_ai_prd: 1\nid: PRD-{id:03}\nstatus: {status}\ndependencies: []\nexpected_files:\n  - src/lib.rs\nacceptance_criteria:\n  - it works\nrisk_classes:\n  - persistence\nexternal_gates: []\n---\n# PRD-{id:03}: Fixture {id}\n"
    )
}

fn lifecycle_of(backlog: &Value, path: &str) -> (String, String) {
    let item = backlog["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["prd_path"] == path)
        .unwrap_or_else(|| panic!("no backlog item for {path}"));
    (
        item["status"].as_str().unwrap().to_string(),
        item["lifecycle"].as_str().unwrap().to_string(),
    )
}

#[test]
fn the_backlog_surface_carries_the_derived_lifecycle_beside_the_raw_status() {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-001.md"), prd(1, "ready")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-002.md"), prd(2, "ready")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-003.md"), prd(3, "draft")).unwrap();
    fs::write(
        repo.path().join("docs/prds/done/PRD-004.md"),
        prd(4, "ready"),
    )
    .unwrap();

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let layout = familiar_ai_core::BacklogLayout {
        risk_vocabulary: vec!["persistence".into()],
        ..Default::default()
    };
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&identity, &layout)
        .unwrap();
    let database = repo.path().join("state.db");
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();

    // One session: PRD-1 retained on a human decision, PRD-2 retained on a
    // defect. Both rows stay `pending` in the ledger; only the lifecycle
    // tells them apart.
    let driver = DriverRepository::new(db.conn());
    driver
        .open_session("s1", &identity.key, r#"{"max_prds":2}"#)
        .unwrap();
    let a = driver
        .record_attempt_started("s1", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished("s1", a, "retained", Some("scope_ambiguous"), None, Some(5))
        .unwrap();
    let b = driver
        .record_attempt_started("s1", "PRD-2", "docs/prds/PRD-002.md", None)
        .unwrap();
    driver
        .record_attempt_finished(
            "s1",
            b,
            "retained",
            Some("verification_failed"),
            None,
            Some(5),
        )
        .unwrap();
    driver
        .finish_session("s1", "budget_prds_exhausted")
        .unwrap();

    let backlog = familiar_ai_daemon::stewardship::list_backlog(
        &db,
        &identity,
        Some(&layout),
        None,
        None,
        100,
    )
    .unwrap();

    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/PRD-001.md"),
        ("pending".to_string(), "awaiting_feedback".to_string()),
        "a scope decision is the owner's to make"
    );
    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/PRD-002.md"),
        ("pending".to_string(), "failed".to_string()),
        "a verification failure is a defect, not a decision"
    );
    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/PRD-003.md"),
        ("pending".to_string(), "draft".to_string()),
        "the file's draft marker wins over the pending row"
    );
    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/done/PRD-004.md"),
        ("completed".to_string(), "completed".to_string()),
        "location is truth for completion"
    );
    // The stop left a blocked checkpoint behind, the thing the progress
    // strip draws as "— blocked".
    familiar_ai_storage::CheckpointRepository::new(db.conn())
        .put(&familiar_ai_storage::ExecutionCheckpoint {
            checkpoint_id: "cp-1".into(),
            repository_key: identity.key.clone(),
            prd_id: "PRD-1".into(),
            prd_path: "docs/prds/PRD-001.md".into(),
            execution_id: Some("exec-1".into()),
            phase: "blocked".into(),
            base_revision: "deadbeef".into(),
            worktree_path: "/tmp/does-not-matter".into(),
            branch_name: None,
            diff_hash: "sha256:candidate".into(),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        })
        .unwrap();
    let listed = |db: &Database| -> Vec<String> {
        familiar_ai_daemon::stewardship::list_checkpoints(db, &identity, None, 50).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["checkpoint_id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(listed(&db), ["cp-1"], "before the release the stop is live");

    // FAM-BUG-089: the owner releases PRD-1's scope stop. The retained
    // attempt is still the latest row, but it is history now, and every
    // surface must say Ready again — the gates list already did, the
    // lifecycle did not.
    let target = discovered
        .iter()
        .find(|prd| prd.path.as_str() == "docs/prds/PRD-001.md")
        .unwrap()
        .clone();
    // The run had claimed it (pending -> in_progress) before retaining;
    // release audits that claim, so it has to look like a real run's.
    SqliteBacklogRepository::new(db.conn_mut())
        .claim_run(
            &identity,
            &discovered,
            &target,
            "system:familiar-ai-run:00001788597053154009-0003279353-000001",
        )
        .unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .recover(
            &identity,
            &target,
            familiar_ai_core::BacklogRecoveryAction::Release,
            "human:trollboy",
            "scope stop superseded by the rewritten PRD",
        )
        .unwrap();
    let backlog = familiar_ai_daemon::stewardship::list_backlog(
        &db,
        &identity,
        Some(&layout),
        None,
        None,
        100,
    )
    .unwrap();
    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/PRD-001.md"),
        ("pending".to_string(), "ready".to_string()),
        "a released stop is history, not a decision still owed"
    );
    assert_eq!(
        lifecycle_of(&backlog, "docs/prds/PRD-002.md"),
        ("pending".to_string(), "failed".to_string()),
        "the release of PRD-1 changes nothing for PRD-2"
    );
    assert!(
        listed(&db).is_empty(),
        "a released stop's checkpoint is history and leaves the progress strip"
    );

    let raw_present = backlog["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item.get("status").is_some() && item.get("prd_id").is_some());
    assert!(raw_present, "the raw inputs stay beside the derived state");
}

/// Every retained reason the stall taxonomy names resolves to exactly one of
/// AwaitingFeedback or Failed — never silently to something else — and only
/// the owner-decision classes resolve to AwaitingFeedback.
#[test]
fn every_stall_class_is_a_human_gate_or_a_failure_and_nothing_else() {
    use familiar_ai_core::{derive_lifecycle, AttemptFacts, LifecycleInputs, PrdLifecycle};
    let human: Vec<&str> = familiar_ai_storage::stall_taxonomy_classes()
        .into_iter()
        .filter(|class| {
            let derived = derive_lifecycle(&LifecycleInputs {
                latest_attempt: Some(AttemptFacts {
                    outcome: Some("retained".into()),
                    retained_reason: Some((*class).to_string()),
                    last_durable_phase: None,
                }),
                ..Default::default()
            });
            match derived.lifecycle {
                PrdLifecycle::AwaitingFeedback => true,
                PrdLifecycle::Failed => false,
                other => panic!("stall class {class} derived {other:?}, which is neither"),
            }
        })
        .collect();
    assert_eq!(
        human,
        vec![
            "scope_broadened",
            "scope_ambiguous",
            "human_review_required"
        ],
        "the set of owner-decision stall classes is closed"
    );
}
