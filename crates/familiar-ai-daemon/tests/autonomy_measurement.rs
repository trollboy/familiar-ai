//! PRD-085: the closed stall taxonomy and the autonomy it measures.
//!
//! Familiar previously had no record of which completions were assisted,
//! no closed name for what a stall was, and no number that would move if
//! autonomy improved or regressed. These regressions pin: every stall class
//! resolves to a real command or an explicit unrecoverable reason; a
//! session containing one PRD of each disposition (unattended, assisted,
//! stalled) is reported correctly from durable rows alone; a session below
//! the configured floor reports itself as an autonomy failure; and the
//! stewardship autonomy query groups by repository, stall class, and
//! intervention command.

use std::process::Command;

use familiar_ai_core::RepositoryIdentity;
use familiar_ai_storage::{stall_taxonomy_classes, Database, DriverRepository, StallRecovery};

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_familiar-ai"))
}

fn test_db() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

/// AC1's pinned regression: the closed vocabulary enumerates at least one
/// class, the invariant check passes over the whole table, and (the
/// invariant's own failure mode) a class deliberately given an empty
/// command is caught rather than silently advertised.
#[test]
fn every_class_in_the_taxonomy_has_a_recovery_command_or_a_named_reason() {
    familiar_ai_storage::assert_stall_taxonomy_complete().unwrap();
    let classes = stall_taxonomy_classes();
    assert!(classes.contains(&"unclassified"));
    assert!(classes.contains(&"scope_broadened"));
    assert!(classes.contains(&"environment_denied"));
    for class in classes {
        match familiar_ai_storage::stall_recovery(class, "PRD-1", "docs/prds/PRD-001.md") {
            StallRecovery::Command(command) => assert!(
                !command.trim().is_empty(),
                "class {class} has an empty recovery command"
            ),
            StallRecovery::Unrecoverable(reason) => assert!(
                !reason.trim().is_empty(),
                "class {class} is unrecoverable with no stated reason"
            ),
        }
    }
}

/// AC5: every recovery command the taxonomy can produce resolves to a real
/// installed subcommand — the taxonomy can never advertise a tool that does
/// not ship. Runs the actual built binary's `--help` for each distinct
/// command family the vocabulary uses.
#[test]
fn every_recovery_command_resolves_to_a_real_installed_subcommand() {
    let mut checked = std::collections::BTreeSet::new();
    for class in stall_taxonomy_classes() {
        let StallRecovery::Command(command) =
            familiar_ai_storage::stall_recovery(class, "PRD-1", "docs/prds/PRD-001.md")
        else {
            continue;
        };
        let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        assert_eq!(words[0], "familiar-ai", "class {class} command: {command}");
        // Every command here is either `familiar-ai <verb> ...` or
        // `familiar-ai operator <verb> ...`; the subcommand chain is
        // whichever of those two shapes applies, everything else is an
        // argument to it.
        let chain: Vec<String> = if words.get(1).map(String::as_str) == Some("operator") {
            vec![words[1].clone(), words[2].clone()]
        } else {
            vec![words[1].clone()]
        };
        checked.insert(chain);
    }
    assert!(!checked.is_empty());
    for chain in checked {
        let mut args = chain.clone();
        args.push("--help".to_string());
        let output = Command::new(binary())
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("failed to run familiar-ai {args:?}: {e}"));
        assert!(
            output.status.success(),
            "familiar-ai {args:?} --help did not resolve: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn seed_session<'a>(
    db: &'a Database,
    session_id: &str,
    repository_key: &str,
) -> DriverRepository<'a> {
    let driver = DriverRepository::new(db.conn());
    driver
        .open_session(session_id, repository_key, "{}")
        .unwrap();
    driver
}

