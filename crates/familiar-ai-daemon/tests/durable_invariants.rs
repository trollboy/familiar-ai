//! PRD-087: identity and event-sequence invariants that used to exist only
//! in prose. Each invariant gets a regression that exercises the exact
//! violating shape a prior bug produced, so removing the enforcement fails
//! this test, not only a scenario test. This file also owns the ledger-to-
//! invariant linkage check (`docs/running_bugs.md`), so the doc and the code
//! cannot silently drift apart.

use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_core::repository_path::INVARIANT_REPOSITORY_IDENTITY;
use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};
use familiar_ai_daemon::worktree::WorktreeLease;
use familiar_ai_review::{BlockingPolicy, ReviewTask};
use familiar_ai_storage::{
    CheckpointRepository, Database, ExecutionCheckpoint, ReviewRepository,
    INVARIANT_CHECKPOINT_EVENT_SEQUENCE, INVARIANT_REVIEW_RECOVERY_TOLERANCE,
};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ---------------------------------------------------------------------
// Invariant 1: repository-identity-single-mint (PRD-087 AC1)
// ---------------------------------------------------------------------
//
// Repository identity is minted in exactly one place
// (`familiar_ai_core::repository_path::repository_origin_key`). Before this
// PRD, backlog discovery, per-repository config matching, and accounting
// evidence each shelled out to `git rev-parse --git-common-dir`
// independently. This regression drives two concurrent worktrees of one
// repository and asserts a single identity across every durable row both
// produce; if a future change reintroduces an independent computation that
// derives a worktree-specific key instead of the shared origin, this test
// fails because the two worktrees would mint different keys and the
// cross-worktree checkpoint lookup below would come back empty.
#[test]
fn repository_identity_is_one_key_across_two_concurrent_worktrees() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("file"), "base").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);

    let state = temp.path().join("state");
    let lease_a = WorktreeLease::create(&repo, &state, "session-a", "PRD-1").unwrap();
    let lease_b = WorktreeLease::create(&repo, &state, "session-b", "PRD-1").unwrap();

    let main_identity = FilesystemBacklogDiscovery.resolve(&repo).unwrap();
    let identity_a = FilesystemBacklogDiscovery.resolve(lease_a.path()).unwrap();
    let identity_b = FilesystemBacklogDiscovery.resolve(lease_b.path()).unwrap();
    assert_eq!(
        main_identity.key, identity_a.key,
        "{INVARIANT_REPOSITORY_IDENTITY} violated: identity derived inside worktree a must resolve to the origin repository's key"
    );
    assert_eq!(
        main_identity.key, identity_b.key,
        "{INVARIANT_REPOSITORY_IDENTITY} violated: identity derived inside worktree b must resolve to the origin repository's key"
    );

    // The other former minting sites (worktree-origin resolution, the
    // advisory config-matching wrapper) must agree — they now delegate to
    // the same function instead of computing it independently.
    assert_eq!(
        lease_a.origin_repository_key(),
        Some(main_identity.key.clone())
    );
    assert_eq!(
        lease_b.origin_repository_key(),
        Some(main_identity.key.clone())
    );
    assert_eq!(
        familiar_ai_core::repository_path::git_common_directory(lease_a.path()),
        Some(main_identity.key.clone())
    );

    // A single identity across every durable row both worktrees produce: a
    // checkpoint frozen using worktree a's resolved key is visible through
    // worktree b's resolved key.
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let checkpoints = CheckpointRepository::new(db.conn());
    checkpoints
        .put(&ExecutionCheckpoint {
            checkpoint_id: "checkpoint-1".into(),
            repository_key: identity_a.key.clone(),
            prd_id: "PRD-1".into(),
            prd_path: "docs/prds/PRD-1.md".into(),
            execution_id: Some("exec-1".into()),
            phase: "implemented".into(),
            base_revision: "deadbeef".into(),
            worktree_path: lease_a.path().display().to_string(),
            branch_name: Some(lease_a.branch().into()),
            diff_hash: "sha256:one".into(),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        })
        .unwrap();
    let seen_via_b = checkpoints.get(&identity_b.key, "PRD-1").unwrap();
    assert!(
        seen_via_b.is_some(),
        "a checkpoint frozen under worktree a's identity must be visible under worktree b's identity"
    );
    assert_eq!(seen_via_b.unwrap().checkpoint_id, "checkpoint-1");
}

