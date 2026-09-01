//! CLI coverage for `familiar-ai stewardship` (PRD-035): repository-scoped
//! reads over the backlog graph, driver sessions/attempts, and pending
//! human gates, using the same temp-repository fixture shape as the
//! existing recovery CLI tests.

use std::{fs, process::Command};

use familiar_ai_core::{BacklogDiscovery, BacklogStatusStore, FilesystemBacklogDiscovery};
use familiar_ai_storage::{Database, DriverRepository, SqliteBacklogRepository};
use serde_json::Value;
use tempfile::tempdir;

fn repo_with_backlog() -> (tempfile::TempDir, std::path::PathBuf) {
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
    drop(db);

    (repo, database)
}

fn run_json(repo: &std::path::Path, database: &std::path::Path, args: &[&str]) -> Value {
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
fn stewardship_backlog_reflects_reconciled_state() {
    let (repo, database) = repo_with_backlog();
    let value = run_json(repo.path(), &database, &["stewardship", "backlog"]);
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["prd_path"], "docs/prds/PRD-1.md");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(
        value["repository_key"],
        FilesystemBacklogDiscovery.resolve(repo.path()).unwrap().key
    );
}

#[test]
fn stewardship_sessions_attempts_and_budget_reflect_driver_records() {
    let (repo, database) = repo_with_backlog();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    {
        let db = Database::open(&database).unwrap();
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
    }

    let sessions = run_json(repo.path(), &database, &["stewardship", "sessions"]);
    let items = sessions["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["session_id"], "session-1");

    let attempts = run_json(
        repo.path(),
        &database,
        &["stewardship", "attempts", "session-1"],
    );
    assert_eq!(attempts["items"].as_array().unwrap().len(), 1);

    let budget = run_json(
        repo.path(),
        &database,
        &["stewardship", "budget", "session-1"],
    );
    assert_eq!(budget["known_cost_microusd"], 1000);
    assert_eq!(budget["known_cost_attempts"], 1);
}

#[test]
fn stewardship_gates_lists_stopped_attempt_with_recovery_command() {
    let (repo, database) = repo_with_backlog();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
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

    let gates = run_json(repo.path(), &database, &["stewardship", "gates"]);
    let items = gates["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "stopped_attempt");
    let commands = items[0]["recovery_commands"].as_array().unwrap();
    assert!(commands.iter().any(|c| c
        .as_str()
        .unwrap()
        .contains("backlog release docs/prds/PRD-1.md")));
}

#[test]
fn stewardship_session_from_another_repository_is_refused() {
    let (repo_a, database) = repo_with_backlog();
    let repo_b = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo_b.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo_b.path().join("docs/prds")).unwrap();

    let identity_a = FilesystemBacklogDiscovery.resolve(repo_a.path()).unwrap();
    {
        let db = Database::open(&database).unwrap();
        DriverRepository::new(db.conn())
            .open_session("session-a", &identity_a.key, "{}")
            .unwrap();
    }

    // Repository B's database is the same file (a real deployment would use
    // a separate database per repository, but the point under test is that
    // the *repository_key* — not merely "a database exists" — governs
    // disclosure).
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(["stewardship", "attempts", "session-a"])
        .current_dir(repo_b.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "repository B must not be able to read repository A's session"
    );
}
