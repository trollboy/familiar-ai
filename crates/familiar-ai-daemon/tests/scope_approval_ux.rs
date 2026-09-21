//! PRD-083 regressions: "Reachable Approval -- the Decidable Pause".
//!
//! A scope pause must never leave a human staring at exactly two
//! continuations that don't answer it (`backlog release` throws the work
//! away, `backlog complete` bypasses every gate). Every surface that
//! reports a scope pause must carry that attempt's exact, hash-bound
//! approve/reject commands ahead of any release/complete line, the bare
//! `scope-decisions` picker must let an operator decide by ordinal or PRD
//! id without ever transcribing a hash, and the drive's own pause must
//! reach the operator either interactively (attended) or via a
//! paste-runnable command (unattended) -- never neither.

use std::io::Cursor;
use std::process::Command;

use familiar_ai_core::RepositoryIdentity;
use familiar_ai_storage::{
    CheckpointRepository, Database, DriverRepository, ExecutionCheckpoint, OrchestrationRepository,
};

fn database() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

fn identity() -> RepositoryIdentity {
    RepositoryIdentity {
        worktree: "/tmp/repo".into(),
        key: "repo".into(),
    }
}

const FINDING_JSON: &str = r#"{"finding_id":"f","change_id":"c","path":"src/unexpected.rs","old_path":null,"change_kind":"added","file_class":"ordinary_source","decision":"undeclared_scope_expansion","rule_id":"no-expected-file-match","rule_source":"built_in","rule_detail":"src/unexpected.rs is not in the PRD's expected_files","expected_file_match":null,"allowed_path_match":null,"prohibited_rule_match":null,"policy_snapshot_hash":"policy-1"}"#;

fn seed_checkpoint(db: &Database, checkpoint_id: &str, prd_id: &str) {
    CheckpointRepository::new(db.conn())
        .put(&ExecutionCheckpoint {
            checkpoint_id: checkpoint_id.into(),
            repository_key: "repo".into(),
            prd_id: prd_id.into(),
            prd_path: format!("docs/prds/{prd_id}.md"),
            execution_id: Some(format!("exec-{prd_id}")),
            phase: "implemented_pending_review".into(),
            base_revision: "deadbeef".into(),
            worktree_path: "/tmp/does-not-matter".into(),
            branch_name: None,
            diff_hash: format!("sha256:candidate-{prd_id}"),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        })
        .unwrap();
}

