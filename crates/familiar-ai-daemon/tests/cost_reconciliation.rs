//! CLI coverage for PRD-053 cost reconciliation: `billing reconcile`,
//! `accounting prd-cost`, and `stewardship reconciliation`, over fixture
//! data seeded directly through the storage repositories (matching the
//! `crates/familiar-ai-storage/src/repos/accounting.rs` unit-test fixture
//! shape). No provider is contacted; every value here is deterministic and
//! independent of wall-clock "now".

use std::{fs, process::Command};

use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};
use familiar_ai_storage::repos::billing::{BillingRepository, BillingSource, ProviderCostRow};
use familiar_ai_storage::{
    AccountingRepository, Database, ExecutionHistoryRepository, UsageObservation,
};
use serde_json::Value;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn repo_fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-053.md"), "# PRD-053\n").unwrap();

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let database = repo.path().join("state.db");
    let db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();

    let accounting = AccountingRepository::new(db.conn());
    accounting
        .register_project(
            "prj_fixturea0000001",
            "Fixture A",
            "repository",
            &identity.key,
            "test",
        )
        .unwrap();
    accounting
        .bind_provider(
            "prj_fixturea0000001",
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
            prd_path: "docs/prds/PRD-053.md".into(),
            unavailable_fields: BTreeMap::new(),
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
            uncached_input_tokens: Some(100),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            output_tokens: Some(50),
            reasoning_output_tokens: None,
            unknown_reason: None,
            period_start: "2020-01-01T10:00:00Z",
            period_end: "2020-01-01T10:00:01Z",
            terminal_status: "succeeded",
            source_event_hash: "h-exec-a",
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
    drop(db);

    (repo, database, identity.key)
}

fn run(repo: &std::path::Path, database: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(args)
        .current_dir(repo)
        .env("FAMILIAR_AI_DATABASE__PATH", database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .output()
        .unwrap()
}

fn run_json(repo: &std::path::Path, database: &std::path::Path, args: &[&str]) -> Value {
    let output = run(repo, database, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn billing_reconcile_matches_local_estimate_against_authoritative_revision() {
    let (repo, database, _key) = repo_fixture();
    let output = run(
        repo.path(),
        &database,
        &["billing", "reconcile", "org-main", "--month", "2020-01"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 new rows"), "{stdout}");
    assert!(stdout.contains("status=reconciled"), "{stdout}");
    assert!(stdout.contains("local=Some(1000000000)"), "{stdout}");
    assert!(
        stdout.contains("authoritative=Some(1000000000)"),
        "{stdout}"
    );

    // Re-running is idempotent: no new rows the second time.
    let second = run(
        repo.path(),
        &database,
        &["billing", "reconcile", "org-main", "--month", "2020-01"],
    );
    assert!(second.status.success());
    let stdout2 = String::from_utf8_lossy(&second.stdout);
    assert!(stdout2.contains("0 new rows, 1 unchanged"), "{stdout2}");
}

#[test]
fn stewardship_reconciliation_is_scoped_to_this_repository_project() {
    let (repo, database, _key) = repo_fixture();
    run(
        repo.path(),
        &database,
        &["billing", "reconcile", "org-main", "--month", "2020-01"],
    );
    let value = run_json(
        repo.path(),
        &database,
        &[
            "stewardship",
            "reconciliation",
            "2020-01-01T00:00:00Z",
            "2020-02-01T00:00:00Z",
        ],
    );
    assert_eq!(value["project_id"], "prj_fixturea0000001");
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "reconciled");
    assert_eq!(rows[0]["local_estimate_nanousd"], 1_000_000_000);
    assert_eq!(rows[0]["authoritative_nanousd"], 1_000_000_000);
    assert_eq!(rows[0]["variance_nanousd"], 0);
    assert_eq!(
        value["by_source"]["org-main"]["local_estimate_nanousd"],
        1_000_000_000
    );
    assert_eq!(
        value["by_source"]["org-main"]["authoritative_nanousd"],
        1_000_000_000
    );
}

#[test]
fn accounting_prd_cost_labels_estimated_authority_for_prd_053_scoring() {
    let (repo, database, _key) = repo_fixture();
    let value = run_json(repo.path(), &database, &["accounting", "prd-cost"]);
    let scores = value.as_array().unwrap();
    let fixture = scores
        .iter()
        .find(|s| s["worker_identity"] == "anthropic/claude")
        .unwrap();
    assert_eq!(fixture["prd"], "docs/prds/PRD-053.md");
    assert_eq!(fixture["authority"], "estimated");
    assert_eq!(fixture["completeness"], "complete");
    assert_eq!(fixture["local_estimate_nanousd"], 1_000_000_000);
}

#[test]
fn billing_reconcile_reports_unattributed_provider_spend_for_unbound_workspace() {
    let (repo, database, _key) = repo_fixture();
    // A second, unbound workspace's provider spend must show up as explicit
    // unattributed spend, never silently dropped or forced to reconcile.
    {
        let db = Database::open(&database).unwrap();
        let billing = BillingRepository::new(db.conn());
        billing
            .commit_complete(
                "org-main",
                "2020-01-01T00:00:00Z",
                "2020-01-02T00:00:00Z",
                &[ProviderCostRow {
                    bucket_start: "2020-01-01T00:00:00Z".into(),
                    bucket_end: "2020-01-02T00:00:00Z".into(),
                    workspace_id: "wrk_unbound".into(),
                    description: "usage".into(),
                    charge_class: "token-spend".into(),
                    currency: "USD".into(),
                    amount_lexical: "3.00".into(),
                    provider_payload: r#"{"workspace":"wrk_unbound","amount":"3.00"}"#.into(),
                }],
            )
            .unwrap();
    }
    let output = run(
        repo.path(),
        &database,
        &["billing", "reconcile", "org-main", "--month", "2020-01"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Unattributed spend is never an error, never dropped, and never
    // distributed into the matched project's row — it must exist as its own
    // explicit fact.
    let db = Database::open(&database).unwrap();
    let (status, project_id, authoritative): (String, Option<String>, i64) = db
        .conn()
        .query_row(
            "SELECT status,project_id,authoritative_nanousd FROM current_reconciliation WHERE match_key='unattributed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "unattributed-provider-spend");
    assert_eq!(project_id, None);
    assert_eq!(authoritative, 3_000_000_000);

    // The project-scoped stewardship view only shows this repository's
    // project row, never the unattributed spend of another workspace.
    let value = run_json(
        repo.path(),
        &database,
        &[
            "stewardship",
            "reconciliation",
            "2020-01-01T00:00:00Z",
            "2020-02-01T00:00:00Z",
        ],
    );
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "reconciled");
}
