use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};
use std::{fs, process::Command};
use tempfile::tempdir;

#[test]
fn first_next_applies_manifest_then_is_silent_and_idempotent() {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::create_dir_all(repo.path().join(".familiar")).unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-001: Historical\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("docs/prds/PRD-010.md"),
        "# PRD-010: Current\n",
    )
    .unwrap();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let found = FilesystemBacklogDiscovery.discover(&identity).unwrap();
    let hash = &found.iter().find(|p| p.number == 1).unwrap().content_hash;
    fs::write(
        repo.path().join(".familiar/backlog-bootstrap.toml"),
        format!("version=1\n[[completed]]\npath='docs/prds/PRD-001.md'\nsha256='{hash}'\n"),
    )
    .unwrap();
    let database = repo.path().join("state.db");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
            .arg("next")
            .current_dir(repo.path())
            .env("FAMILIAR_AI_DATABASE__PATH", &database)
            .env(
                "XDG_RUNTIME_DIR",
                database.parent().unwrap().join("xdg-runtime"),
            )
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        first.stdout,
        b"PRD-10\tdocs/prds/PRD-010.md\tpending\tCurrent\n"
    );
    assert!(String::from_utf8_lossy(&first.stderr)
        .starts_with("historical backlog bootstrap applied: run=bootstrap-"));
    let second = run();
    assert!(second.status.success());
    assert_eq!(second.stdout, first.stdout);
    assert!(second.stderr.is_empty());
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let counts:(i64,i64,i64)=db.conn().query_row("SELECT (SELECT count(*) FROM backlog_bootstrap_runs),(SELECT count(*) FROM backlog_bootstrap_items),(SELECT count(*) FROM backlog_status_events)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(counts, (1, 1, 1));
}