// ---------------------------------------------------------------------
// Invariant 2: checkpoint-event-sequence-allocator (PRD-087 AC2)
// ---------------------------------------------------------------------
//
// Replays the FAM-BUG-039 shape: one checkpoint re-entered across
// occurrences (a re-freeze after remediation revisits phases it already
// recorded). Every event id is minted through
// `familiar_ai_storage::next_checkpoint_event_id`; if a future change
// reverts to a per-lifetime id (e.g. `{checkpoint_id}:{phase}`, dropping the
// sequence), the second occurrence's insert collides on the `event_id`
// primary key or the `(checkpoint_id, sequence)` unique index and this test
// fails with a database error instead of silently losing an event.
#[test]
fn reentered_checkpoint_never_collides_or_drops_events() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let checkpoints = CheckpointRepository::new(db.conn());

    let base = ExecutionCheckpoint {
        checkpoint_id: "checkpoint-occurrence-1".into(),
        repository_key: "/repo/.git".into(),
        prd_id: "PRD-39".into(),
        prd_path: "docs/prds/PRD-39.md".into(),
        execution_id: Some("exec-1".into()),
        phase: "implemented".into(),
        base_revision: "deadbeef".into(),
        worktree_path: "/state/worktrees/PRD-39".into(),
        branch_name: Some("familiar/PRD-39".into()),
        diff_hash: "sha256:first".into(),
        changed_files_json: "[]".into(),
        agent_identity: "claude-code".into(),
        usage_json: "{}".into(),
        test_evidence_json: "{}".into(),
        invalid_reason: None,
    };

    // First occurrence: created, then transitions through verified/reviewed.
    checkpoints.put(&base).unwrap();
    checkpoints
        .transition("checkpoint-occurrence-1", "verified", "first pass")
        .unwrap();
    checkpoints
        .transition("checkpoint-occurrence-1", "reviewed", "first pass")
        .unwrap();

    // Second occurrence: a re-freeze (remediation) revisits the SAME
    // phases. The checkpoint identity survives (FAM-BUG-037); this must not
    // collide with, or silently skip, the first occurrence's events
    // (FAM-BUG-039).
    let mut second = base.clone();
    second.diff_hash = "sha256:remediated".into();
    checkpoints.put(&second).unwrap_or_else(|e| {
        panic!("{INVARIANT_CHECKPOINT_EVENT_SEQUENCE} violated on re-entry: {e}")
    });
    checkpoints
        .transition("checkpoint-occurrence-1", "verified", "second pass")
        .unwrap_or_else(|e| panic!("{INVARIANT_CHECKPOINT_EVENT_SEQUENCE} violated: second occurrence's 'verified' event collided with the first's: {e}"));
    checkpoints
        .transition("checkpoint-occurrence-1", "reviewed", "second pass")
        .unwrap_or_else(|e| panic!("{INVARIANT_CHECKPOINT_EVENT_SEQUENCE} violated: second occurrence's 'reviewed' event collided with the first's: {e}"));

    let events = checkpoints.events("checkpoint-occurrence-1").unwrap();
    // created + (verified, reviewed) x 2 occurrences = 5 events, none dropped.
    assert_eq!(
        events.len(),
        5,
        "{INVARIANT_CHECKPOINT_EVENT_SEQUENCE} violated: no event may be dropped across occurrences: {events:?}"
    );
    let verified_count = events
        .iter()
        .filter(|(phase, _)| phase == "verified")
        .count();
    let reviewed_count = events
        .iter()
        .filter(|(phase, _)| phase == "reviewed")
        .count();
    assert_eq!(
        verified_count, 2,
        "both occurrences' verified events must survive: {events:?}"
    );
    assert_eq!(
        reviewed_count, 2,
        "both occurrences' reviewed events must survive: {events:?}"
    );

    // The underlying ids are distinct, unique sequenced allocations —
    // proves this is not merely tolerated by chance.
    let ids: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(DISTINCT event_id) FROM execution_checkpoint_events WHERE checkpoint_id='checkpoint-occurrence-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        ids, 5,
        "every event id minted by {INVARIANT_CHECKPOINT_EVENT_SEQUENCE} must be distinct"
    );
}

