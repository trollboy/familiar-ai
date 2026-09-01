//! PRD-035 acceptance criterion 5: "MCP, CLI, and dashboard agree on shared
//! fixture state." The dashboard's `/stewardship/*` handlers are thin
//! wrappers around `familiar_ai_daemon::stewardship::*` (see
//! `src/dashboard.rs`); this test seeds one fixture database and proves the
//! `familiar-ai stewardship` CLI subcommand's JSON output matches calling
//! those exact same library functions directly — i.e. the CLI and the
//! dashboard boundary are reading identical facts from identical state.
//! (MCP tool coverage over the same facts lives in
//! `crates/familiar-ai-mcp/tests/stewardship.rs`.)

use std::{fs, process::Command};

use familiar_ai_core::{BacklogDiscovery, BacklogStatusStore, FilesystemBacklogDiscovery};
use familiar_ai_storage::{
    Database, DeliveryRepository, DriverRepository, SqliteBacklogRepository,
};
use serde_json::Value;
use tempfile::tempdir;

fn seeded_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    familiar_ai_core::RepositoryIdentity,
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

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();

    let database = repo.path().join("state.db");
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();

    let driver = DriverRepository::new(db.conn());
    driver
        .open_session("session-1", &identity.key, r#"{"max_prds":1}"#)
        .unwrap();
    let a = driver
        .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
        .unwrap();
    driver
        .record_attempt_finished("session-1", a, "completed", None, Some(1_000), Some(10))
        .unwrap();
    driver.finish_session("session-1", "backlog_empty").unwrap();

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
            2,
        )
        .unwrap();

    drop(db);
    (repo, database, identity)
}

fn cli_json(repo: &std::path::Path, database: &std::path::Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(args)
        .current_dir(repo)
        .env("FAMILIAR_AI_DATABASE__PATH", database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_and_dashboard_boundary_agree_on_backlog() {
    let (repo, database, identity) = seeded_fixture();
    let cli = cli_json(repo.path(), &database, &["stewardship", "backlog"]);

    let db = Database::open(&database).unwrap();
    let direct =
        familiar_ai_daemon::stewardship::list_backlog(&db, &identity, None, None, 20).unwrap();

    assert_eq!(cli, direct);
}

#[test]
fn cli_and_dashboard_boundary_agree_on_sessions_and_attempts() {
    let (repo, database, identity) = seeded_fixture();
    let db = Database::open(&database).unwrap();

    let cli_sessions = cli_json(repo.path(), &database, &["stewardship", "sessions"]);
    let direct_sessions =
        familiar_ai_daemon::stewardship::list_sessions(&db, &identity, None, 20).unwrap();
    assert_eq!(cli_sessions, direct_sessions);

    let cli_attempts = cli_json(
        repo.path(),
        &database,
        &["stewardship", "attempts", "session-1"],
    );
    let direct_attempts =
        familiar_ai_daemon::stewardship::list_attempts(&db, &identity, "session-1", None, 20)
            .unwrap();
    assert_eq!(cli_attempts, direct_attempts);
}

#[test]
fn cli_and_dashboard_boundary_agree_on_budget_and_delivery() {
    let (repo, database, identity) = seeded_fixture();
    let db = Database::open(&database).unwrap();

    let cli_budget = cli_json(
        repo.path(),
        &database,
        &["stewardship", "budget", "session-1"],
    );
    let direct_budget =
        familiar_ai_daemon::stewardship::get_budget(&db, &identity, "session-1").unwrap();
    assert_eq!(cli_budget, direct_budget);
    assert_eq!(cli_budget["known_cost_microusd"], 1000);
    assert_eq!(cli_budget["delivery_warrant_consumed"], 2);

    let cli_delivery = cli_json(repo.path(), &database, &["stewardship", "delivery"]);
    let direct_delivery =
        familiar_ai_daemon::stewardship::list_delivery_decisions(&db, &identity, None, 20).unwrap();
    assert_eq!(cli_delivery, direct_delivery);
}
