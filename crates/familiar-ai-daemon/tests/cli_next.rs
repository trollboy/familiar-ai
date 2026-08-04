use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn git_repo() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success());
    fs::create_dir_all(temp.path().join("docs/prds")).unwrap();
    temp
}

fn next_command(repo: &tempfile::TempDir, database: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
    command
        .current_dir(repo.path())
        .arg("next")
        .env("FAMILIAR_AI_DATABASE__PATH", database);
    command
}

#[test]
fn next_is_stable_read_only_and_prints_exact_line() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    fs::write(
        repo.path().join("docs/prds/PRD-010.md"),
        "# PRD-010: Later\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-009.md"),
        "# PRD-009: Deterministic Backlog Manager\n",
    )
    .unwrap();

    for _ in 0..2 {
        let output = next_command(&repo, &database).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            b"PRD-9\tdocs/prds/PRD-009.md\tpending\tDeterministic Backlog Manager\n"
        );
        assert!(output.stderr.is_empty());
    }
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let events: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM backlog_status_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let executions: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM execution_history", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((events, executions), (0, 0));
}

#[test]
fn next_reports_categorized_backlog_failures_on_stderr_only() {
    let repo = git_repo();
    let database = repo.path().join("familiar.db");
    let empty = next_command(&repo, &database).output().unwrap();
    assert!(!empty.status.success());
    assert!(empty.stdout.is_empty());
    assert_eq!(
        String::from_utf8(empty.stderr).unwrap(),
        "error: backlog is empty\n"
    );

    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-002: Wrong\n",
    )
    .unwrap();
    let malformed = next_command(&repo, &database).output().unwrap();
    assert!(!malformed.status.success());
    assert!(malformed.stdout.is_empty());
    let stderr = String::from_utf8(malformed.stderr).unwrap();
    assert!(
        stderr.starts_with("error: backlog discovery failed: malformed PRD docs/prds/PRD-001.md:"),
        "{stderr}"
    );
}

#[test]
fn next_fails_outside_a_git_repository() {
    let temp = tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .current_dir(temp.path())
        .arg("next")
        .env(
            "FAMILIAR_AI_DATABASE__PATH",
            temp.path().join("familiar.db"),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .starts_with("error: repository resolution failed:"));
}
