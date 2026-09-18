//! PRD-099 regressions: the gate runs without being asked, is defined once,
//! never reports an unknown outcome as a pass, refuses an unrecorded override,
//! answers for any commit, and cannot be weakened from outside this tree.
//!
//! These read the gate definition as data. That is the point: the assertions
//! are about the files a reviewer would have to change to weaken verification,
//! so weakening it fails the build rather than passing quietly.

use std::fs;
use std::path::{Path, PathBuf};

use familiar_ai_daemon::cli::gate::{merge_decision, verdict_from_check_runs, GateVerdict};
use familiar_ai_storage::{Database, GateOverrideRepository};

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

// ---------------------------------------------------------------------------
// AC1 — verification runs on both triggers, without a human invoking it.
// ---------------------------------------------------------------------------

#[test]
fn the_gate_declares_both_triggers_and_neither_is_manual() {
    let full = read(".github/workflows/gate.yml");
    // Comments explain why the file is shaped the way it is and routinely name
    // the things it must not do, so the assertions read the directives only.
    let workflow: String = full
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        workflow.contains("push:"),
        "the gate must run on push to the default branch"
    );
    assert!(
        workflow.contains("branches: [main]"),
        "the push trigger must name the default branch"
    );
    assert!(
        workflow.contains("pull_request:"),
        "the gate must run on every pull request"
    );

    // A gate that runs when someone chooses to run it measures diligence, not
    // correctness. No manual trigger, and no condition on the job that could
    // make a run silently not happen.
    assert!(
        !workflow.contains("workflow_dispatch"),
        "the gate must not be manually triggerable — that is the failure mode it exists to end"
    );
    for line in workflow.lines() {
        let trimmed = line.trim();
        assert!(
            !trimmed.starts_with("if:"),
            "no step or job in the gate may be conditional, found: {trimmed}"
        );
        assert!(
            !trimmed.starts_with("continue-on-error"),
            "a step that may fail without failing the gate is not a gate step: {trimmed}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC2 — the gate is defined in exactly one place.
// ---------------------------------------------------------------------------

#[test]
fn no_verification_step_is_declared_outside_the_single_definition() {
    // Two lists that are supposed to match will eventually not match. The
    // workflow, the compose service and the README may invoke the definition;
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

    scan("gate.yml", &read(".github/workflows/gate.yml"));
    scan("docker-compose.yml", &read("docker-compose.yml"));
    scan("README.md", &read("README.md"));

    assert!(
        offenders.is_empty(),
        "verification steps must live only in scripts/gate.sh, found elsewhere:\n{}",
        offenders.join("\n")
    );

    // And the single definition really is the thing every caller runs.
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
        read(".github/workflows/gate.yml").contains("scripts/gate.sh"),
        "the workflow must invoke the single definition"
    );
    assert!(
        read("README.md").contains("scripts/gate.sh"),
        "the README must point a contributor at the single definition"
    );
}

// ---------------------------------------------------------------------------
// AC3 — an incomplete gate is a failed gate.
// ---------------------------------------------------------------------------

#[test]
fn every_unknown_outcome_is_reported_as_failure_and_never_as_a_pass() {
    let cases = [
        (
            "cancelled",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"cancelled"}]}"#,
        ),
        (
            "timed out (lost runner)",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"timed_out"}]}"#,
        ),
        (
            "skipped — the step never executed",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"skipped"}]}"#,
        ),
        (
            "still queued",
            r#"{"check_runs":[{"name":"gate","status":"queued","conclusion":null}]}"#,
        ),
        (
            "in progress",
            r#"{"check_runs":[{"name":"gate","status":"in_progress","conclusion":null}]}"#,
        ),
        (
            "completed with no conclusion at all",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":null}]}"#,
        ),
        (
            "action required",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"action_required"}]}"#,
        ),
        (
            "stale",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"stale"}]}"#,
        ),
        (
            "neutral",
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"neutral"}]}"#,
        ),
    ];
    for (label, body) in cases {
        let verdict = verdict_from_check_runs(body);
        assert!(
            !verdict.is_pass(),
            "{label} must never be a pass, got {}",
            verdict.as_str()
        );
        assert_eq!(
            verdict,
            GateVerdict::Red,
            "{label} is an incomplete gate and an incomplete gate is a failed gate"
        );
    }

    // A verdict that cannot be read is its own answer, and also not a pass.
    for (label, body) in [
        ("not json at all", "<html>502 Bad Gateway</html>"),
        ("json without check_runs", r#"{"message":"Not Found"}"#),
        ("empty body", ""),
    ] {
        let verdict = verdict_from_check_runs(body);
        assert_eq!(
            verdict,
            GateVerdict::Unreadable,
            "{label} must read as unreadable"
        );
        assert!(!verdict.is_pass(), "{label} must never be a pass");
    }

    // One green run among failures is still a failure.
    let mixed = r#"{"check_runs":[
        {"name":"gate","status":"completed","conclusion":"success"},
        {"name":"gate","status":"completed","conclusion":"failure"}
    ]}"#;
    assert_eq!(verdict_from_check_runs(mixed), GateVerdict::Red);
}

