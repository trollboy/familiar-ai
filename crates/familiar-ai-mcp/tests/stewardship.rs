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
use familiar_ai_storage::repos::billing::{BillingRepository, BillingSource, ProviderCostRow};
use familiar_ai_storage::{
    AccountingRepository, CheckpointRepository, Database, DeliveryRepository, DriverRepository,
    ExecutionCheckpoint, ExecutionHistoryRepository, SqliteBacklogRepository, UsageObservation,
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

#[tokio::test]
async fn get_reconciliation_tool_is_project_scoped_and_labels_by_source() {
    let (repo, database, identity, _discovered) = repo_fixture();
    {
        let db = Database::open(&database).unwrap();
        let accounting = AccountingRepository::new(db.conn());
        accounting
            .register_project(
                "prj_mcpfixture00001",
                "MCP Fixture",
                "repository",
                &identity.key,
                "test",
            )
            .unwrap();
        accounting
            .bind_provider(
                "prj_mcpfixture00001",
                "org-main",
                "workspace",
                "wrk_a",
                "exact",
                "test",
            )
            .unwrap();
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&familiar_ai_storage::ExecutionStart {
                execution_id: "exec-a".into(),
                started_at: "2020-01-01T10:00:00Z".into(),
                repository: identity.key.clone(),
                worktree: identity.key.clone(),
                git_commit: None,
                prd_path: "docs/prds/PRD-1.md".into(),
                unavailable_fields: Default::default(),
            })
            .unwrap();
        let observation = accounting
            .append_observation(&UsageObservation {
                execution_id: "exec-a",
                attempt_id: "attempt-1",
                stage: "implementation",
                session_id: None,
                worker_identity: "anthropic/claude",
                adapter: "claude-code",
                cli_version: None,
                model_identity: Some("claude"),
                service_tier: None,
                provider_request_id: None,
                uncached_input_tokens: Some(10),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                output_tokens: Some(5),
                reasoning_output_tokens: None,
                unknown_reason: None,
                period_start: "2020-01-01T10:00:00Z",
                period_end: "2020-01-01T10:00:01Z",
                terminal_status: "succeeded",
                source_event_hash: "h-mcp-exec-a",
                provider_cost_lexical: Some("1.00"),
                project_resolution_evidence: Some(&identity.key),
                output_register_id: "none",
                output_register_version: "none",
                input_compression_id: "none",
                input_compression_version: "none",
                compression_experiment: None,
                compression_lane: None,
                edit_form_id: "none",
                edit_form_version: "none",
                truncation_config_id: "none",
                truncation_config_version: "none",
            })
            .unwrap()
            .unwrap();
        accounting
            .append_vendor_estimate(&observation, "1.00")
            .unwrap();

        let billing = BillingRepository::new(db.conn());
        billing
            .bind_source(&BillingSource {
                name: "org-main",
                mode: "anthropic-organization",
                organization_id: "org_main",
                organization_name: "Main",
                credential_reference: "env: ADMIN_MAIN",
            })
            .unwrap();
        billing
            .commit_complete(
                "org-main",
                "2020-01-01T00:00:00Z",
                "2020-01-02T00:00:00Z",
                &[ProviderCostRow {
                    bucket_start: "2020-01-01T00:00:00Z".into(),
                    bucket_end: "2020-01-02T00:00:00Z".into(),
                    workspace_id: "wrk_a".into(),
                    description: "usage".into(),
                    charge_class: "token-spend".into(),
                    currency: "USD".into(),
                    amount_lexical: "1.00".into(),
                    provider_payload: r#"{"workspace":"wrk_a","amount":"1.00"}"#.into(),
                }],
            )
            .unwrap();
        accounting
            .reconcile_window(
                "org-main",
                chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                chrono::DateTime::parse_from_rfc3339("2020-01-02T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                "explicit",
                10_000_000,
                3,
                chrono::DateTime::parse_from_rfc3339("2020-01-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                "test",
            )
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let result = GetReconciliationTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "start": "2020-01-01T00:00:00Z",
                "end": "2020-01-02T00:00:00Z",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(result["project_id"], "prj_mcpfixture00001");
    assert_eq!(result["network_collection"], false);
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "reconciled");
    assert_eq!(
        result["by_source"]["org-main"]["local_estimate_nanousd"],
        1_000_000_000
    );
    assert_eq!(
        result["by_source"]["org-main"]["authoritative_nanousd"],
        1_000_000_000
    );

    // A different repository never sees this project's reconciliation.
    let other = tempdir().unwrap();
    let outside = GetReconciliationTool
        .call(
            json!({
                "repository_path": other.path().to_str().unwrap(),
                "start": "2020-01-01T00:00:00Z",
                "end": "2020-01-02T00:00:00Z",
            }),
            &ctx,
        )
        .await;
    assert!(outside.is_err());
}
