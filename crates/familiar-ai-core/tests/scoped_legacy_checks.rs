//! FAM-BUG-108: a repository's legacy `[[repositories.X.review.verification]]`
//! entry may reuse a global check id (`lint`) for a different command. The
//! rewrite into named checks must scope it to the repository, not refuse the
//! whole configuration; the run in that repository still sees `lint`.

use std::fs;

#[test]
fn a_repository_may_redefine_a_global_check_id_for_itself() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("spectra");
    fs::create_dir_all(repo.join("docs/prds")).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let repo_key = repo.canonicalize().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[review.verification]]
check_id = "lint"
argv = ["cargo", "clippy"]
timeout_ms = 1000

[[review.verification]]
check_id = "format"
argv = ["cargo", "fmt", "--check"]
timeout_ms = 1000

[repositories."{repo}"]
profile = "canonical"

[[repositories."{repo}".review.verification]]
check_id = "lint"
argv = ["make", "lint"]
timeout_ms = 2000
"#,
            repo = repo_key.display()
        ),
    )
    .unwrap();
    let config = familiar_ai_core::Config::load(Some(&config_path))
        .expect("a repository-scoped redefinition of `lint` is not a conflict");

    let global_lint = config
        .review
        .verification
        .iter()
        .find(|c| c.check_id == "lint")
        .expect("global lint survives");
    assert_eq!(global_lint.argv, ["cargo", "clippy"]);

    let effective = config.repository(&repo_key).unwrap();
    let repo_checks = &effective
        .review
        .as_ref()
        .expect("repository review")
        .verification;
    let repo_lint = repo_checks
        .iter()
        .find(|c| c.check_id == "lint")
        .expect("the repository sees a plain `lint`");
    assert_eq!(repo_lint.argv, ["make", "lint"]);
    assert_eq!(repo_lint.timeout_ms, 2000);
    assert!(
        config.checks.keys().any(|k| k.starts_with("lint@")),
        "the scoped key exists in the named checks: {:?}",
        config.checks.keys().collect::<Vec<_>>()
    );
}