#[test]
fn another_workflows_green_check_cannot_turn_the_gate_green() {
    // Absence of the gate is absence, even when the commit has other checks
    // that passed. This is the difference between "verified" and "something
    // ran".
    let body =
        r#"{"check_runs":[{"name":"docs-preview","status":"completed","conclusion":"success"}]}"#;
    assert_eq!(verdict_from_check_runs(body), GateVerdict::Absent);
    assert!(!verdict_from_check_runs(body).is_pass());
}

// ---------------------------------------------------------------------------
// AC5 — the verdict is answerable, in four distinct answers.
// ---------------------------------------------------------------------------

#[test]
fn the_command_answers_green_red_absent_and_unreadable() {
    assert_eq!(
        verdict_from_check_runs(
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"success"}]}"#
        ),
        GateVerdict::Green
    );
    assert_eq!(
        verdict_from_check_runs(
            r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"failure"}]}"#
        ),
        GateVerdict::Red
    );
    assert_eq!(
        verdict_from_check_runs(r#"{"check_runs":[]}"#),
        GateVerdict::Absent
    );
    assert_eq!(verdict_from_check_runs("{"), GateVerdict::Unreadable);

    // Absent is deliberately not red: a commit nothing ever verified is a
    // different fact from a commit that failed.
    assert_ne!(GateVerdict::Absent, GateVerdict::Red);
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
}

// ---------------------------------------------------------------------------
// AC4 — required, and an unrecorded override is itself refused.
// ---------------------------------------------------------------------------

fn database() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

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
    // Green needs no override and is never refused.
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
            "runner outage, verified locally",
        )
        .unwrap();

    assert_eq!(record.commit_sha, "abc123");
    assert_eq!(record.actor, "human:trollboy");
    assert_eq!(record.reason, "runner outage, verified locally");
    assert!(
        !record.created_at.is_empty(),
        "the record must be timestamped"
    );

    let found = repository.for_commit("abc123").unwrap().expect("durable");
    assert_eq!(found, record, "the override must survive as written");

    // With it on record, the same merge proceeds — and says so out loud.
    let line = merge_decision("abc123", GateVerdict::Red, Some(&found)).unwrap();
    assert!(line.contains("human:trollboy"), "{line}");
    assert!(line.contains("runner outage"), "{line}");

    // A commit with no override of its own is unaffected by someone else's.
    assert!(repository.for_commit("def456").unwrap().is_none());
}

#[test]
fn an_override_that_names_nobody_cannot_be_written_at_all() {
    // The constraint is in the schema, not in the caller remembering to check.
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
    let workflow: String = read(".github/workflows/gate.yml")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let gate: String = read("scripts/gate.sh")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    // A step that a repository or organisation setting can disable is a step
    // whose removal leaves no diff and no reviewer.
    for forbidden in ["secrets.", "vars.", "repository_dispatch"] {
        assert!(
            !workflow.contains(forbidden),
            "the gate must not depend on {forbidden}, which lives outside this tree"
        );
    }

    // And the definition itself must not branch on ambient environment.
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
        "the gate's contract, including the branch-protection precondition, must be documented"
    );
}