/// Asserts a rendered surface never presents `backlog release`/`backlog
/// complete` as the only continuation of a scope pause: whenever the text
/// mentions release or complete, an approve command for `scope-decisions`
/// must appear earlier in the text.
fn assert_approve_precedes_release_or_complete(text: &str) {
    let approve_at = text.find("scope-decisions").and_then(|_| {
        text.find("--approve").map(|approve_index| {
            text[..approve_index]
                .rfind("scope-decisions")
                .unwrap_or(approve_index)
        })
    });
    let release_at = text.find("backlog release");
    let complete_at = text.find("backlog complete");
    if let Some(release_at) = release_at {
        let approve_at = approve_at
            .expect("a scope pause must carry an approve command when release is offered");
        assert!(
            approve_at < release_at,
            "approve command must precede backlog release:\n{text}"
        );
    }
    if let Some(complete_at) = complete_at {
        let approve_at = approve_at
            .expect("a scope pause must carry an approve command when complete is offered");
        assert!(
            approve_at < complete_at,
            "approve command must precede backlog complete:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------
// Acceptance criterion 1: the session report, stewardship gates, next, and
// resume all carry the exact approve/reject commands, ahead of any
// release/complete continuation.
// ---------------------------------------------------------------------

#[test]
fn report_prints_approve_and_reject_before_release_or_complete() {
    let db = database();
    let identity = identity();
    let driver = DriverRepository::new(db.conn());
    driver.open_session("drive-1", &identity.key, "{}").unwrap();
    let sequence = driver
        .record_attempt_started("drive-1", "PRD-83", "docs/prds/PRD-83.md", Some("exec-1"))
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-1",
            sequence,
            "retained",
            Some("scope_broadened"),
            None,
            Some(5),
        )
        .unwrap();
    driver
        .finish_session("drive-1", "nothing_eligible")
        .unwrap();

    seed_checkpoint(&db, "cp-1", "PRD-83");
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &identity.key,
            "cp-1",
            "PRD-83",
            "sha256:candidate-PRD-83",
            "finding-hash-1",
            FINDING_JSON,
        )
        .unwrap();

    let report = familiar_ai_daemon::report::render(&db, None, 0).unwrap();
    assert!(
        report.contains(
            "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --approve --actor human:<identity> --reason \"<why>\""
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --reject --actor human:<identity> --reason \"<why>\""
        ),
        "{report}"
    );
    assert!(report.contains("backlog release"));
    assert!(report.contains("backlog complete"));
    assert_approve_precedes_release_or_complete(&report);
}

#[test]
fn report_never_treats_a_non_scope_stop_as_hiding_a_scope_decision() {
    // A stop with no pending scope decision must still show release/complete
    // -- this PRD narrows the "hidden command" bug to scope pauses, it does
    // not remove the existing continuations for other retention reasons.
    let db = database();
    let driver = DriverRepository::new(db.conn());
    driver.open_session("drive-2", "repo", "{}").unwrap();
    let sequence = driver
        .record_attempt_started("drive-2", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-2",
            sequence,
            "retained",
            Some("review_disabled"),
            None,
            Some(5),
        )
        .unwrap();
    driver
        .finish_session("drive-2", "nothing_eligible")
        .unwrap();

    let report = familiar_ai_daemon::report::render(&db, None, 0).unwrap();
    assert!(report.contains("backlog release"));
    assert!(report.contains("backlog complete"));
    assert!(!report.contains("scope-decisions"));
}

#[test]
fn stewardship_gates_prepends_scope_commands_ahead_of_recovery_commands() {
    let db = database();
    let identity = identity();
    let driver = DriverRepository::new(db.conn());
    driver.open_session("drive-1", &identity.key, "{}").unwrap();
    let sequence = driver
        .record_attempt_started("drive-1", "PRD-83", "docs/prds/PRD-83.md", Some("exec-1"))
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-1",
            sequence,
            "retained",
            Some("scope_broadened"),
            None,
            Some(5),
        )
        .unwrap();

    seed_checkpoint(&db, "cp-1", "PRD-83");
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &identity.key,
            "cp-1",
            "PRD-83",
            "sha256:candidate-PRD-83",
            "finding-hash-1",
            FINDING_JSON,
        )
        .unwrap();

    let gates =
        familiar_ai_daemon::stewardship::list_pending_human_gates(&db, &identity, 20).unwrap();
    let items = gates["items"].as_array().unwrap();
    let gate = items
        .iter()
        .find(|item| item["prd_id"] == "PRD-83")
        .expect("PRD-83 gate present");
    let commands: Vec<&str> = gate["recovery_commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(
        commands[0],
        "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --approve --actor human:<identity> --reason \"<why>\""
    );
    assert_eq!(
        commands[1],
        "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --reject --actor human:<identity> --reason \"<why>\""
    );
    assert!(commands.iter().any(|c| c.contains("backlog release")));
    assert!(commands.iter().any(|c| c.contains("backlog complete")));
    assert_approve_precedes_release_or_complete(&gate.to_string());
}

