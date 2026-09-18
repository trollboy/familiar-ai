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
        .env("FAMILIAR_AI_DATABASE__PATH", database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        );
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
fn fresh_database_selects_active_work_after_archived_dependency() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
    fs::write(
        repo.path().join("docs/prds/done/PRD-001.md"),
        "# PRD-001: Finished\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-002.md"),
        "# PRD-002: Remaining\n\n**Depends on:** PRD-001\n",
    )
    .unwrap();

    let output = next_command(&repo, &database).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"PRD-2\tdocs/prds/PRD-002.md\tpending\tRemaining\n"
    );
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let archived: (String, i64) = db
        .conn()
        .query_row(
            "SELECT status,(SELECT count(*) FROM backlog_status_events WHERE prd_path='docs/prds/done/PRD-001.md') FROM backlog_prds WHERE prd_path='docs/prds/done/PRD-001.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(archived, ("completed".into(), 0));
}

/// Archiving a PRD that already carries history must not break the next run.
///
/// `backlog_status_events` holds a foreign key into
/// `backlog_prds(repository_key, prd_path)`, so any attempt to tidy the
/// moved-from row away — by deleting it or by moving its path — aborts the
/// reconciliation transaction and takes `next`, `run` and `drive` with it. A
/// change that did exactly that shipped to main, because the unit fixtures
/// had no history to violate and nothing ran the command itself.
#[test]
fn archiving_a_prd_with_history_leaves_next_working() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-001: Moves later\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-002.md"),
        "# PRD-002: Remaining\n",
    )
    .unwrap();

    // First run registers both and gives PRD-001 the history a claimed PRD
    // accumulates.
    assert!(next_command(&repo, &database)
        .output()
        .unwrap()
        .status
        .success());
    {
        let db = familiar_ai_storage::Database::open(&database).unwrap();
        db.conn()
            .execute(
                "INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,\
                 actor,changed_at) SELECT repository_key,prd_path,'pending','in_progress',\
                 'human:tester','t' FROM backlog_prds WHERE prd_path='docs/prds/PRD-001.md'",
                [],
            )
            .unwrap();
    }

    // Archive it: same PRD, new path, history still pointing at the old one.
    fs::rename(
        repo.path().join("docs/prds/PRD-001.md"),
        repo.path().join("docs/prds/done/PRD-001.md"),
    )
    .unwrap();

    let output = next_command(&repo, &database).output().unwrap();
    assert!(
        output.status.success(),
        "next must survive archiving a PRD that has history: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // And the run after it, which is where an aborted transaction surfaces.
    let output = next_command(&repo, &database).output().unwrap();
    assert!(
        output.status.success(),
        "the run after an archive must also succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"PRD-2\tdocs/prds/PRD-002.md\tpending\tRemaining\n"
    );
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
        .env("XDG_RUNTIME_DIR", temp.path().join("xdg-runtime"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .starts_with("error: repository resolution failed:"));
}

#[test]
fn numbered_slug_profile_bootstraps_archived_and_selects_child_before_umbrella() {
    let repo = git_repo();
    let home = tempdir().unwrap();
    let xdg = home.path().join("xdg");
    let config_dirs = [
        home.path().join("Library/Application Support/Familiar-AI"),
        xdg.join("familiar-ai"),
    ];
    for config_dir in &config_dirs {
        fs::create_dir_all(config_dir).unwrap();
    }
    fs::create_dir_all(repo.path().join("docs/prd/todo")).unwrap();
    fs::create_dir_all(repo.path().join("docs/prd/done")).unwrap();
    fs::write(
        repo.path().join("docs/prd/done/0138-finished.md"),
        "# PRD 0138 — Finished\n",
    )
    .unwrap();
    for (name, heading) in [
        ("0139-epic.md", "# PRD 0139 — Epic\n"),
        ("0139b-child.md", "# PRD 0139b: Child B\n"),
        ("0139a-child.md", "# PRD 0139a — Child A\n"),
        ("0140-next.md", "# PRD 0140 — Next\n"),
    ] {
        fs::write(repo.path().join("docs/prd/todo").join(name), heading).unwrap();
    }
    for config_dir in &config_dirs {
        fs::write(
            config_dir.join("config.toml"),
            format!("[repositories.\"{}\"]\nprofile = \"numbered-slug\"\nactive_dir = \"docs/prd/todo\"\narchived_dir = \"docs/prd/done\"\n", repo.path().display()),
        ).unwrap();
    }
    let database = repo.path().join("state/profile.db");
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .current_dir(repo.path())
        .arg("next")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", &xdg)
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
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
    assert_eq!(
        output.stdout,
        b"PRD 0139a\tdocs/prd/todo/0139a-child.md\tpending\tChild A\n"
    );
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let archived: (String, Option<String>) = db.conn().query_row("SELECT status,prd_suffix FROM backlog_prds WHERE prd_path='docs/prd/done/0138-finished.md'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(archived, ("completed".into(), None));
}
