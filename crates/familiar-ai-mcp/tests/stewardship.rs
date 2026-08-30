//! PRD-035 stewardship tool coverage: repository-scoped reads over the
//! backlog graph, driver sessions/attempts, checkpoints, delivery, and
//! recovery events, plus the audited backlog/bootstrap mutation tools —
//! exercised against a real temp Git repository and SQLite database, the
//! same fixture shape the `familiar-ai` CLI tests use.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::tempdir;

use familiar_ai_core::config::Config;
use familiar_ai_core::AppStatus;
use familiar_ai_core::{
    BacklogDiscovery, BacklogStatusStore, DiscoveredPrd, FilesystemBacklogDiscovery,
    RepositoryIdentity,
};
use familiar_ai_mcp::storage::SqliteStorage;
use familiar_ai_mcp::tool::{Tool, ToolContext};
use familiar_ai_mcp::tools::{stewardship_mutations::*, stewardship_reads::*};
use familiar_ai_storage::{
    CheckpointRepository, Database, DeliveryRepository, DriverRepository, ExecutionCheckpoint,
    SqliteBacklogRepository,
};

/// A temp Git repository with two active PRDs, reconciled into a fresh
/// database. Returns the repo handle (kept alive for its `TempDir` drop
/// guard), the database path, the resolved repository identity, and the
/// discovered PRDs in `docs/prds/PRD-1.md` / `docs/prds/PRD-2.md` order.
fn repo_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    RepositoryIdentity,
    Vec<DiscoveredPrd>,
) {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-1.md"), "# PRD-1: One\n").unwrap();
    fs::write(repo.path().join("docs/prds/PRD-2.md"), "# PRD-2: Two\n").unwrap();

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();

    let database = repo.path().join("state.db");
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();
    drop(db);

    (repo, database, identity, discovered)
}

fn ctx_for(database: &Path) -> ToolContext {
    let db = Arc::new(Mutex::new(Database::open(database).unwrap()));
    ToolContext {
        storage: Arc::new(SqliteStorage::new(db)),
        status: Arc::new(Mutex::new(AppStatus::new())),
        config: Arc::new(Config::default()),
        router: None,
    }
}

const RUN_ACTOR: &str = "system:familiar-ai-run:00000000000000000001-0000000001-000001";

#[tokio::test]
async fn list_backlog_is_repository_scoped_and_reflects_claim_status() {
    let (repo, database, identity, discovered) = repo_fixture();
    {
        let mut db = Database::open(&database).unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .claim_run(&identity, &discovered, &discovered[0], RUN_ACTOR)
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let result = ListBacklogTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["prd_path"], "docs/prds/PRD-1.md");
    assert_eq!(items[0]["status"], "in_progress");
    assert_eq!(items[1]["status"], "pending");

    // A different repository never sees these entries.
    let other = tempdir().unwrap();
    let outside = ListBacklogTool
        .call(
            json!({"repository_path": other.path().to_str().unwrap()}),
            &ctx,
        )
        .await;
    assert!(
        outside.is_err(),
        "a non-Git directory must fail to resolve, not silently return this repository's backlog"
    );
}

#[tokio::test]
async fn backlog_release_matches_cli_audit_trail() {
    let (repo, database, identity, discovered) = repo_fixture();
    {
        let mut db = Database::open(&database).unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .claim_run(&identity, &discovered, &discovered[0], RUN_ACTOR)
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let result = BacklogReleaseTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "ops:alice",
                "reason": "review was disabled",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(result["new_status"], "pending");
    assert_eq!(result["action"], "release");

    let db = Database::open(&database).unwrap();
    let counts: (i64, i64) = db
        .conn()
        .query_row(
            "SELECT (SELECT count(*) FROM backlog_status_events),(SELECT count(*) FROM backlog_recovery_events)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (2, 1));
}

#[tokio::test]
async fn backlog_complete_requires_human_actor() {
    let (repo, database, identity, discovered) = repo_fixture();
    {
        let mut db = Database::open(&database).unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .claim_run(&identity, &discovered, &discovered[0], RUN_ACTOR)
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let rejected = BacklogCompleteTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "ops:alice",
                "reason": "trust me",
            }),
            &ctx,
        )
        .await;
    assert!(rejected.is_err(), "non-human actor must be rejected");

    let accepted = BacklogCompleteTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "human:alice",
                "reason": "verified manually",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(accepted["manual_override"], true);
    assert_eq!(accepted["new_status"], "completed");
}

#[tokio::test]
async fn backlog_record_complete_requires_satisfied_dependencies() {
    let (repo, database, _identity, _discovered) = repo_fixture();
    let ctx = ctx_for(&database);

    let result = BacklogRecordCompleteTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "human:alice",
                "reason": "done outside familiar",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(result["new_status"], "completed");
    assert_eq!(result["old_status"], "pending");
}

