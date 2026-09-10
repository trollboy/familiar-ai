//! Composite, repository-scoped read model over durable execution-era state
//! (PRD-035). These functions compose the existing driver, checkpoint,
//! delivery, and review repositories into the exact fact set the PRD-018
//! morning report already renders — budgets, review findings/verification,
//! and pending human gates — so CLI, MCP, and dashboard surfaces can query
//! one boundary instead of database internals.
//!
//! This module reads only; it never mutates canonical state.

use rusqlite::{params, Connection};

use familiar_ai_core::FamiliarError;
use familiar_ai_review::{ScopeDecision, ScopeFinding};

use super::delivery::DeliveryRepository;
use super::driver::DriverRepository;
use super::review::ReviewRepository;

/// Session-level budget: the driver warrant plus known/unknown cost across
/// its attempts and any delivery warrant consumed. Unknown cost is reported
/// as an explicit count, never coerced into the known total.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BudgetSummary {
    pub session_id: String,
    pub repository_key: String,
    pub warrant_json: String,
    pub known_cost_microusd: u64,
    pub known_cost_attempts: usize,
    pub unknown_cost_attempts: usize,
    pub delivery_warrant_consumed: u64,
}

/// `None` when the session does not exist.
pub fn budget_summary(
    conn: &Connection,
    session_id: &str,
) -> familiar_ai_core::Result<Option<BudgetSummary>> {
    let sessions = DriverRepository::new(conn);
    let Some(session) = sessions.get_session(session_id)? else {
        return Ok(None);
    };
    let attempts = sessions.attempts(session_id)?;
    let known_cost_microusd: u64 = attempts.iter().filter_map(|a| a.known_cost_microusd).sum();
    let known_cost_attempts = attempts
        .iter()
        .filter(|a| a.known_cost_microusd.is_some())
        .count();
    let unknown_cost_attempts = attempts.len() - known_cost_attempts;
    let delivery_warrant_consumed: u64 = DeliveryRepository::new(conn)
        .decisions_for_session(session_id)?
        .iter()
        .map(|d| d.warrant_consumed)
        .sum();
    Ok(Some(BudgetSummary {
        session_id: session.session_id,
        repository_key: session.repository_key,
        warrant_json: session.warrant_json,
        known_cost_microusd,
        known_cost_attempts,
        unknown_cost_attempts,
        delivery_warrant_consumed,
    }))
}

/// One attempt's review disposition and blocking scope findings —
/// verification and review evidence for a single completed or stopped
/// attempt within a session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReviewFindingsRow {
    pub prd_id: String,
    pub prd_path: String,
    pub execution_id: String,
    pub cycle_id: String,
    pub state: familiar_ai_review::ReviewCycleState,
    pub disposition: familiar_ai_review::ReviewDisposition,
    pub blocking_findings: Vec<ScopeFinding>,
}