#[test]
fn resume_output_carries_scope_decision_lines_for_a_paused_candidate() {
    use familiar_ai_daemon::resume::{scope_decision_lines, ResumeCandidate};

    let candidates = vec![ResumeCandidate {
        checkpoint_id: "cp-1".into(),
        prd_id: "PRD-83".into(),
        prd_path: "docs/prds/PRD-83.md".into(),
        phase: "blocked".into(),
        worktree: "/tmp/PRD-83".into(),
        changed_files: vec!["src/unexpected.rs".into()],
        valid: true,
        reason: None,
    }];
    let pending = vec![familiar_ai_storage::ScopeDecision {
        finding_hash: "finding-hash-1".into(),
        checkpoint_id: "cp-1".into(),
        prd_id: "PRD-83".into(),
        candidate_hash: "sha256:candidate-PRD-83".into(),
        finding_json: FINDING_JSON.into(),
    }];
    let lines = scope_decision_lines(&candidates, &pending);
    assert!(lines.iter().any(|line| line.contains("--approve")));
    assert!(lines.iter().any(|line| line.contains("--reject")));
    assert!(lines
        .iter()
        .all(|line| line.starts_with("PRD-83\tscope-decision-pending\t")));

    // A candidate with no pending decision gets no lines at all.
    let unaffected = vec![ResumeCandidate {
        checkpoint_id: "cp-2".into(),
        prd_id: "PRD-1".into(),
        prd_path: "docs/prds/PRD-1.md".into(),
        phase: "implemented".into(),
        worktree: "/tmp/PRD-1".into(),
        changed_files: vec![],
        valid: true,
        reason: None,
    }];
    assert!(scope_decision_lines(&unaffected, &pending).is_empty());
}

#[test]
fn next_surfaces_pending_scope_decisions_as_notices() {
    let pending = vec![familiar_ai_storage::ScopeDecision {
        finding_hash: "finding-hash-1".into(),
        checkpoint_id: "cp-1".into(),
        prd_id: "PRD-83".into(),
        candidate_hash: "sha256:candidate-PRD-83".into(),
        finding_json: FINDING_JSON.into(),
    }];
    let notices = familiar_ai_daemon::stewardship::scope_pause_notices(&pending, "next");
    assert_eq!(notices.len(), 2);
    assert!(notices[0].starts_with("next: scope decision pending for PRD-83:"));
    assert!(notices[0].contains("--approve"));
    assert!(notices[1].contains("--reject"));
}

// ---------------------------------------------------------------------
// Acceptance criterion 2: the bare `scope-decisions` picker lists pending
// decisions numbered, naming PRD/finding/change, and never asks for a hash.
// ---------------------------------------------------------------------

#[test]
fn bare_scope_decisions_cli_lists_without_a_hash() {
    let repo = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    std::fs::write(
        repo.path().join("docs/prds/PRD-83.md"),
        "# PRD-83: Reachable Approval\n",
    )
    .unwrap();
    let database = repo.path().join("state.db");
    let db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    let identity = familiar_ai_core::RepositoryIdentity {
        worktree: repo.path().to_path_buf(),
        key: {
            use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};
            FilesystemBacklogDiscovery.resolve(repo.path()).unwrap().key
        },
    };
    seed_checkpoint(&db, "cp-1", "PRD-83");
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &identity.key,
            "cp-1",
            "PRD-83",
            "sha256:candidate-PRD-83",
            "finding-hash-1",
            FINDING_JSON,
        )
        .unwrap();
    drop(db);

    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .arg("scope-decisions")
        .current_dir(repo.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("1. PRD-83"), "{stdout}");
    assert!(stdout.contains("src/unexpected.rs"), "{stdout}");
    assert!(stdout.contains("UndeclaredScopeExpansion"), "{stdout}");
    assert!(!stdout.contains("finding-hash-1"), "{stdout}");
    assert!(!stdout.contains("sha256:candidate"), "{stdout}");
}

// ---------------------------------------------------------------------
// Acceptance criterion 3: the drive's own pause invokes the interactive
// surface when attended, and prints the paste-runnable command when not --
// never neither.
// ---------------------------------------------------------------------