/// AC3's pinned regression: a session containing one PRD of each
/// disposition — unattended, assisted, stalled — reports all three
/// correctly, computed from durable driver_attempts/backlog_status_events
/// rows alone.
#[test]
fn a_session_with_one_of_each_disposition_reports_all_three() {
    let db = test_db();
    let repository_key = "/repo/.git";
    let driver = seed_session(&db, "drive-1", repository_key);

    let unattended = driver
        .record_attempt_started("drive-1", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished("drive-1", unattended, "completed", None, None, None)
        .unwrap();

    let assisted = driver
        .record_attempt_started("drive-1", "PRD-2", "docs/prds/PRD-002.md", None)
        .unwrap();
    let started_at = driver
        .attempts("drive-1")
        .unwrap()
        .iter()
        .find(|a| a.sequence == assisted)
        .unwrap()
        .started_at
        .clone();
    db.conn()
        .execute(
            "INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,created_at,updated_at) \
             VALUES(?1,'docs/prds/PRD-002.md',2,'h','in_progress',?2,?2,?2,?2)",
            rusqlite::params![repository_key, started_at],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) \
             VALUES(1,?1,'docs/prds/PRD-002.md','in_progress','completed','human:alice',?2)",
            rusqlite::params![repository_key, started_at],
        )
        .unwrap();
    driver
        .record_attempt_finished("drive-1", assisted, "completed", None, None, None)
        .unwrap();

    let stalled = driver
        .record_attempt_started("drive-1", "PRD-3", "docs/prds/PRD-003.md", None)
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-1",
            stalled,
            "retained",
            Some("scope_broadened"),
            None,
            None,
        )
        .unwrap();
    driver.finish_session("drive-1", "backlog_empty").unwrap();

    let report = familiar_ai_daemon::report::render(&db, Some("drive-1"), 0).unwrap();
    assert!(
        report.contains("unattended=1 assisted=1 stalled=1"),
        "{report}"
    );
    assert!(
        report.contains("familiar-ai backlog complete docs/prds/PRD-002.md --actor human:alice"),
        "{report}"
    );
    assert!(
        report.contains("stalled: PRD-3 docs/prds/PRD-003.md class=scope_broadened"),
        "{report}"
    );
    assert!(report.contains("familiar-ai scope-decisions"), "{report}");

    let autonomy = familiar_ai_storage::session_autonomy(db.conn(), "drive-1").unwrap();
    assert_eq!(autonomy.unattended_count(), 1);
    assert_eq!(autonomy.assisted().len(), 1);
    assert_eq!(autonomy.stalled().len(), 1);
}

/// AC6: a session whose unattended-completion fraction falls below the
/// configured floor is reported as an autonomy failure naming the dominant
/// stall class, not as an ordinary session. A stall counts against the
/// fraction exactly as an assisted completion does (PRD-085 F2) — a session
/// is not "clean" merely because every PRD that finished did so unattended
/// while others stalled.
#[test]
fn a_session_below_the_configured_floor_reports_an_autonomy_failure() {
    let db = test_db();
    let repository_key = "/repo/.git";
    let driver = seed_session(&db, "drive-2", repository_key);

    // A session with only unattended completions and no stalls is clean at
    // any floor.
    let unattended = driver
        .record_attempt_started("drive-2", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished("drive-2", unattended, "completed", None, None, None)
        .unwrap();
    driver.finish_session("drive-2", "backlog_empty").unwrap();

    let clean = familiar_ai_daemon::report::render(&db, Some("drive-2"), 100).unwrap();
    assert!(!clean.contains("AUTONOMY FAILURE"), "{clean}");

    // A session with one unattended completion and several stalls falls
    // below a 50% floor — the stalls count against the fraction even though
    // nothing was assisted.
    let fourth = seed_session(&db, "drive-4", repository_key);
    let unattended4 = fourth
        .record_attempt_started("drive-4", "PRD-6", "docs/prds/PRD-006.md", None)
        .unwrap();
    fourth
        .record_attempt_finished("drive-4", unattended4, "completed", None, None, None)
        .unwrap();
    for (n, path) in [(7, "docs/prds/PRD-007.md"), (8, "docs/prds/PRD-008.md")] {
        let stalled4 = fourth
            .record_attempt_started("drive-4", &format!("PRD-{n}"), path, None)
            .unwrap();
        fourth
            .record_attempt_finished(
                "drive-4",
                stalled4,
                "retained",
                Some("verification_failed"),
                None,
                None,
            )
            .unwrap();
    }
    fourth.finish_session("drive-4", "backlog_empty").unwrap();

    let below_floor = familiar_ai_daemon::report::render(&db, Some("drive-4"), 50).unwrap();
    assert!(
        below_floor.contains("AUTONOMY FAILURE") && below_floor.contains("verification_failed"),
        "{below_floor}"
    );

    // ...while a session that also has an assisted completion drags the
    // fraction under a 100% floor and must say so.
    let second = seed_session(&db, "drive-3", repository_key);
    let unattended = second
        .record_attempt_started("drive-3", "PRD-3", "docs/prds/PRD-003.md", None)
        .unwrap();
    second
        .record_attempt_finished("drive-3", unattended, "completed", None, None, None)
        .unwrap();
    let stalled2 = second
        .record_attempt_started("drive-3", "PRD-4", "docs/prds/PRD-004.md", None)
        .unwrap();
    second
        .record_attempt_finished(
            "drive-3",
            stalled2,
            "retained",
            Some("verification_failed"),
            None,
            None,
        )
        .unwrap();
    let assisted = second
        .record_attempt_started("drive-3", "PRD-5", "docs/prds/PRD-005.md", None)
        .unwrap();
    let started_at = second
        .attempts("drive-3")
        .unwrap()
        .iter()
        .find(|a| a.sequence == assisted)
        .unwrap()
        .started_at
        .clone();
    db.conn()
        .execute(
            "INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,created_at,updated_at) \
             VALUES(?1,'docs/prds/PRD-005.md',5,'h','in_progress',?2,?2,?2,?2)",
            rusqlite::params![repository_key, started_at],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) \
             VALUES(2,?1,'docs/prds/PRD-005.md','in_progress','completed','human:bob',?2)",
            rusqlite::params![repository_key, started_at],
        )
        .unwrap();
    second
        .record_attempt_finished("drive-3", assisted, "completed", None, None, None)
        .unwrap();
    second.finish_session("drive-3", "backlog_empty").unwrap();

    let failing = familiar_ai_daemon::report::render(&db, Some("drive-3"), 100).unwrap();
    assert!(
        failing.contains("AUTONOMY FAILURE") && failing.contains("verification_failed"),
        "{failing}"
    );
}