/// Review findings for every attempt in one session. Returns an empty list
/// (rather than another session's findings) when the session does not exist
/// or does not belong to `repository_key` — repository isolation is
/// enforced here, not left to the caller.
pub fn review_findings_for_session(
    conn: &Connection,
    repository_key: &str,
    session_id: &str,
) -> familiar_ai_core::Result<Vec<ReviewFindingsRow>> {
    let sessions = DriverRepository::new(conn);
    let Some(session) = sessions.get_session(session_id)? else {
        return Ok(Vec::new());
    };
    if session.repository_key != repository_key {
        return Ok(Vec::new());
    }
    let attempts = sessions.attempts(session_id)?;
    let reviews = ReviewRepository::new(conn);
    let mut out = Vec::new();
    for attempt in attempts {
        let Some(execution_id) = attempt.execution_id.as_deref() else {
            continue;
        };
        let Some(cycle) = reviews.get_cycle(&format!("{execution_id}-cycle"))? else {
            continue;
        };
        let blocking_findings = cycle
            .scope_evaluations
            .last()
            .map(|evaluation| {
                evaluation
                    .findings
                    .iter()
                    .filter(|finding| {
                        !matches!(
                            finding.decision,
                            ScopeDecision::AllowedChange
                                | ScopeDecision::JustifiedExpectedFileChange
                        )
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.push(ReviewFindingsRow {
            prd_id: attempt.prd_id.clone(),
            prd_path: attempt.prd_path.clone(),
            execution_id: execution_id.to_string(),
            cycle_id: cycle.cycle_id.clone(),
            state: cycle.state,
            disposition: cycle.disposition,
            blocking_findings,
        });
    }
    Ok(out)
}

/// The commands that actually advance a stopped attempt, ordered so the
/// first one is the one an operator most likely wants.
///
/// This list used to be two entries regardless of why the attempt stopped:
/// `backlog release` (discard the work) and `backlog complete` (mark it done,
/// bypassing every gate). For a scope pause — by far the most common stop —
/// neither is right, and the command that *is* right (`scope-decisions`) was
/// never named, so the only advertised ways forward were destructive. The
/// destructive pair is still offered, but last, and only after the remedy.
fn recovery_for(detail: &str, prd_id: &str, prd_path: &str) -> Vec<String> {
    let mut commands = Vec::new();
    if detail.starts_with("scope_") {
        commands.push(
            "familiar-ai scope-decisions   # numbered picker; approve or reject each finding"
                .to_string(),
        );
    }
    if matches!(
        detail,
        "verification_failed"
            | "human_review_required"
            | "integration_failed"
            | "checkpoint_failed"
            | "malformed_output"
            | "unclassified_result"
    ) || detail.starts_with("interrupted")
    {
        commands.push(format!(
            "familiar-ai resume {prd_id}   # re-drive the retained candidate"
        ));
    }
    if detail == "integration_failed" || detail == "human_review_required" {
        commands.push(
            "familiar-ai waive --help   # required when a terminal review retains an open finding"
                .to_string(),
        );
    }
    // Destructive, therefore last and labelled.
    commands.push(format!(
        "familiar-ai backlog release {prd_path} --actor human:<you> --reason \"<why>\"   # DISCARDS the work"
    ));
    commands.push(format!(
        "familiar-ai backlog complete {prd_path} --actor human:<you> --reason \"<why>\"   # BYPASSES all gates"
    ));
    commands
}

/// One item currently awaiting a human decision: a stopped driver attempt
/// (not completed) or a checkpoint blocked/invalidated by PRD-039 recovery
/// validation, together with the exact recovery command(s) that resolve it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingGate {
    pub kind: String,
    pub session_id: Option<String>,
    pub prd_id: String,
    pub prd_path: String,
    pub detail: String,
    pub recovery_commands: Vec<String>,
}

/// A bounded, deterministically ordered snapshot of pending human gates
/// across both sources (stopped attempts, blocked checkpoints), each capped
/// independently at `limit`. This is a composite view over two collections,
/// not a single cursor-paginated one; the caller sees the count returned and
/// can lower `limit` for a smaller snapshot.
/// The PRDs actually awaiting a human decision.
///
/// A stopped attempt is evidence that something *once* needed a decision, not
/// that it still does. The backlog records what the human then decided:
/// `completed` means they accepted or force-completed it, `pending` means they
/// released it and the work was discarded. Both are settled, so both are
/// filtered out; `in_progress` and `blocked` are not, and neither is a PRD
/// with no backlog row — this excludes only what can be shown to be decided.
///
/// Without that filter this returned every attempt that ever ended in anything
/// but success, so half of "waiting on you" was work already finished and the
/// list read as a second, noisier copy of the backlog. A PRD with no backlog
/// row at all is still reported: absence is not proof that it was settled.
pub fn pending_human_gates(
    conn: &Connection,
    repository_key: &str,
    limit: usize,
) -> familiar_ai_core::Result<Vec<PendingGate>> {
    let mut gates = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT a.session_id,a.prd_id,a.prd_path,a.retained_reason,a.outcome \
                 FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id \
                 LEFT JOIN backlog_prds b \
                   ON b.prd_path=a.prd_path AND b.repository_key=s.repository_key \
                 WHERE s.repository_key=?1 AND (a.outcome IS NULL OR a.outcome<>'completed') \
                   AND (b.status IS NULL OR b.status NOT IN ('completed','pending')) \
                 ORDER BY a.started_at DESC, a.sequence DESC LIMIT ?2",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(params![repository_key, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(db)?;
        for row in rows {
            let (session_id, prd_id, prd_path, retained_reason, outcome) = row.map_err(db)?;
            let detail = retained_reason.unwrap_or_else(|| match outcome {
                None => "interrupted (attempt did not finish)".into(),
                Some(_) => "unrecorded".into(),
            });
            gates.push(PendingGate {
                kind: "stopped_attempt".into(),
                session_id: Some(session_id),
                prd_id: prd_id.clone(),
                recovery_commands: recovery_for(&detail, &prd_id, &prd_path),
                prd_path,
                detail,
            });
        }
    }
    {
        let mut stmt = conn
            .prepare(
                "SELECT c.prd_id,c.prd_path,c.phase,c.invalid_reason \
                 FROM execution_checkpoints c \
                 LEFT JOIN backlog_prds b \
                   ON b.prd_path=c.prd_path AND b.repository_key=c.repository_key \
                 WHERE c.repository_key=?1 AND c.phase IN ('blocked','invalid_checkpoint') \
                   AND (b.status IS NULL OR b.status NOT IN ('completed','pending')) \
                 ORDER BY c.prd_id LIMIT ?2",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(params![repository_key, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(db)?;
        for row in rows {
            let (prd_id, prd_path, phase, invalid_reason) = row.map_err(db)?;
            let detail = match invalid_reason {
                Some(reason) => format!("phase={phase} reason={reason}"),
                None => format!("phase={phase}"),
            };
            gates.push(PendingGate {
                kind: "blocked_checkpoint".into(),
                session_id: None,
                recovery_commands: vec![format!("familiar-ai resume {prd_id}")],
                prd_id,
                prd_path,
                detail,
            });
        }
    }
    Ok(gates)
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::checkpoint::{CheckpointRepository, ExecutionCheckpoint};
    use crate::Database;

    fn database() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    #[test]
    fn budget_summary_separates_known_and_unknown_cost() {
        let db = database();
        let driver = DriverRepository::new(db.conn());
        driver
            .open_session("session-1", "/repo/.git", r#"{"max_prds":2}"#)
            .unwrap();
        let a = driver
            .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
            .unwrap();
        driver
            .record_attempt_finished("session-1", a, "completed", None, Some(1_000), Some(10))
            .unwrap();
        let b = driver
            .record_attempt_started("session-1", "PRD-2", "docs/prds/PRD-2.md", Some("exec-2"))
            .unwrap();
        driver
            .record_attempt_finished(
                "session-1",
                b,
                "retained",
                Some("review_disabled"),
                None,
                Some(5),
            )
            .unwrap();
        DeliveryRepository::new(db.conn())
            .record_authority_decision(
                "d1",
                "/repo/.git",
                "session-1",
                "PRD-1",
                "manual",
                "human:tester",
                "approved",
                None,
                "[]",
                "[]",
                None,
                7,
            )
            .unwrap();

        let summary = budget_summary(db.conn(), "session-1").unwrap().unwrap();
        assert_eq!(summary.known_cost_microusd, 1_000);
        assert_eq!(summary.known_cost_attempts, 1);
        assert_eq!(summary.unknown_cost_attempts, 1);
        assert_eq!(summary.delivery_warrant_consumed, 7);
        assert_eq!(summary.repository_key, "/repo/.git");

        assert!(budget_summary(db.conn(), "nope").unwrap().is_none());
    }

    /// A stopped attempt whose PRD the human has since settled is history, not
    /// a decision waiting to be made. Before this filter, 8 of the 16 PRDs in
    /// the owner's "waiting on you" list were already completed.
    #[test]
    fn pending_human_gates_omits_prds_whose_decision_was_already_made() {
        let db = database();
        let driver = DriverRepository::new(db.conn());
        driver.open_session("session-1", "/repo/.git", "{}").unwrap();

        // Same stopped outcome for all three; only the backlog status differs.
        for (n, status) in [(1, "completed"), (2, "pending"), (3, "in_progress")] {
            let path = format!("docs/prds/PRD-{n}.md");
            let attempt = driver
                .record_attempt_started("session-1", &format!("PRD-{n}"), &path, None)
                .unwrap();
            driver
                .record_attempt_finished(
                    "session-1",
                    attempt,
                    "retained",
                    Some("scope_broadened"),
                    None,
                    Some(5),
                )
                .unwrap();
            db.conn()
                .execute(
                    "INSERT INTO backlog_prds (repository_key,prd_path,prd_number,content_hash,\
                     status,discovered_at,last_seen_at,created_at,updated_at) \
                     VALUES ('/repo/.git',?1,?2,'hash',?3,'t','t','t','t')",
                    rusqlite::params![path, n as i64, status],
                )
                .unwrap();
        }

        let gates = pending_human_gates(db.conn(), "/repo/.git", 10).unwrap();
        let prds: Vec<&str> = gates.iter().map(|g| g.prd_id.as_str()).collect();
        assert_eq!(prds, vec!["PRD-3"], "only the undecided PRD is waiting");
    }

    /// A blocked checkpoint for a PRD the human already completed is likewise
    /// finished business.
    #[test]
    fn pending_human_gates_omits_blocked_checkpoints_for_settled_prds() {
        let db = database();
        let checkpoints = CheckpointRepository::new(db.conn());
        checkpoints
            .put(&ExecutionCheckpoint {
                checkpoint_id: "cp-1".into(),
                repository_key: "/repo/.git".into(),
                prd_id: "PRD-1".into(),
                prd_path: "docs/prds/PRD-1.md".into(),
                execution_id: None,
                phase: "blocked".into(),
                base_revision: "deadbeef".into(),
                worktree_path: "/state/worktrees/PRD-1".into(),
                branch_name: None,
                diff_hash: "sha256:abc".into(),
                changed_files_json: "[]".into(),
                agent_identity: "claude-code".into(),
                usage_json: "{}".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: None,
            })
            .unwrap();
        assert_eq!(pending_human_gates(db.conn(), "/repo/.git", 10).unwrap().len(), 1);

        db.conn()
            .execute(
                "INSERT INTO backlog_prds (repository_key,prd_path,prd_number,content_hash,\
                 status,discovered_at,last_seen_at,created_at,updated_at) \
                 VALUES ('/repo/.git','docs/prds/PRD-1.md',1,'hash','completed','t','t','t','t')",
                [],
            )
            .unwrap();
        assert!(pending_human_gates(db.conn(), "/repo/.git", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_human_gates_combines_stopped_attempts_and_blocked_checkpoints() {
        let db = database();
        let driver = DriverRepository::new(db.conn());
        driver
            .open_session("session-1", "/repo/.git", "{}")
            .unwrap();
        let a = driver
            .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
            .unwrap();
        driver
            .record_attempt_finished(
                "session-1",
                a,
                "retained",
                Some("scope_broadened"),
                None,
                Some(5),
            )
            .unwrap();
        // A different repository's stopped attempt must never leak in.
        driver.open_session("other-1", "/other/.git", "{}").unwrap();
        let c = driver
            .record_attempt_started("other-1", "PRD-9", "docs/prds/PRD-9.md", Some("exec-9"))
            .unwrap();
        driver
            .record_attempt_finished(
                "other-1",
                c,
                "retained",
                Some("scope_broadened"),
                None,
                Some(5),
            )
            .unwrap();

        let checkpoints = CheckpointRepository::new(db.conn());
        checkpoints
            .put(&ExecutionCheckpoint {
                checkpoint_id: "cp-1".into(),
                repository_key: "/repo/.git".into(),
                prd_id: "PRD-2".into(),
                prd_path: "docs/prds/PRD-2.md".into(),
                execution_id: Some("exec-2".into()),
                phase: "blocked".into(),
                base_revision: "deadbeef".into(),
                worktree_path: "/state/worktrees/PRD-2".into(),
                branch_name: Some("familiar/PRD-2".into()),
                diff_hash: "sha256:abc".into(),
                changed_files_json: "[]".into(),
                agent_identity: "claude-code".into(),
                usage_json: "{}".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: Some("dirty worktree".into()),
            })
            .unwrap();

        let gates = pending_human_gates(db.conn(), "/repo/.git", 10).unwrap();
        assert_eq!(gates.len(), 2);
        assert!(gates
            .iter()
            .any(|g| g.kind == "stopped_attempt" && g.prd_id == "PRD-1"));
        assert!(gates
            .iter()
            .any(|g| g.kind == "blocked_checkpoint" && g.prd_id == "PRD-2"));
        assert!(gates.iter().all(|g| !g.recovery_commands.is_empty()));
    }
}
