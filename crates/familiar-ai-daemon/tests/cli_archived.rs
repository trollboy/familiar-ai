//! PRD-023: an archived PRD is a completed PRD. These tests drive the real
//! binary over temporary repositories so the guarantee is proven end to end,
//! not just in the discovery unit tests.

use std::fs;
use std::path::Path;
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
    fs::create_dir_all(temp.path().join("docs/prds/done")).unwrap();
    temp
}

fn active(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join("docs/prds").join(name), body).unwrap();
}

fn archived(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join("docs/prds/done").join(name), body).unwrap();
}

fn next_command(repo: &tempfile::TempDir, database: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
    command
        .current_dir(repo.path())
        .arg("next")
        .env("FAMILIAR_AI_DATABASE__PATH", database);
    command
}

/// Criterion 8: a fresh database over an archived repository needs neither a
/// bootstrap manifest nor a reconciliation pass to select correctly.
/// Criteria 1 and 2: the dependency on archived work still resolves.
#[test]
fn a_fresh_database_selects_correctly_over_an_archived_repository() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    archived(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    archived(repo.path(), "PRD-002.md", "# PRD-002: Two\n");
    active(
        repo.path(),
        "PRD-003.md",
        "# PRD-003: Three\n**Depends on:** PRD-001, PRD-002\n",
    );
    assert!(
        !repo
            .path()
            .join(".familiar/backlog-bootstrap.toml")
            .exists(),
        "this fixture must prove selection without a manifest"
    );

    let output = next_command(&repo, &database).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"PRD-3\tdocs/prds/PRD-003.md\tpending\tThree\n"
    );

    // Criterion 3: seeding by location is not a correction, so nothing is
    // recorded as a status change on a first run.
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let events: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM backlog_status_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(events, 0);
    let archived_status: String = db
        .conn()
        .query_row(
            "SELECT status FROM backlog_prds WHERE prd_path='docs/prds/done/PRD-001.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(archived_status, "completed");
}

/// Criterion 4: selection never returns archived work even when it is the
/// lowest-numbered pending-looking entry in the repository.
#[test]
fn selection_skips_archived_work_and_reports_only_active_backlog() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    archived(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    active(repo.path(), "PRD-007.md", "# PRD-007: Seven\n");

    let output = next_command(&repo, &database).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"PRD-7\tdocs/prds/PRD-007.md\tpending\tSeven\n"
    );
}

/// Criterion 4: admission refuses an archived path by exact diagnostic.
#[test]
fn run_refuses_an_archived_prd_by_exact_diagnostic() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    archived(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    active(repo.path(), "PRD-002.md", "# PRD-002: Two\n");

    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .current_dir(repo.path())
        .args(["run", "docs/prds/done/PRD-001.md"])
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "cannot run docs/prds/done/PRD-001.md: PRD is archived and already completed"
        ),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// PRD-023 criterion 5 as amended by PRD-025: a PRD in both locations is
/// refused by exact diagnostic naming both paths — but only that identity. The
/// rest of the backlog stays drivable, because one ambiguous number must not
/// hold every unambiguous one hostage.
#[test]
fn a_prd_in_both_locations_is_refused_without_stopping_the_rest() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    active(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    archived(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    active(repo.path(), "PRD-002.md", "# PRD-002: Two\n");

    let output = next_command(&repo, &database).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The unambiguous PRD is still selected...
    assert_eq!(
        output.stdout,
        b"PRD-2\tdocs/prds/PRD-002.md\tpending\tTwo\n"
    );
    // ...and the conflict is reported, naming both paths.
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refusing conflicting identities: PRD 1 is present in both locations: \
         docs/prds/PRD-001.md, docs/prds/done/PRD-001.md\n"
    );
    // The refused identity is never recorded as backlog work.
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let rows: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM backlog_prds WHERE prd_number = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "a refused identity must mutate no row");
}

/// Criteria 2 and 7: archiving tracked work leaves `next` working and retires
/// the vacated active row, while wave-one documents already in `done/` are
/// ignored rather than misread as wave-two entries.
///
/// Note this is not the criterion-3 *correction* path. Archiving moves the file,
/// so the archived path is a new row seeded completed — there is no stored
/// status at that path to disagree with. The correction path needs a row that
/// already exists at the archived path with a non-completed status, which is
/// constructible only below the CLI; it is pinned in the storage suite by
/// `a_stored_status_disagreeing_with_location_is_corrected_visibly_and_once`.
#[test]
fn archiving_tracked_work_retires_the_active_row_and_keeps_next_working() {
    let repo = git_repo();
    let database = repo.path().join("state/familiar.db");
    active(repo.path(), "PRD-001.md", "# PRD-001: One\n");
    active(repo.path(), "PRD-002.md", "# PRD-002: Two\n");
    archived(
        repo.path(),
        "001-daemon-skeleton.md",
        "# Spec 1: Skeleton\n",
    );

    // First run tracks PRD-1 as pending active work.
    assert!(next_command(&repo, &database)
        .output()
        .unwrap()
        .status
        .success());

    // The human archives it, exactly as a `git mv` would.
    fs::rename(
        repo.path().join("docs/prds/PRD-001.md"),
        repo.path().join("docs/prds/done/PRD-001.md"),
    )
    .unwrap();

    let output = next_command(&repo, &database).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"PRD-2\tdocs/prds/PRD-002.md\tpending\tTwo\n"
    );

    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let (status, missing): (String, Option<String>) = db
        .conn()
        .query_row(
            "SELECT status,missing_since FROM backlog_prds \
             WHERE prd_path='docs/prds/done/PRD-001.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "completed");
    assert!(missing.is_none());
    // The vacated active row is retired rather than left as selectable work.
    let vacated: Option<String> = db
        .conn()
        .query_row(
            "SELECT missing_since FROM backlog_prds WHERE prd_path='docs/prds/PRD-001.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(vacated.is_some());
}
