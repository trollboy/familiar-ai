//! PRD-099 regressions: the gate runs without being asked, is defined once,
//! never reports an unknown outcome as a pass, refuses an unrecorded override,
//! answers for any commit, and cannot be weakened from outside this tree.
//!
//! Amended 2026-09-19: verification is local. This is a desktop application
//! that runs on the machine doing the work, so the trigger is a git hook and
//! the verdict lives in Familiar's ledger — not a hosted runner and not a
//! forge's check-run API.
//!
//! These read the gate definition as data. That is the point: the assertions
//! are about the files a reviewer would have to change to weaken verification,
//! so weakening it fails the build rather than passing quietly.

use std::fs;
use std::path::{Path, PathBuf};

use familiar_ai_daemon::cli::gate::{merge_decision, verdict_of, GateVerdict};
use familiar_ai_storage::{Database, GateOverrideRepository, GateVerdictRepository};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()))
}

/// Strip `#` comments so prose explaining the design cannot satisfy or break
/// an assertion about the directives.
fn directives(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// AC1 — verification runs without a human invoking it, and locally.
// ---------------------------------------------------------------------------

#[test]
fn the_trigger_is_a_local_hook_that_records_its_verdict() {
    let hook = directives(&read("scripts/hooks/pre-push"));

    assert!(
        hook.contains("gate run"),
        "the hook must run the gate and record the verdict, not just run the steps"
    );
    assert!(
        hook.contains("scripts/gate.sh"),
        "its fallback must still be the single definition"
    );
    // A binary predating the gate command exists on PATH and cannot run it.
    // Trusting the name blocks the push with an argument-parsing error
    // instead of verifying anything, which is how this hook first failed.
    assert!(
        hook.contains("gate --help"),
        "the hook must probe for the gate capability, not merely for a binary \
         called familiar-ai"
    );

    // A gate that runs when someone chooses to run it measures diligence, not
    // correctness. The hook must not be opt-in at its own discretion.
    for weakener in ["GATE_SKIP", "SKIP_GATE", "if [ -z"] {
        assert!(
            !hook.contains(weakener),
            "the hook must not carry its own bypass ({weakener}) — `git push --no-verify` \
             is the bypass, and it leaves the commit reading `absent`"
        );
    }
}

#[test]
fn verification_does_not_depend_on_a_hosted_runner() {
    // This is a locally running desktop application. Nothing about knowing
    // whether a commit is verified should require a forge, a network, or
    // anyone's build minutes.
    assert!(
        !repo_root().join(".github/workflows").exists(),
        "verification must not be delegated to a hosted runner"
    );
    let command = read("crates/familiar-ai-daemon/src/cli/gate.rs");
    for forge in ["api.github.com", "check_runs", "reqwest"] {
        assert!(
            !command.contains(forge),
            "the verdict must be read from Familiar's own ledger, not from {forge}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC2 — the gate is defined in exactly one place.
// ---------------------------------------------------------------------------

#[test]
fn no_verification_step_is_declared_outside_the_single_definition() {
    // Two lists that are supposed to match will eventually not match. The
    // compose service, the README and the hook may invoke the definition;
    // none of them may restate what it does.
    let mut offenders = Vec::new();

    let mut scan = |label: &str, body: &str| {
        for (number, line) in body.lines().enumerate() {
            let text = line.trim();
            if text.starts_with('#') || text.starts_with("//") {
                continue;
            }
            for verb in ["cargo fmt", "cargo clippy", "cargo test", "cargo llvm-cov"] {
                if text.contains(verb) {
                    offenders.push(format!("{label}:{}: {text}", number + 1));
                }
            }
            // `cargo build` is allowed only as the documented install command,
            // which builds the product rather than verifying it.
            if text.contains("cargo build") && !text.contains("--bin familiar-ai") {
                offenders.push(format!("{label}:{}: {text}", number + 1));
            }
        }
    };

    scan("docker-compose.yml", &read("docker-compose.yml"));
    scan("README.md", &read("README.md"));
    scan("pre-push", &read("scripts/hooks/pre-push"));

    assert!(
        offenders.is_empty(),
        "verification steps must live only in scripts/gate.sh, found elsewhere:\n{}",
        offenders.join("\n")
    );

    let gate = read("scripts/gate.sh");
    for verb in ["cargo fmt", "cargo clippy", "cargo test"] {
        assert!(
            gate.contains(verb),
            "scripts/gate.sh is the definition and must contain {verb}"
        );
    }
    assert!(
        read("docker-compose.yml").contains("scripts/gate.sh"),
        "the compose service must invoke the single definition"
    );
    assert!(
        read("README.md").contains("scripts/gate.sh"),
        "the README must point a contributor at the single definition"
    );
}

// ---------------------------------------------------------------------------
// AC3 / AC5 — the four answers, and unknown is never a pass.
// ---------------------------------------------------------------------------

fn database() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

#[test]
fn the_command_answers_green_red_absent_and_unreadable() {
    let db = database();
    let verdicts = GateVerdictRepository::new(&db);

    // Absent: nothing ever verified this commit. Deliberately not red.
    assert_eq!(verdict_of(None), GateVerdict::Absent);
    assert_eq!(
        verdict_of(verdicts.for_commit("never-seen").unwrap().as_ref()),
        GateVerdict::Absent
    );

    let green = verdicts
        .record("aaa", "green", "fmt ok; clippy ok; test ok")
        .unwrap();
    assert_eq!(verdict_of(Some(&green)), GateVerdict::Green);

    let red = verdicts.record("bbb", "red", "test FAILED").unwrap();
    assert_eq!(verdict_of(Some(&red)), GateVerdict::Red);

    assert_eq!(GateVerdict::Green.as_str(), "green");
    assert_eq!(GateVerdict::Red.as_str(), "red");
    assert_eq!(GateVerdict::Absent.as_str(), "absent");
    assert_eq!(GateVerdict::Unreadable.as_str(), "unreadable");

    assert!(GateVerdict::Green.is_pass());
    for verdict in [
        GateVerdict::Red,
        GateVerdict::Absent,
        GateVerdict::Unreadable,
    ] {
        assert!(!verdict.is_pass(), "{} must not pass", verdict.as_str());
    }
    assert_ne!(GateVerdict::Absent, GateVerdict::Red);
}

#[test]
fn a_verdict_that_cannot_be_understood_is_never_a_pass() {
    // The schema constrains what can be written, but a row read back with an
    // outcome this build does not recognise — a newer writer, a corrupted
    // value — must not be assumed green.
    for stored in ["", "GREEN", "passed", "probably fine", "unknown"] {
        let record = familiar_ai_storage::GateVerdictRecord {
            commit_sha: "ccc".into(),
            verdict: stored.into(),
            detail: String::new(),
            recorded_at: "2026-09-19T00:00:00Z".into(),
        };
        let verdict = verdict_of(Some(&record));
        assert_eq!(
            verdict,
            GateVerdict::Unreadable,
            "a stored verdict of {stored:?} must read as unreadable"
        );
        assert!(!verdict.is_pass(), "{stored:?} must never be a pass");
    }
}

#[test]
fn the_schema_refuses_an_outcome_the_gate_cannot_produce() {
    let db = database();
    let verdicts = GateVerdictRepository::new(&db);
    for bogus in ["amber", "skipped", "cancelled", ""] {
        assert!(
            verdicts.record("ddd", bogus, "").is_err(),
            "the ledger must refuse a verdict of {bogus:?}"
        );
    }
}

#[test]
fn a_verdict_is_only_recorded_for_a_tree_that_matches_the_commit() {
    // `gate run` verifies the working tree but records against HEAD. If those
    // differ, the verdict describes a tree nobody committed, and attaching it
    // to the commit is a false green. The command refuses to record instead.
    let command = read("crates/familiar-ai-daemon/src/cli/gate.rs");
    assert!(
        command.contains("working_tree_is_dirty"),
        "gate run must check the tree against HEAD before recording a verdict"
    );
    assert!(
        command.contains("--untracked-files=no"),
        "untracked files are not part of the commit and must not block recording"
    );
    let guard = command
        .split_once("working_tree_is_dirty()?")
        .map(|(_, tail)| tail.split("}}").next().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        guard.contains("not recording"),
        "the refusal to record must say so out loud rather than silently skipping"
    );
}

#[test]
fn rerunning_the_gate_replaces_that_commits_verdict() {
    // The latest run is the truth about that tree — a stale red must not
    // outlive the fix, and a stale green must not outlive a regression.
    let db = database();
    let verdicts = GateVerdictRepository::new(&db);
    verdicts.record("eee", "red", "test FAILED").unwrap();
    verdicts.record("eee", "green", "test ok").unwrap();
    let found = verdicts.for_commit("eee").unwrap().unwrap();
    assert_eq!(found.verdict, "green");
    assert_eq!(verdict_of(Some(&found)), GateVerdict::Green);
}

// ---------------------------------------------------------------------------
// AC4 — required, and an unrecorded override is itself refused.
// ---------------------------------------------------------------------------

#[test]
fn a_merge_past_a_non_green_gate_is_refused_without_a_recorded_override() {
    for verdict in [
        GateVerdict::Red,
        GateVerdict::Absent,
        GateVerdict::Unreadable,
    ] {
        let refusal = merge_decision("abc123", verdict, None)
            .expect_err("a non-green gate with no override must refuse the merge");
        assert!(
            refusal.contains("no override is recorded"),
            "the refusal must say why: {refusal}"
        );
        assert!(
            refusal.contains(verdict.as_str()),
            "the refusal must name the verdict it is refusing: {refusal}"
        );
    }
    assert!(merge_decision("abc123", GateVerdict::Green, None).is_ok());
}

#[test]
fn a_recorded_override_names_the_actor_the_commit_and_the_reason() {
    let db = database();
    let repository = GateOverrideRepository::new(&db);
    let record = repository
        .record(
            "abc123",
            "red",
            "human:trollboy",
            "verified locally, see FAM-BUG-031",
        )
        .unwrap();

    assert_eq!(record.commit_sha, "abc123");
    assert_eq!(record.actor, "human:trollboy");
    assert!(
        !record.created_at.is_empty(),
        "the record must be timestamped"
    );

    let found = repository.for_commit("abc123").unwrap().expect("durable");
    assert_eq!(found, record, "the override must survive as written");

    let line = merge_decision("abc123", GateVerdict::Red, Some(&found)).unwrap();
    assert!(line.contains("human:trollboy"), "{line}");
    assert!(line.contains("FAM-BUG-031"), "{line}");

    assert!(repository.for_commit("def456").unwrap().is_none());
}

#[test]
fn an_override_that_names_nobody_cannot_be_written_at_all() {
    let db = database();
    let repository = GateOverrideRepository::new(&db);
    for (actor, reason) in [
        ("", "a reason"),
        ("  ", "a reason"),
        ("human:x", ""),
        ("human:x", "   "),
    ] {
        assert!(
            repository.record("abc123", "red", actor, reason).is_err(),
            "an override with actor {actor:?} and reason {reason:?} must be refused"
        );
    }
    assert!(
        repository.for_commit("abc123").unwrap().is_none(),
        "no partial record may survive a refused override"
    );
}

// ---------------------------------------------------------------------------
// AC6 — what verification means is changeable only in this repository.
// ---------------------------------------------------------------------------

#[test]
fn the_gate_definition_depends_on_no_configuration_outside_this_repository() {
    let gate = directives(&read("scripts/gate.sh"));

    for forbidden in ["GATE_SKIP", "SKIP_", "if [ -n \"${CI"] {
        assert!(
            !gate.contains(forbidden),
            "scripts/gate.sh must not be weakenable by the environment ({forbidden})"
        );
    }

    assert!(
        repo_root()
            .join("docs/contracts/verification-gate.md")
            .exists(),
        "the gate's contract must be documented"
    );
}