// ---------------------------------------------------------------------
// Invariant 3: review-recovery-schema-tolerance (PRD-087 AC3)
// ---------------------------------------------------------------------
//
// Opens a database populated at the pre-migration schema (a review cycle
// row whose JSON predates the `repository_key` field, exactly FAM-BUG-032's
// shape) and asserts review recovery accepts its own rows rather than
// refusing them. If recovery regresses to replaying the row through the
// full `save_cycle` path (which refuses keyless verification evidence),
// this test fails with a database error instead of silently wedging.
#[test]
fn review_recovery_accepts_rows_written_before_repository_key_existed() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repository = ReviewRepository::new(db.conn());
    let task = ReviewTask {
        task_id: "task".into(),
        objective: "objective".into(),
        acceptance_criteria: vec!["criterion".into()],
        base_revision: "tree".into(),
        allowed_paths: vec!["src/".into()],
        prohibited_changes: vec![],
        verification_plan_id: "checks".into(),
    };
    repository
        .insert_task(&task, &BlockingPolicy::default())
        .unwrap();

    // A cycle written before `repository_key` existed on `ReviewCycle`:
    // `#[serde(default)]` deserializes it to "", the pre-invariant shape.
    let legacy = serde_json::json!({
        "cycle_id": "pre-migration-cycle",
        "task_id": "task",
        "attempt": 1,
        "state": "awaiting_review",
        "implementation": {
            "assignment": {
                "adapter_id": "fake",
                "agent_id": "fake",
                "provider": null,
                "requested_model": null,
                "role": "implementation",
                "session_id": null
            },
            "agent_version": null,
            "reported_model": null,
            "unavailable_fields": {}
        },
        "implementation_execution": null,
        "reviewer": null,
        "independence": null,
        "review_request": null,
        "review_result": null,
        "remediation_request": null,
        "remediation_result": null,
        "verification_before_review": [],
        "verification_after_remediation": [],
        "verification_history": [{
            "check_id": "tests",
            "argv": ["/usr/bin/true"],
            "working_directory": ".",
            "environment_identity": {},
            "tool_identity": null,
            "tested_identity": "tree",
            "started_at": "2026-08-03T00:00:00Z",
            "ended_at": "2026-08-03T00:00:01Z",
            "duration_ms": 1000,
            "exit_code": 0,
            "signal": null,
            "status": "passed",
            "required": true,
            "summary": "ok",
            "stdout": null,
            "stderr": null,
            "truncated": false
        }],
        "aggregate_usage": familiar_ai_review::ExecutionUsage::default(),
        "aggregate_duration_ms": 0,
        "started_at": "2026-08-03T00:00:00Z",
        "ended_at": null,
        "disposition": "pending",
        "stop_reasons": [],
        "review_attempts": [],
        "remediation_attempts": []
    });
    db.conn()
        .execute(
            "INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at,ended_at) VALUES('pre-migration-cycle','task',1,'awaiting_review','pending',?1,'2026-08-03T00:00:00Z',NULL)",
            rusqlite::params![legacy.to_string()],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json,repository_key,environment_identity_json,classification) VALUES('pre-migration-cycle','tests','attempt-0','{}','old/repo/.git','{}','passed')",
            [],
        )
        .unwrap();

    let recovered = repository.recover_incomplete().unwrap_or_else(|e| {
        panic!("{INVARIANT_REVIEW_RECOVERY_TOLERANCE} violated: recovery refused its own pre-invariant row instead of tolerating it: {e}")
    });
    assert_eq!(
        recovered, 1,
        "{INVARIANT_REVIEW_RECOVERY_TOLERANCE} violated: recovery must accept its own pre-invariant row, not refuse it"
    );

    let evidence: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM review_verification_evidence WHERE cycle_id='pre-migration-cycle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        evidence, 1,
        "recovery must not delete evidence it can no longer derive"
    );

    // The tolerance is a durable, recorded fact (PRD-087 design: "migrated
    // forward with a recorded migration", never silently accepted).
    let (invariant, row_key): (String, String) = db
        .conn()
        .query_row(
            "SELECT invariant,row_key FROM identity_invariant_tolerances WHERE table_name='review_cycles'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(invariant, INVARIANT_REVIEW_RECOVERY_TOLERANCE);
    assert_eq!(row_key, "pre-migration-cycle");
}