#[test]
fn drive_pause_unattended_prints_approve_and_reject() {
    let mut db = database();
    let identity = identity();
    let mut input = Cursor::new(Vec::new());
    let mut output = Vec::new();
    familiar_ai_daemon::drive::handle_scope_pause(
        &mut db,
        &identity,
        &familiar_ai_core::Config::default(),
        "PRD-83",
        &["finding-hash-1".to_string()],
        "sha256:candidate-PRD-83",
        false,
        &mut input,
        &mut output,
    );
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains(
        "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --approve"
    ));
    assert!(text.contains(
        "familiar-ai scope-decisions --finding-hash finding-hash-1 --candidate-hash sha256:candidate-PRD-83 --reject"
    ));
    assert!(text.find("--approve").unwrap() < text.find("--reject").unwrap());
}

#[test]
fn drive_pause_attended_decides_through_the_interactive_surface() {
    let mut db = database();
    let identity = identity();
    seed_checkpoint(&db, "cp-1", "PRD-83");
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &identity.key,
            "cp-1",
            "PRD-83",
            "sha256:candidate-PRD-83",
            "finding-hash-1",
            FINDING_JSON,
        )
        .unwrap();

    let mut input = Cursor::new(b"1\na\nhuman:tester\nlooks fine\n".to_vec());
    let mut output = Vec::new();
    familiar_ai_daemon::drive::handle_scope_pause(
        &mut db,
        &identity,
        &familiar_ai_core::Config::default(),
        "PRD-83",
        &["finding-hash-1".to_string()],
        "sha256:candidate-PRD-83",
        true,
        &mut input,
        &mut output,
    );

    let (decision, actor): (Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT decision,actor FROM scope_decisions WHERE finding_hash='finding-hash-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(decision.as_deref(), Some("approved"));
    assert_eq!(actor.as_deref(), Some("human:tester"));
}

#[test]
fn drive_pause_never_acts_when_nothing_is_pending() {
    let mut db = database();
    let identity = identity();
    let mut input = Cursor::new(Vec::new());
    let mut output = Vec::new();
    familiar_ai_daemon::drive::handle_scope_pause(
        &mut db,
        &identity,
        &familiar_ai_core::Config::default(),
        "PRD-83",
        &[],
        "sha256:candidate-PRD-83",
        false,
        &mut input,
        &mut output,
    );
    assert!(output.is_empty());
}

// ---------------------------------------------------------------------
// Acceptance criterion 4: an interactive decision and a flag-form decision
// produce identical durable rows and resume through the same continuation.
// ---------------------------------------------------------------------