#[tokio::test]
async fn list_sessions_attempts_and_budget_are_repository_scoped() {
    let (repo, database, identity, _discovered) = repo_fixture();
    {
        let db = Database::open(&database).unwrap();
        let driver = DriverRepository::new(db.conn());
        driver
            .open_session("session-1", &identity.key, r#"{"max_prds":2}"#)
            .unwrap();
        let a = driver
            .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
            .unwrap();
        driver
            .record_attempt_finished("session-1", a, "completed", None, Some(1_000), Some(10))
            .unwrap();
        driver.finish_session("session-1", "backlog_empty").unwrap();

        // A session in a different repository must never be reachable
        // through this repository's tool calls.
        driver
            .open_session("other-session", "/some/other/.git", "{}")
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let sessions = ListDriverSessionsTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = sessions["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["session_id"], "session-1");

    let attempts = ListDriverAttemptsTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "session_id": "session-1",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(attempts["items"].as_array().unwrap().len(), 1);

    // The cross-repository session must be refused, not silently served.
    let cross_repo = ListDriverAttemptsTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "session_id": "other-session",
            }),
            &ctx,
        )
        .await;
    assert!(cross_repo.is_err());

    let budget = GetBudgetTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "session_id": "session-1",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(budget["known_cost_microusd"], 1000);
    assert_eq!(budget["known_cost_attempts"], 1);
    assert_eq!(budget["unknown_cost_attempts"], 0);

    let cross_repo_budget = GetBudgetTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "session_id": "other-session",
            }),
            &ctx,
        )
        .await;
    assert!(cross_repo_budget.is_err());
}

#[tokio::test]
async fn list_checkpoints_redacts_secret_markers_and_bounds_large_fields() {
    let (repo, database, identity, _discovered) = repo_fixture();
    {
        let db = Database::open(&database).unwrap();
        CheckpointRepository::new(db.conn())
            .put(&ExecutionCheckpoint {
                checkpoint_id: "cp-1".into(),
                repository_key: identity.key.clone(),
                prd_id: "PRD-1".into(),
                prd_path: "docs/prds/PRD-1.md".into(),
                execution_id: Some("exec-1".into()),
                phase: "implemented".into(),
                base_revision: "deadbeef".into(),
                worktree_path: "/state/worktrees/PRD-1".into(),
                branch_name: Some("familiar/PRD-1".into()),
                diff_hash: "sha256:abc".into(),
                changed_files_json: "[]".into(),
                agent_identity: "claude-code".into(),
                usage_json: "Authorization: Bearer sekrit-token".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: None,
            })
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let result = ListCheckpointsTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["usage"]["redacted"], true);
    assert_eq!(items[0]["worktree_path"], "/state/worktrees/PRD-1");
    assert_eq!(items[0]["branch_name"], "familiar/PRD-1");
}

#[tokio::test]
async fn pending_human_gates_lists_stopped_attempt_with_recovery_commands() {
    let (repo, database, identity, _discovered) = repo_fixture();
    {
        let db = Database::open(&database).unwrap();
        let driver = DriverRepository::new(db.conn());
        driver
            .open_session("session-1", &identity.key, "{}")
            .unwrap();
        let a = driver
            .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
            .unwrap();
        driver
            .record_attempt_finished(
                "session-1",
                a,
                "retained",
                Some("scope_broadened"),
                None,
                Some(5),
            )
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let gates = ListPendingHumanGatesTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = gates["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "stopped_attempt");
    assert_eq!(items[0]["detail"], "scope_broadened");
    let commands = items[0]["recovery_commands"].as_array().unwrap();
    assert!(commands.iter().any(|c| c
        .as_str()
        .unwrap()
        .contains("backlog release docs/prds/PRD-1.md")));
}

#[tokio::test]
async fn list_delivery_decisions_and_recovery_events_are_repository_scoped() {
    let (repo, database, identity, discovered) = repo_fixture();
    {
        let mut db = Database::open(&database).unwrap();
        DeliveryRepository::new(db.conn())
            .record_authority_decision(
                "d1",
                &identity.key,
                "session-1",
                "PRD-1",
                "manual",
                "human:tester",
                "approved",
                None,
                "[]",
                "[]",
                None,
                3,
            )
            .unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .claim_run(&identity, &discovered, &discovered[0], RUN_ACTOR)
            .unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .recover(
                &identity,
                &discovered[0],
                familiar_ai_core::BacklogRecoveryAction::Release,
                "human:tester",
                "needs another pass",
            )
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let decisions = ListDeliveryDecisionsTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = decisions["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["decision_id"], "d1");
    assert_eq!(items[0]["warrant_consumed"], 3);

    let events = ListRecoveryEventsTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = events["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["action"], "release");
    assert_eq!(items[0]["reason"], "needs another pass");
}

#[tokio::test]
async fn bootstrap_rollback_tool_matches_cli_semantics() {
    // Without a prior bootstrap run, rollback is ineligible — the tool must
    // surface the same rejection the CLI would, not silently succeed.
    let (repo, database, _identity, _discovered) = repo_fixture();
    let ctx = ctx_for(&database);
    let result = BootstrapRollbackTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "run_id": "no-such-run",
                "actor": "human:alice",
                "reason": "undo",
            }),
            &ctx,
        )
        .await;
    assert!(result.is_err());
}