// ---------------------------------------------------------------------
// Ledger-to-invariant linkage (PRD-087 AC5)
// ---------------------------------------------------------------------
//
// `docs/running_bugs.md` carries a "### Invariant coverage ledger" section
// linking every prior entry in these three families to the invariant that
// now covers it, or recording why it stays uncovered. This test hardcodes
// the same table the doc carries; if either drifts without the other, this
// fails — the linkage cannot silently rot.
const LEDGER_LINKAGE: &[(&str, Option<&str>)] = &[
    ("FAM-BUG-040", Some(INVARIANT_REPOSITORY_IDENTITY)),
    ("FAM-BUG-037", Some(INVARIANT_CHECKPOINT_EVENT_SEQUENCE)),
    ("FAM-BUG-039", Some(INVARIANT_CHECKPOINT_EVENT_SEQUENCE)),
    ("FAM-BUG-032", Some(INVARIANT_REVIEW_RECOVERY_TOLERANCE)),
    ("FAM-BUG-016", None),
    ("FAM-BUG-026", None),
    ("FAM-BUG-041", None),
    ("FAM-BUG-035", None),
    ("FAM-BUG-051", None),
    ("FAM-BUG-018", None),
    ("FAM-BUG-021", None),
];

#[test]
fn ledger_invariant_linkage_matches_the_live_invariant_names() {
    let ledger = std::fs::read_to_string(repo_root().join("docs/running_bugs.md")).unwrap();
    let section_start = ledger
        .find("### Invariant coverage ledger")
        .expect("running_bugs.md must carry the PRD-087 invariant coverage ledger section");
    let section = &ledger[section_start..];

    for (bug_id, expected) in LEDGER_LINKAGE {
        // The referenced bug must actually exist elsewhere in the ledger —
        // catches a typo'd or renamed id.
        assert!(
            ledger.matches(&format!("### {bug_id} ")).count() >= 1,
            "{bug_id} is referenced in the invariant coverage ledger but has no entry of its own"
        );
        match expected {
            Some(invariant) => {
                let needle = format!("- {bug_id}: covered by {invariant}");
                assert!(
                    section.contains(&needle),
                    "expected '{needle}' in the invariant coverage ledger section"
                );
            }
            None => {
                let needle = format!("- {bug_id}: uncovered:");
                assert!(
                    section.contains(&needle),
                    "expected '{needle}' in the invariant coverage ledger section"
                );
            }
        }
    }

    // Every "covered by" line in the section names a name a live invariant
    // constant — never a stale or hand-typed string.
    let known_invariants = [
        INVARIANT_REPOSITORY_IDENTITY,
        INVARIANT_CHECKPOINT_EVENT_SEQUENCE,
        INVARIANT_REVIEW_RECOVERY_TOLERANCE,
    ];
    for line in section.lines().filter(|line| line.contains("covered by")) {
        assert!(
            known_invariants
                .iter()
                .any(|invariant| line.contains(invariant)),
            "line '{line}' names an invariant that is not one of the three live constants"
        );
    }
}