#[test]
fn interactive_and_flag_decisions_produce_identical_durable_rows() {
    // Two independent, structurally identical pending decisions in the same
    // repository -- one decided interactively, one decided the way the
    // flag-form CLI path does (`OrchestrationRepository::decide_scope`
    // directly, exactly as `cli::scope_decisions::scope_decisions`'s flag
    // branch calls it).
    let mut db = database();
    let identity = identity();
    seed_checkpoint(&db, "cp-interactive", "PRD-1");
    seed_checkpoint(&db, "cp-flag", "PRD-2");
    let repo = OrchestrationRepository::new(db.conn());
    repo.record_scope_finding(
        &identity.key,
        "cp-interactive",
        "PRD-1",
        "sha256:candidate-PRD-1",
        "finding-hash-interactive",
        FINDING_JSON,
    )
    .unwrap();
    repo.record_scope_finding(
        &identity.key,
        "cp-flag",
        "PRD-2",
        "sha256:candidate-PRD-2",
        "finding-hash-flag",
        FINDING_JSON,
    )
    .unwrap();

    // Flag-form: the exact call `scope_decisions()`'s flag branch makes.
    let flag_checkpoint = OrchestrationRepository::new(db.conn())
        .decide_scope(
            &identity.key,
            "finding-hash-flag",
            "sha256:candidate-PRD-2",
            true,
            "human:alice",
            "same reason",
        )
        .unwrap();

    // Interactive form: the shared picker, selecting by ordinal, given the
    // same actor and reason a human would type.
    let mut input = Cursor::new(b"1\na\nhuman:alice\nsame reason\n".to_vec());
    familiar_ai_daemon::cli::scope_decisions::list_or_decide_interactively(
        &mut db,
        &identity,
        &familiar_ai_core::Config::default(),
        true,
        &mut input,
    )
    .unwrap();

    let flag_row: (Option<String>, Option<String>, Option<String>, String, String) = db
        .conn()
        .query_row(
            "SELECT decision,actor,reason,candidate_hash,checkpoint_id FROM scope_decisions WHERE finding_hash='finding-hash-flag'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    let interactive_row: (Option<String>, Option<String>, Option<String>, String, String) = db
        .conn()
        .query_row(
            "SELECT decision,actor,reason,candidate_hash,checkpoint_id FROM scope_decisions WHERE finding_hash='finding-hash-interactive'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();

    assert_eq!(flag_row.0, interactive_row.0, "same decision value");
    assert_eq!(flag_row.1, interactive_row.1, "same actor");
    assert_eq!(flag_row.2, interactive_row.2, "same reason");
    assert_eq!(flag_row.2.as_deref(), Some("same reason"));
    assert_eq!(flag_row.0.as_deref(), Some("approved"));
    assert_eq!(flag_row.1.as_deref(), Some("human:alice"));
    assert_eq!(flag_checkpoint, flag_row.4);

    // Both continuations land the checkpoint in the same post-decision
    // phase: a lone, fully approved finding moves the checkpoint on to
    // `implemented` (still owed verification/review), never `reviewed`.
    let flag_phase: String = db
        .conn()
        .query_row(
            "SELECT phase FROM execution_checkpoints WHERE checkpoint_id='cp-flag'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let interactive_phase: String = db
        .conn()
        .query_row(
            "SELECT phase FROM execution_checkpoints WHERE checkpoint_id='cp-interactive'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(flag_phase, interactive_phase);
    assert_eq!(flag_phase, "implemented");
}

// ---------------------------------------------------------------------
// Acceptance criterion 5: the approve path is discoverable without the
// source tree -- the pause text and the top-level help both name the
// deciding command.
// ---------------------------------------------------------------------

#[test]
fn top_level_help_names_scope_decisions() {
    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // PRD-090: the deciding command is now a top-level daily verb named
    // `approve` -- more discoverable than the old `scope-decisions`, which
    // still works as a recorded alias (see cli_surface.rs).
    assert!(stdout.contains("approve"), "{stdout}");
}

#[test]
fn every_scope_pause_surface_names_scope_decisions_not_only_release_or_complete() {
    // The report (already exercised above with a full pause) is the
    // canonical "surface" this criterion is about; re-assert the
    // discoverability property directly here so it is pinned next to the
    // top-level help check.
    let db = database();
    let identity = identity();
    let driver = DriverRepository::new(db.conn());
    driver.open_session("drive-1", &identity.key, "{}").unwrap();
    let sequence = driver
        .record_attempt_started("drive-1", "PRD-83", "docs/prds/PRD-83.md", Some("exec-1"))
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-1",
            sequence,
            "retained",
            Some("scope_broadened"),
            None,
            Some(5),
        )
        .unwrap();
    driver
        .finish_session("drive-1", "nothing_eligible")
        .unwrap();
    seed_checkpoint(&db, "cp-1", "PRD-83");
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &identity.key,
            "cp-1",
            "PRD-83",
            "sha256:candidate-PRD-83",
            "finding-hash-1",
            FINDING_JSON,
        )
        .unwrap();

    let report = familiar_ai_daemon::report::render(&db, None, 0).unwrap();
    assert!(report.contains("scope-decisions"));
    assert_approve_precedes_release_or_complete(&report);

    let gates =
        familiar_ai_daemon::stewardship::list_pending_human_gates(&db, &identity, 20).unwrap();
    assert!(gates.to_string().contains("scope-decisions"));
}
