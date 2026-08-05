use familiar_ai_core::{
    BacklogDiscovery, BacklogStatus, BacklogStatusStore, FilesystemBacklogDiscovery,
};
use familiar_ai_storage::{Database, SqliteBacklogRepository};
use std::{fs, process::Command};
use tempfile::tempdir;

fn pending_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-014.md"),
        "# PRD-14: Recorded Completion\n",
    )
    .unwrap();
    let database = repo.path().join("state.db");
    (repo, database)
}

fn run(repo: &std::path::Path, database: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(args)
        .current_dir(repo)
        .env("FAMILIAR_AI_DATABASE__PATH", database)
        .output()
        .unwrap()
}

#[test]
fn record_complete_transitions_a_fresh_pending_entry_and_writes_audit_rows() {
    let (repo, database) = pending_repo();
    let output = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "implemented, reviewed, and merged before this database existed",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "backlog recovery: PRD-14 docs/prds/PRD-014.md pending -> completed action=recorded_complete actor=\"human:trollboy\" reason=\"implemented, reviewed, and merged before this database existed\"\n"
    );
    assert!(output.stderr.is_empty());

    let db = Database::open(&database).unwrap();
    let (status, events, recovery): (String, i64, i64) = db
        .conn()
        .query_row(
            "SELECT status,(SELECT count(*) FROM backlog_status_events),\
             (SELECT count(*) FROM backlog_recovery_events) FROM backlog_prds",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), events, recovery), ("completed", 1, 1));
    let (action, actor, reason): (String, String, String) = db
        .conn()
        .query_row(
            "SELECT r.action,e.actor,r.reason FROM backlog_recovery_events r \
             JOIN backlog_status_events e ON e.event_id = r.status_event_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (action.as_str(), actor.as_str(), reason.as_str()),
        (
            "recorded_complete",
            "human:trollboy",
            "implemented, reviewed, and merged before this database existed"
        )
    );
}

#[test]
fn record_complete_refuses_in_progress_and_leaves_prd012_as_the_only_path() {
    let (repo, database) = pending_repo();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    {
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
    }
    drop(db);

    let output = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "should not apply while claimed",
        ],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected pending"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("found in_progress"));

    // PRD-012's release verb remains the path back to pending for an in_progress entry.
    let released = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "release",
            "docs/prds/PRD-014.md",
            "--actor",
            "ops:alice",
            "--reason",
            "returning to pending",
        ],
    );
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
}

#[test]
fn record_complete_refuses_already_completed_by_exact_diagnostic() {
    let (repo, database) = pending_repo();
    let completed = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "first declaration",
        ],
    );
    assert!(completed.status.success());

    let repeated = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "second declaration",
        ],
    );
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("found completed"));
}

#[test]
fn record_complete_refuses_blocked_by_exact_diagnostic() {
    let (repo, database) = pending_repo();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    {
        let mut store = SqliteBacklogRepository::new(db.conn_mut());
        store
            .reconcile_and_snapshot(&identity, &discovered)
            .unwrap();
        store
            .transition(
                &identity,
                &discovered[0].path,
                BacklogStatus::Pending,
                BacklogStatus::Blocked,
                "ops:alice",
            )
            .unwrap();
    }
    drop(db);

    let output = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "should not apply while blocked",
        ],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("found blocked"));
}

#[test]
fn record_complete_refuses_unknown_path_non_human_actor_and_blank_reason() {
    let (repo, database) = pending_repo();

    let unknown = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-999.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "declared",
        ],
    );
    assert!(!unknown.status.success());
    assert!(unknown.stdout.is_empty());

    let non_human = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "ops:alice",
            "--reason",
            "declared",
        ],
    );
    assert!(!non_human.status.success());
    assert!(non_human.stdout.is_empty());

    let blank_reason = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-014.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "   ",
        ],
    );
    assert!(!blank_reason.status.success());
    assert!(blank_reason.stdout.is_empty());

    let db = Database::open(&database).unwrap();
    let events: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM backlog_status_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(events, 0);
}

#[test]
fn record_complete_refuses_incomplete_dependency_naming_it_then_succeeds_in_order() {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-009.md"), "# PRD-9: Nine\n").unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-010.md"),
        "# PRD-10: Ten\n\n**Depends on:** PRD-9\n",
    )
    .unwrap();
    let database = repo.path().join("state.db");

    let reversed = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-010.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "reversed order",
        ],
    );
    assert!(!reversed.status.success());
    assert!(reversed.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&reversed.stderr);
    assert!(stderr.contains("incomplete dependencies"));
    assert!(stderr.contains("PRD-9"));

    let dependency_first = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-009.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "dependency merged earlier",
        ],
    );
    assert!(
        dependency_first.status.success(),
        "{}",
        String::from_utf8_lossy(&dependency_first.stderr)
    );

    let dependent_second = run(
        repo.path(),
        &database,
        &[
            "backlog",
            "record-complete",
            "docs/prds/PRD-010.md",
            "--actor",
            "human:trollboy",
            "--reason",
            "dependent merged after",
        ],
    );
    assert!(
        dependent_second.status.success(),
        "{}",
        String::from_utf8_lossy(&dependent_second.stderr)
    );
}
