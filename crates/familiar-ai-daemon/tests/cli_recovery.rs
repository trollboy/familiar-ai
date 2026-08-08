use familiar_ai_core::{
    BacklogDiscovery, BacklogStatus, BacklogStatusStore, FilesystemBacklogDiscovery,
};
use familiar_ai_storage::{Database, SqliteBacklogRepository};
use std::{fs, process::Command};
use tempfile::tempdir;

fn claimed_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-012.md"),
        "# PRD-012: Recovery\n",
    )
    .unwrap();
    let database = repo.path().join("state.db");
    let identity = FilesystemBacklogDiscovery::default()
        .resolve(repo.path())
        .unwrap();
    let discovered = FilesystemBacklogDiscovery::default()
        .discover(&identity)
        .unwrap()
        .prds;
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    let mut store = SqliteBacklogRepository::new(db.conn_mut());
    store
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();
    store
        .transition(
            &identity,
            &discovered[0].path,
            BacklogStatus::Pending,
            BacklogStatus::InProgress,
            "system:familiar-ai-run:00001785772020811891-0000057947-000001",
        )
        .unwrap();
    drop(db);
    (repo, database)
}

#[test]
fn release_prints_one_line_and_preserves_claim() {
    let (repo, database) = claimed_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args([
            "backlog",
            "release",
            "docs/prds/PRD-012.md",
            "--actor",
            "ops:alice",
            "--reason",
            "review was disabled",
        ])
        .current_dir(repo.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "backlog recovery: PRD-12 docs/prds/PRD-012.md in_progress -> pending action=release actor=\"ops:alice\" reason=\"review was disabled\"\n"
    );
    assert!(output.stderr.is_empty());
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

#[test]
fn complete_is_a_human_only_manual_override() {
    let (repo, database) = claimed_repo();
    let rejected = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args([
            "backlog",
            "complete",
            "docs/prds/PRD-012.md",
            "--actor",
            "system:operator",
            "--reason",
            "accepted",
        ])
        .current_dir(repo.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());

    let accepted = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args([
            "backlog",
            "complete",
            "docs/prds/PRD-012.md",
            "--actor",
            "human:alice",
            "--reason",
            "accepted outside normal review",
        ])
        .current_dir(repo.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(accepted.status.success());
    assert!(String::from_utf8(accepted.stdout)
        .unwrap()
        .starts_with("backlog recovery: MANUAL OVERRIDE PRD-12 "));
    assert!(accepted.stderr.is_empty());
}

#[test]
fn stale_persisted_identity_fails_without_reconciling_or_writing_an_event() {
    let (repo, database) = claimed_repo();
    let db = Database::open(&database).unwrap();
    db.conn()
        .execute(
            "UPDATE backlog_prds SET content_hash='stale-hash', last_seen_at='original'",
            [],
        )
        .unwrap();
    drop(db);

    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args([
            "backlog",
            "release",
            "docs/prds/PRD-012.md",
            "--actor",
            "ops:alice",
            "--reason",
            "retry",
        ])
        .current_dir(repo.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected in_progress"));

    let db = Database::open(&database).unwrap();
    let row: (String, String, String, i64) = db
        .conn()
        .query_row(
            "SELECT content_hash,last_seen_at,status,\
             (SELECT count(*) FROM backlog_status_events) FROM backlog_prds",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "stale-hash".into(),
            "original".into(),
            "in_progress".into(),
            1
        )
    );
}

#[test]
fn complete_help_labels_and_explains_the_manual_override() {
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(["backlog", "complete", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("MANUAL OVERRIDE"));
    assert!(stdout.contains("PRD-011's normal completion-evidence predicate"));
    assert!(stdout.contains("Mandatory explicit human authority"));
    assert!(stdout.contains("Mandatory non-empty audit reason"));
}
