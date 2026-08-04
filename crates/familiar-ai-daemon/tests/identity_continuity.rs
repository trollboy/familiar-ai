//! End-to-end identity-migration continuity: state seeded under the legacy
//! `familiar` paths is atomically migrated by the new binary at startup, with
//! prior records readable unchanged; ambiguity and stale environment fail
//! closed. Uses the `history` command, which needs no git repository.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use familiar_ai_storage::{Database, ExecutionHistoryRepository, ExecutionStart};

fn seeded_home() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    // identity-gate: allow — legacy layout is the test fixture.
    let old_data = home.join(".local/share/familiar");
    let old_config = home.join(".config/familiar"); // identity-gate: allow
    let old_state = home.join(".local/state/familiar"); // identity-gate: allow
    std::fs::create_dir_all(&old_data).unwrap();
    std::fs::create_dir_all(&old_config).unwrap();
    std::fs::create_dir_all(&old_state).unwrap();
    let db = Database::open(&old_data.join("familiar.db")).unwrap();
    db.run_migrations().unwrap();
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: "continuity-1".into(),
            started_at: "2026-08-01T00:00:00Z".into(),
            repository: "/repo/.git".into(),
            worktree: "/repo".into(),
            git_commit: Some("abc123".into()),
            prd_path: "docs/prds/PRD-777.md".into(),
            unavailable_fields: BTreeMap::new(),
        })
        .unwrap();
    temp
}

fn binary(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
    command.env_clear();
    command.env("PATH", std::env::var("PATH").unwrap_or_default());
    command.env("HOME", home);
    command.arg("history");
    command
}

#[test]
fn legacy_state_migrates_atomically_and_history_reads_unchanged() {
    let temp = seeded_home();
    let home = temp.path();
    let database_before = std::fs::read(home.join(".local/share/familiar/familiar.db")).unwrap(); // identity-gate: allow

    let output = binary(home).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "history failed: stderr={stderr} stdout={stdout}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("identity migration: "))
            .count(),
        3,
        "one audit line per migrated persistent directory: {stderr}"
    );
    assert!(stdout.contains("docs/prds/PRD-777.md"), "got: {stdout}");
    assert!(stdout.contains("continuity-1"));
    // Old locations are gone; the database moved byte-identically.
    assert!(!home.join(".local/share/familiar").exists()); // identity-gate: allow
    let database_after = std::fs::read(home.join(".local/share/familiar-ai/familiar.db")).unwrap();
    assert_eq!(database_before, database_after);

    // Second run: already migrated, no further audit lines, same output.
    let second = binary(home).output().unwrap();
    assert!(second.status.success());
    let second_err = String::from_utf8_lossy(&second.stderr);
    assert!(!second_err.contains("identity migration: "), "{second_err}");
    assert!(String::from_utf8_lossy(&second.stdout).contains("continuity-1"));
}

#[test]
fn both_present_fails_closed_naming_both_paths() {
    let temp = seeded_home();
    let home = temp.path();
    std::fs::create_dir_all(home.join(".local/share/familiar-ai")).unwrap();

    let output = binary(home).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("both legacy and new state directories exist"));
    assert!(stderr.contains(".local/share/familiar"), "{stderr}"); // identity-gate: allow
    assert!(stderr.contains(".local/share/familiar-ai"), "{stderr}");
    // Nothing was moved.
    assert!(home
        .join(".local/share/familiar/familiar.db") // identity-gate: allow
        .exists());
}

#[test]
fn stale_legacy_environment_fails_closed_naming_the_variable() {
    let temp = tempfile::tempdir().unwrap();
    let output = binary(temp.path())
        .env("FAMILIAR_DATABASE__PATH", "/tmp/nowhere.db") // identity-gate: allow
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FAMILIAR_DATABASE__PATH"), "{stderr}"); // identity-gate: allow
    assert!(stderr.contains("FAMILIAR_AI_"), "{stderr}");
}
