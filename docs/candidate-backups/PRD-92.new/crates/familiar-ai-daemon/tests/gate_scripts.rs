//! PRD-092 f1: scripts/gate-build.sh, check-no-workaround-flags.sh,
//! check-feature-parity.sh and measure-build-time.sh existed only as
//! documentation a human had to remember to run — no executed check invoked
//! any of them, so a regression in any could only be caught by hand.
//!
//! `cargo test -p familiar-ai-daemon` (tests-green-crates) and
//! `cargo test --workspace` (tests-workspace-advisory) already run on every
//! cycle, so wiring the scripts in as an integration test makes each one an
//! executed, failable part of the gate without touching docker-compose.yml
//! or a CI workflow — neither of which is in this crate's allowed change
//! scope.
//!
//! measure-build-time.sh's real timing run (`cargo clean` plus two full
//! rebuilds) is deliberately not invoked here: it would make every test run
//! take minutes and its pass/fail would depend on the host's speed. Its
//! `--selftest` mode pins the FAM-BUG-030 ceiling-comparison logic
//! deterministically, which is what this test asserts instead.
//!
//! check-feature-parity.sh and check-no-workaround-flags.sh's real
//! (non-`--selftest`) invocations scan `README.md`, which the top-level
//! `.dockerignore` excludes from the tester image `tests-green-crates` and
//! `tests-workspace-advisory` actually run in (that file is outside this
//! crate's allowed change scope this cycle). The real-repo-scan tests below
//! skip themselves when `README.md` is absent rather than failing on an
//! environment precondition they cannot fix, but still run for real on any
//! host — a plain `cargo test`, or a future non-Docker CI job — where the
//! docs are present.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_script(name: &str, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(format!("scripts/{name}"))
        .args(args)
        .current_dir(workspace_root())
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke scripts/{name}: {e}"))
}

fn assert_script_succeeds(name: &str, args: &[&str]) {
    let output = run_script(name, args);
    assert!(
        output.status.success(),
        "scripts/{name} {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[cfg(unix)]
fn gate_build_script_builds_default_feature_set_clean() {
    assert_script_succeeds("gate-build.sh", &[]);
}

#[test]
#[cfg(unix)]
fn check_no_workaround_flags_selftest_passes() {
    assert_script_succeeds("check-no-workaround-flags.sh", &["--selftest"]);
}

#[test]
#[cfg(unix)]
fn check_no_workaround_flags_repo_has_no_undeclared_invocations() {
    if !workspace_root().join("README.md").is_file() {
        eprintln!(
            "skipping: README.md absent (excluded by .dockerignore in the tester image) — \
             this check needs it to scan for undeclared workaround flags"
        );
        return;
    }
    assert_script_succeeds("check-no-workaround-flags.sh", &[]);
}

#[test]
#[cfg(unix)]
fn check_feature_parity_selftest_passes() {
    assert_script_succeeds("check-feature-parity.sh", &["--selftest"]);
}

#[test]
#[cfg(unix)]
fn check_feature_parity_repo_gate_dockerfile_and_readme_agree() {
    if !workspace_root().join("README.md").is_file() {
        eprintln!(
            "skipping: README.md absent (excluded by .dockerignore in the tester image) — \
             this check needs it to compare marked feature-set lines"
        );
        return;
    }
    assert_script_succeeds("check-feature-parity.sh", &[]);
}

#[test]
#[cfg(unix)]
fn measure_build_time_selftest_pins_fam_bug_030_ceiling() {
    assert_script_succeeds("measure-build-time.sh", &["--selftest"]);
}
