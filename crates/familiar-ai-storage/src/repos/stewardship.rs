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
                 WHERE s.repository_key=?1 AND (a.outcome IS NULL OR a.outcome<>'completed') \
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
                prd_id,
                recovery_commands: vec![
                    format!(
                        "familiar-ai backlog release {prd_path} --actor human:<you> --reason \"<why>\""
                    ),
                    format!(
                        "familiar-ai backlog complete {prd_path} --actor human:<you> --reason \"<why>\""
                    ),
                ],
                prd_path,
                detail,
            });
        }
    }
    {
        let mut stmt = conn
            .prepare(
                "SELECT prd_id,prd_path,phase,invalid_reason FROM execution_checkpoints \
                 WHERE repository_key=?1 AND phase IN ('blocked','invalid_checkpoint') \
                 ORDER BY prd_id LIMIT ?2",
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
