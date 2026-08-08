//! PRD-025: a repository whose backlog does not follow the canonical convention
//! is drivable once the operator describes it. These tests drive the real binary
//! over a temporary repository in the numbered-slug convention.

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

/// A repository in the motivating convention: `docs/prd/todo` and
/// `docs/prd/done`, `0139a-name.md` filenames, em-dash headings.
fn spectra_repo() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(temp.path().join("docs/prd/todo")).unwrap();
    fs::create_dir_all(temp.path().join("docs/prd/done")).unwrap();
    temp
}

fn todo(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join("docs/prd/todo").join(name), body).unwrap();
}

fn done(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join("docs/prd/done").join(name), body).unwrap();
}

/// Write an operator configuration binding this worktree to the profile, and
/// return the config directory to point the binary at.
fn describe(repo: &Path, config_home: &Path, profile: &str) {
    let dir = config_home.join("familiar-ai");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        format!(
            "[repositories.\"{}\"]\nprofile = \"{profile}\"\n\
             active_dir = \"docs/prd/todo\"\narchived_dir = \"docs/prd/done\"\n",
            repo.display()
        ),
    )
    .unwrap();
}

fn next_command(repo: &Path, config_home: &Path, database: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
    command
        .current_dir(repo)
        .arg("next")
        .env("XDG_CONFIG_HOME", config_home)
        .env("FAMILIAR_AI_DATABASE__PATH", database);
    command
}

/// Criterion 1: the numbered-slug repository is discovered, validated,
/// completed from location, and selected from — with epic children ordered
/// strictly before their umbrella, and the umbrella before the next number.
#[test]
fn a_numbered_slug_repository_is_drivable_and_epic_ordered() {
    let repo = spectra_repo();
    let config_home = tempdir().unwrap();
    let database = repo.path().join("state/familiar.db");
    describe(repo.path(), config_home.path(), "numbered-slug");

    done(repo.path(), "0100-earlier.md", "# PRD 0100 — Earlier\n");
    todo(repo.path(), "0140-next-number.md", "# PRD 0140 — Next\n");
    todo(repo.path(), "0139-umbrella.md", "# PRD 0139 — Umbrella\n");
    todo(
        repo.path(),
        "0139b-second-child.md",
        "# PRD 0139b — Second\n",
    );
    todo(repo.path(), "0139a-first-child.md", "# PRD 0139a — First\n");

    let output = next_command(repo.path(), config_home.path(), &database)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The lowest pending identity is the first epic child, rendered in the
    // repository's own spelling so it can be grepped against the backlog.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "PRD 0139a\tdocs/prd/todo/0139a-first-child.md\tpending\tFirst\n"
    );

    // The archived PRD is completed by location, with no bootstrap manifest.
    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let archived: String = db
        .conn()
        .query_row(
            "SELECT status FROM backlog_prds WHERE prd_path='docs/prd/done/0100-earlier.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(archived, "completed");
    // The suffix round-trips into persistence.
    let suffix: Option<String> = db
        .conn()
        .query_row(
            "SELECT prd_suffix FROM backlog_prds WHERE prd_path='docs/prd/todo/0139a-first-child.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(suffix.as_deref(), Some("a"));
}

/// Criterion 5: no dependency parsing occurs under numbered-slug, so a file
/// carrying the field is not treated as declaring one.
#[test]
fn a_depends_on_field_is_opaque_content_under_numbered_slug() {
    let repo = spectra_repo();
    let config_home = tempdir().unwrap();
    let database = repo.path().join("state/familiar.db");
    describe(repo.path(), config_home.path(), "numbered-slug");
    todo(
        repo.path(),
        "0002-second.md",
        "# PRD 0002 — Second\n**Depends on:** PRD-9999\n",
    );

    let output = next_command(repo.path(), config_home.path(), &database)
        .output()
        .unwrap();
    // A missing dependency would have failed the graph. It never existed.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "PRD 0002\tdocs/prd/todo/0002-second.md\tpending\tSecond\n"
    );
}

/// Criterion 6: configuration is refused before any repository is accessed.
#[test]
fn an_invalid_repository_entry_fails_closed_at_load() {
    let repo = spectra_repo();
    let config_home = tempdir().unwrap();
    let database = repo.path().join("state/familiar.db");
    let dir = config_home.path().join("familiar-ai");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        format!(
            "[repositories.\"{}\"]\nprofile = \"numbered-slug\"\n\
             active_dir = \"../escape\"\narchived_dir = \"docs/prd/done\"\n",
            repo.path().display()
        ),
    )
    .unwrap();
    todo(repo.path(), "0002-second.md", "# PRD 0002 — Second\n");

    let output = next_command(repo.path(), config_home.path(), &database)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("must be a repository-relative path without traversal"),
        "stderr was: {stderr}"
    );
}

/// Criterion 3: a repository with no entry behaves byte-identically to today.
#[test]
fn an_undescribed_canonical_repository_is_unchanged() {
    let temp = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(temp.path().join("docs/prds")).unwrap();
    let config_home = tempdir().unwrap();
    let database = temp.path().join("state/familiar.db");
    fs::write(
        temp.path().join("docs/prds/PRD-009.md"),
        "# PRD-009: Deterministic Backlog Manager\n",
    )
    .unwrap();

    let output = next_command(temp.path(), config_home.path(), &database)
        .output()
        .unwrap();
    assert!(output.status.success());
    // Byte-identical to the pinned canonical output, padding and all.
    assert_eq!(
        output.stdout,
        b"PRD-9\tdocs/prds/PRD-009.md\tpending\tDeterministic Backlog Manager\n"
    );
}
