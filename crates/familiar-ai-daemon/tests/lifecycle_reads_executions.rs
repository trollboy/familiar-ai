//! FAM-BUG-099: a PRD claimed by a desktop-launched run that has since
//! failed must not read as Implementing forever. `run` records no driver
//! attempt, so the control-plane execution is the durable record, and the
//! lifecycle reads it when no attempt speaks for a claimed row.

use std::{
    fs,
    process::Command,
    sync::{Arc, Mutex},
};

use familiar_ai_core::control_plane::{
    Authority, CapabilityScope, ClientClass, ExecutionMode, ExecutionState, SchedulingPolicy,
    Submission,
};
use familiar_ai_core::{BacklogDiscovery, BacklogStatusStore, FilesystemBacklogDiscovery};
use familiar_ai_daemon::control_plane::ControlPlaneService;
use familiar_ai_storage::{Database, SqliteBacklogRepository};
use tempfile::tempdir;

#[test]
fn a_claimed_prd_whose_run_failed_reads_failed_with_the_reason() {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "---\nfamiliar_ai_prd: 1\nid: PRD-001\nstatus: ready\ndependencies: []\nexpected_files:\n  - src/lib.rs\nacceptance_criteria:\n  - it works\nrisk_classes:\n  - persistence\nexternal_gates: []\n---\n# PRD-001: One\n\n## Objective\nx\n",
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
    let mut db = Database::open(&repo.path().join("state.db")).unwrap();
    db.run_migrations().unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();
    // The run claimed it: pending -> in_progress.
    SqliteBacklogRepository::new(db.conn_mut())
        .claim_run(
            &identity,
            &discovered,
            &discovered[0],
            "system:familiar-ai-run:00001788597053154009-0003279353-000001",
        )
        .unwrap();

    // ...and the execution that did the claiming failed with a recorded reason.
    let project = identity.worktree.to_string_lossy().into_owned();
    let db = Arc::new(Mutex::new(db));
    let control = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
    control
        .register_project(&project, &project, 0, None)
        .unwrap();
    let scope = CapabilityScope {
        client_class: ClientClass::Operator,
        project_id: Some(project.clone()),
        execution_id: None,
        attempt: None,
        worker_id: None,
        authorities: vec![Authority::Control, Authority::Observe],
    };
    control
        .submit(
            &scope,
            &Submission {
                execution_id: "exec-1".into(),
                project_id: project.clone(),
                idempotency_key: "k1".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: serde_json::json!({"argv": ["familiar-ai", "drive", "--prd", "PRD-001", "--max-prds", "1"]}).to_string(),
            },
        )
        .unwrap();
    assert_eq!(control.claim_next().unwrap().as_deref(), Some("exec-1"));
    control
        .finish(
            "exec-1",
            ExecutionState::Failed,
            "worker_failed exit=1: error: FAMILIAR-INCOMPLETE: criterion 7 not met",
        )
        .unwrap();

    let guard = db.lock().unwrap();
    let backlog = familiar_ai_daemon::stewardship::list_backlog(
        &guard,
        &identity,
        Some(&layout),
        None,
        None,
        50,
    )
    .unwrap();
    let item = backlog["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["prd_path"] == "docs/prds/PRD-001.md")
        .unwrap();
    assert_eq!(
        item["status"], "in_progress",
        "the claim itself is untouched"
    );
    assert_eq!(item["lifecycle"], "failed", "{item}");
    let divergence = item["lifecycle_divergence"].as_str().unwrap();
    assert!(divergence.contains("exec-1"), "{divergence}");
    assert!(divergence.contains("FAMILIAR-INCOMPLETE"), "{divergence}");
}