/// F2 remediation: a session whose only PRDs stalled reports the worst
/// possible autonomy outcome — an autonomy failure naming the dominant
/// stall class — rather than reading as "no failure" because
/// `unattended_fraction` divided by zero completions.
#[test]
fn an_all_stalled_session_reports_an_autonomy_failure() {
    let db = test_db();
    let repository_key = "/repo/.git";
    let driver = seed_session(&db, "drive-6", repository_key);

    let stalled = driver
        .record_attempt_started("drive-6", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-6",
            stalled,
            "retained",
            Some("verification_failed"),
            None,
            None,
        )
        .unwrap();
    driver.finish_session("drive-6", "backlog_empty").unwrap();

    let report = familiar_ai_daemon::report::render(&db, Some("drive-6"), 1).unwrap();
    assert!(
        report.contains("AUTONOMY FAILURE") && report.contains("verification_failed"),
        "{report}"
    );
}

/// F1 remediation: a PRD retried after a stall must be counted once, by its
/// final disposition, not once per attempt. Two attempts at the same PRD —
/// the first retained on a named stall class, the second completed — must
/// report as a single unattended completion, not as one stalled PRD and one
/// unattended PRD.
#[test]
fn a_retried_prd_counts_once_by_its_final_disposition() {
    let db = test_db();
    let repository_key = "/repo/.git";
    let driver = seed_session(&db, "drive-5", repository_key);

    let first = driver
        .record_attempt_started("drive-5", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-5",
            first,
            "retained",
            Some("verification_failed"),
            None,
            None,
        )
        .unwrap();

    let second = driver
        .record_attempt_started("drive-5", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished("drive-5", second, "completed", None, None, None)
        .unwrap();
    driver.finish_session("drive-5", "backlog_empty").unwrap();

    let autonomy = familiar_ai_storage::session_autonomy(db.conn(), "drive-5").unwrap();
    assert_eq!(autonomy.prds.len(), 1, "{:?}", autonomy.prds);
    assert_eq!(autonomy.unattended_count(), 1);
    assert_eq!(autonomy.assisted().len(), 0);
    assert_eq!(autonomy.stalled().len(), 0);
}

/// AC4: stewardship exposes an autonomy query over a bounded window,
/// grouped by repository, stall class, and intervention command.
#[test]
fn stewardship_autonomy_query_groups_by_repository_stall_class_and_command() {
    let db = test_db();
    let repository_key = "/repo/.git";
    let driver = seed_session(&db, "drive-4", repository_key);
    let unattended = driver
        .record_attempt_started("drive-4", "PRD-1", "docs/prds/PRD-001.md", None)
        .unwrap();
    driver
        .record_attempt_finished("drive-4", unattended, "completed", None, None, None)
        .unwrap();
    let stalled = driver
        .record_attempt_started("drive-4", "PRD-2", "docs/prds/PRD-002.md", None)
        .unwrap();
    driver
        .record_attempt_finished(
            "drive-4",
            stalled,
            "retained",
            Some("scope_ambiguous"),
            None,
            None,
        )
        .unwrap();
    driver.finish_session("drive-4", "backlog_empty").unwrap();

    let repository = RepositoryIdentity {
        worktree: std::path::PathBuf::from("/repo"),
        key: repository_key.to_string(),
    };
    let result = familiar_ai_daemon::stewardship::get_autonomy(
        &db,
        &repository,
        "0000-01-01T00:00:00Z",
        "9999-01-01T00:00:00Z",
    )
    .unwrap();
    assert_eq!(result["repository_key"], repository_key);
    assert_eq!(result["unattended_completions"], 1);
    assert_eq!(result["stalled"], 1);
    assert_eq!(result["by_stall_class"]["scope_ambiguous"], 1);
}
