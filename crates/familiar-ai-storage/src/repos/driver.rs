//! Durable records for unattended driver sessions: what ran, in what order,
//! with what outcome, cost, and duration — written around every attempt so an
//! interrupted session stays reconstructible.

use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriverSession {
    pub session_id: String,
    pub repository_key: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub termination_reason: Option<String>,
    pub termination_detail: Option<String>,
    pub warrant_json: String,
}

/// One PRD's appearance in one round, which is all the rounds view needs.
///
/// Deliberately not a `DriverAttempt`: that carries twenty-odd columns about
/// how an attempt was configured, and a chart that reads every one of them for
/// every PRD in the backlog pays for data it never draws.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoundAttempt {
    pub session_id: String,
    pub sequence: i64,
    pub prd_id: String,
    pub prd_path: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// `completed`, `retained`, or absent while the attempt is still running.
    pub outcome: Option<String>,
    pub retained_reason: Option<String>,
    /// The stopping error in its own words, when there was one. Never a
    /// taxonomy token — that is `retained_reason`'s job.
    pub retained_detail: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriverAttempt {
    pub sequence: i64,
    pub prd_id: String,
    pub prd_path: String,
    pub execution_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: Option<String>,
    pub retained_reason: Option<String>,
    /// The stopping error in its own words, when there was one. Never a
    /// taxonomy token — that is `retained_reason`'s job.
    pub retained_detail: Option<String>,
    pub known_cost_microusd: Option<u64>,
    pub duration_ms: Option<u64>,
    pub adapter_id: Option<String>,
    pub model: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub last_durable_phase: Option<String>,
    pub review_configuration_source: String,
    pub execution_context_configuration_source: String,
    pub component_id: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub escalated_from_sequence: Option<i64>,
    pub escalation_reason: Option<String>,
}

pub struct DriverRepository<'a> {
    conn: &'a Connection,
}

impl<'a> DriverRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn open_session(
        &self,
        session_id: &str,
        repository_key: &str,
        warrant_json: &str,
    ) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO driver_sessions(session_id,repository_key,started_at,warrant_json,created_at) \
                 VALUES(?1,?2,?3,?4,?5)",
                params![session_id, repository_key, now, warrant_json, now],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn heartbeat(&self, session_id: &str, worker_id: &str) -> familiar_ai_core::Result<()> {
        let changed = self.conn.execute(
            "UPDATE driver_sessions SET heartbeat_at=?1,worker_id=COALESCE(worker_id,?2) WHERE session_id=?3 AND ended_at IS NULL",
            params![Utc::now().to_rfc3339(), worker_id, session_id],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "open driver session {session_id} not found"
            )));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_attempt_diagnostics(
        &self,
        session_id: &str,
        sequence: i64,
        execution_id: Option<&str>,
        adapter_id: Option<&str>,
        model: Option<&str>,
        exit_code: Option<i32>,
        signal: Option<i32>,
        phase: &str,
    ) -> familiar_ai_core::Result<()> {
        let changed = self.conn.execute(
            "UPDATE driver_attempts SET execution_id=COALESCE(?1,execution_id),adapter_id=COALESCE(?2,adapter_id),model=COALESCE(?3,model),exit_code=COALESCE(?4,exit_code),signal=COALESCE(?5,signal),last_durable_phase=?6 WHERE session_id=?7 AND sequence=?8",
            params![execution_id, adapter_id, model, exit_code, signal, phase, session_id, sequence],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "driver attempt {session_id}/{sequence} not found"
            )));
        }
        Ok(())
    }

    pub fn record_attempt_workspace(
        &self,
        session_id: &str,
        sequence: i64,
        component_id: &str,
        worktree_path: &str,
        branch: &str,
    ) -> familiar_ai_core::Result<()> {
        let changed = self.conn.execute(
            "UPDATE driver_attempts SET component_id=?1,worktree_path=?2,branch=?3 WHERE session_id=?4 AND sequence=?5",
            params![component_id, worktree_path, branch, session_id, sequence],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "driver attempt {session_id}/{sequence} not found"
            )));
        }
        Ok(())
    }

    /// Close sessions and attempts left open by a process crash or terminal
    /// interruption. This runs before a new drive session is opened so an
    /// overnight worker can be restarted without leaving ambiguous rows in
    /// the morning report.
    pub fn recover_incomplete(&self) -> familiar_ai_core::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let attempts = self
            .conn
            .execute(
                "UPDATE driver_attempts SET ended_at=COALESCE(ended_at,?1),\
                 outcome=COALESCE(outcome,'retained'),\
                 retained_reason=COALESCE(retained_reason,'interrupted')\
                 WHERE ended_at IS NULL",
                params![now],
            )
            .map_err(db)?;
        self.conn
            .execute(
                "UPDATE driver_sessions SET ended_at=COALESCE(ended_at,?1),\
                 termination_reason=COALESCE(termination_reason,'interrupted')\
                 WHERE ended_at IS NULL",
                params![now],
            )
            .map_err(db)?;
        Ok(attempts)
    }

    /// Record an attempt as started, returning its stable sequence number.
    pub fn record_attempt_started(
        &self,
        session_id: &str,
        prd_id: &str,
        prd_path: &str,
        execution_id: Option<&str>,
    ) -> familiar_ai_core::Result<i64> {
        self.record_attempt_started_with_sources(
            session_id,
            prd_id,
            prd_path,
            execution_id,
            "global",
            "global",
        )
    }

    pub fn record_attempt_started_with_sources(
        &self,
        session_id: &str,
        prd_id: &str,
        prd_path: &str,
        execution_id: Option<&str>,
        review_configuration_source: &str,
        execution_context_configuration_source: &str,
    ) -> familiar_ai_core::Result<i64> {
        self.record_component_attempt_started_with_sources(
            session_id,
            prd_id,
            prd_path,
            execution_id,
            review_configuration_source,
            execution_context_configuration_source,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_component_attempt_started_with_sources(
        &self,
        session_id: &str,
        prd_id: &str,
        prd_path: &str,
        execution_id: Option<&str>,
        review_configuration_source: &str,
        execution_context_configuration_source: &str,
        component_id: Option<&str>,
        worktree_path: Option<&str>,
        branch: Option<&str>,
    ) -> familiar_ai_core::Result<i64> {
        self.record_escalated_attempt_started_with_sources(
            session_id,
            prd_id,
            prd_path,
            execution_id,
            review_configuration_source,
            execution_context_configuration_source,
            component_id,
            worktree_path,
            branch,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_escalated_attempt_started_with_sources(
        &self,
        session_id: &str,
        prd_id: &str,
        prd_path: &str,
        execution_id: Option<&str>,
        review_configuration_source: &str,
        execution_context_configuration_source: &str,
        component_id: Option<&str>,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        escalated_from_sequence: Option<i64>,
        escalation_reason: Option<&str>,
    ) -> familiar_ai_core::Result<i64> {
        let next: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(sequence),0)+1 FROM driver_attempts WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(db)?;
        self.conn
            .execute(
                "INSERT INTO driver_attempts(session_id,sequence,prd_id,prd_path,execution_id,started_at,review_configuration_source,execution_context_configuration_source,component_id,worktree_path,branch,escalated_from_sequence,escalation_reason) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    session_id,
                    next,
                    prd_id,
                    prd_path,
                    execution_id,
                    Utc::now().to_rfc3339(),
                    review_configuration_source,
                    execution_context_configuration_source,
                    component_id,
                    worktree_path,
                    branch,
                    escalated_from_sequence,
                    escalation_reason,
                ],
            )
            .map_err(db)?;
        Ok(next)
    }

    /// Finishes an attempt without recording stopping detail. Callers that
    /// hold the stopping error use [`Self::record_attempt_finished_with_detail`]
    /// so the text lands in the ledger instead of only on stderr.
    #[allow(clippy::too_many_arguments)]
    pub fn record_attempt_finished(
        &self,
        session_id: &str,
        sequence: i64,
        outcome: &str,
        retained_reason: Option<&str>,
        known_cost_microusd: Option<u64>,
        duration_ms: Option<u64>,
    ) -> familiar_ai_core::Result<()> {
        self.record_attempt_finished_with_detail(
            session_id,
            sequence,
            outcome,
            retained_reason,
            None,
            known_cost_microusd,
            duration_ms,
        )
    }

    /// Finishes an attempt, recording both its taxonomy token and the
    /// stopping error in its own words. `retained_detail` is never a
    /// taxonomy token: appending detail to `retained_reason` costs the row
    /// its class and its recovery command.
    #[allow(clippy::too_many_arguments)]
    pub fn record_attempt_finished_with_detail(
        &self,
        session_id: &str,
        sequence: i64,
        outcome: &str,
        retained_reason: Option<&str>,
        retained_detail: Option<&str>,
        known_cost_microusd: Option<u64>,
        duration_ms: Option<u64>,
    ) -> familiar_ai_core::Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE driver_attempts SET ended_at=?1,outcome=?2,retained_reason=?3,\
                 known_cost_microusd=?4,duration_ms=?5,retained_detail=?8 WHERE session_id=?6 AND sequence=?7",
                params![
                    Utc::now().to_rfc3339(),
                    outcome,
                    retained_reason,
                    known_cost_microusd.map(|v| v as i64),
                    duration_ms.map(|v| v as i64),
                    session_id,
                    sequence,
                    retained_detail
                ],
            )
            .map_err(db)?;
        if changed == 0 {
            return Err(FamiliarError::Database(format!(
                "driver attempt {session_id}/{sequence} not found"
            )));
        }
        Ok(())
    }

    pub fn finish_session(
        &self,
        session_id: &str,
        termination_reason: &str,
    ) -> familiar_ai_core::Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE driver_sessions SET ended_at=?1,termination_reason=?2 WHERE session_id=?3",
                params![Utc::now().to_rfc3339(), termination_reason, session_id],
            )
            .map_err(db)?;
        if changed == 0 {
            return Err(FamiliarError::Database(format!(
                "driver session {session_id} not found"
            )));
        }
        Ok(())
    }

    pub fn record_session_detail(
        &self,
        session_id: &str,
        detail: &str,
    ) -> familiar_ai_core::Result<()> {
        let detail = redact_sensitive(detail.to_owned());
        let changed = self.conn.execute(
            "UPDATE driver_sessions SET termination_detail=?1 WHERE session_id=?2 AND ended_at IS NULL",
            params![detail, session_id],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "open driver session {session_id} not found"
            )));
        }
        Ok(())
    }

    /// PRD-065: one durable scheduling decision — why a ready PRD was
    /// selected, deferred, or excluded in this session.
    pub fn record_selection_decision(
        &self,
        session_id: &str,
        prd_id: &str,
        decision: &str,
        detail: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn
            .execute(
                "INSERT INTO driver_selection_decisions(session_id,prd_id,decision,detail,recorded_at) VALUES(?1,?2,?3,?4,?5)",
                params![session_id, prd_id, decision, detail, Utc::now().to_rfc3339()],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Recorded scheduling decisions for one session, in decision order.
    pub fn selection_decisions(
        &self,
        session_id: &str,
    ) -> familiar_ai_core::Result<Vec<(String, String, String)>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT prd_id,decision,detail FROM driver_selection_decisions WHERE session_id=?1 ORDER BY decision_id",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([session_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn get_session(&self, session_id: &str) -> familiar_ai_core::Result<Option<DriverSession>> {
        self.conn
            .query_row(
                "SELECT session_id,repository_key,started_at,ended_at,termination_reason,warrant_json,termination_detail \
                 FROM driver_sessions WHERE session_id=?1",
                params![session_id],
                map_session,
            )
            .optional()
            .map_err(db)
    }

    /// The most recently started session, complete or interrupted.
    pub fn latest_session(&self) -> familiar_ai_core::Result<Option<DriverSession>> {
        self.conn
            .query_row(
                "SELECT session_id,repository_key,started_at,ended_at,termination_reason,warrant_json,termination_detail \
                 FROM driver_sessions ORDER BY started_at DESC, session_id DESC LIMIT 1",
                [],
                map_session,
            )
            .optional()
            .map_err(db)
    }

    /// Deterministic, repository-scoped, cursor-paginated listing of driver
    /// sessions, most recent first. `after` is the opaque cursor returned by
    /// a previous page (the last delivered session's `session_id`); an
    /// absent cursor starts at the most recent session.
    pub fn list_sessions_by_repository(
        &self,
        repository_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<DriverSession>> {
        let boundary: (String, String) = match after {
            Some(session_id) => {
                let started_at: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT started_at FROM driver_sessions WHERE session_id=?1",
                        params![session_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db)?;
                match started_at {
                    Some(started_at) => (started_at, session_id.to_string()),
                    // An unknown cursor yields an empty continuation rather
                    // than silently restarting the sequence.
                    None => return Ok(Vec::new()),
                }
            }
            // A cursor after every possible (started_at, session_id) pair —
            // the first page starts from the very top of the ordering.
            None => ("~".repeat(40), "~".repeat(64)),
        };
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id,repository_key,started_at,ended_at,termination_reason,warrant_json,termination_detail \
                 FROM driver_sessions WHERE repository_key=?1 \
                 AND (started_at,session_id) < (?2,?3) \
                 ORDER BY started_at DESC, session_id DESC LIMIT ?4",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(
                params![repository_key, boundary.0, boundary.1, limit as i64],
                map_session,
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    pub fn attempts(&self, session_id: &str) -> familiar_ai_core::Result<Vec<DriverAttempt>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sequence,prd_id,prd_path,execution_id,started_at,ended_at,outcome,\
                retained_reason,known_cost_microusd,duration_ms,adapter_id,model,exit_code,signal,last_durable_phase,review_configuration_source,execution_context_configuration_source,component_id,worktree_path,branch,escalated_from_sequence,escalation_reason,retained_detail FROM driver_attempts \
                 WHERE session_id=?1 ORDER BY sequence",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(DriverAttempt {
                    sequence: row.get(0)?,
                    prd_id: row.get(1)?,
                    prd_path: row.get(2)?,
                    execution_id: row.get(3)?,
                    started_at: row.get(4)?,
                    ended_at: row.get(5)?,
                    outcome: row.get(6)?,
                    retained_reason: row.get(7)?,
                    known_cost_microusd: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    duration_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                    adapter_id: row.get(10)?,
                    model: row.get(11)?,
                    exit_code: row.get(12)?,
                    signal: row.get(13)?,
                    last_durable_phase: row.get(14)?,
                    review_configuration_source: row.get(15)?,
                    execution_context_configuration_source: row.get(16)?,
                    component_id: row.get(17)?,
                    worktree_path: row.get(18)?,
                    branch: row.get(19)?,
                    escalated_from_sequence: row.get(20)?,
                    escalation_reason: row.get(21)?,
                    retained_detail: row.get(22)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    /// Deterministic, cursor-paginated listing of one session's attempts,
    /// ordered by `sequence` ascending. `after` is the last delivered
    /// `sequence` (exclusive).
    pub fn attempts_page(
        &self,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<DriverAttempt>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sequence,prd_id,prd_path,execution_id,started_at,ended_at,outcome,\
                retained_reason,known_cost_microusd,duration_ms,adapter_id,model,exit_code,signal,last_durable_phase,review_configuration_source,execution_context_configuration_source,component_id,worktree_path,branch,escalated_from_sequence,escalation_reason,retained_detail FROM driver_attempts \
                 WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(
                params![session_id, after.unwrap_or(0), limit as i64],
                |row| {
                    Ok(DriverAttempt {
                        sequence: row.get(0)?,
                        prd_id: row.get(1)?,
                        prd_path: row.get(2)?,
                        execution_id: row.get(3)?,
                        started_at: row.get(4)?,
                        ended_at: row.get(5)?,
                        outcome: row.get(6)?,
                        retained_reason: row.get(7)?,
                        known_cost_microusd: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                        duration_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                        adapter_id: row.get(10)?,
                        model: row.get(11)?,
                        exit_code: row.get(12)?,
                        signal: row.get(13)?,
                        last_durable_phase: row.get(14)?,
                        review_configuration_source: row.get(15)?,
                        execution_context_configuration_source: row.get(16)?,
                        component_id: row.get(17)?,
                        worktree_path: row.get(18)?,
                        branch: row.get(19)?,
                        escalated_from_sequence: row.get(20)?,
                        escalation_reason: row.get(21)?,
                        retained_detail: row.get(22)?,
                    })
                },
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    pub fn latest_attempt_for_prd(
        &self,
        repository_key: &str,
        prd_id: &str,
    ) -> familiar_ai_core::Result<Option<DriverAttempt>> {
        self.conn.query_row(
            "SELECT a.sequence,a.prd_id,a.prd_path,a.execution_id,a.started_at,a.ended_at,a.outcome,a.retained_reason,a.known_cost_microusd,a.duration_ms,a.adapter_id,a.model,a.exit_code,a.signal,a.last_durable_phase,a.review_configuration_source,a.execution_context_configuration_source,a.component_id,a.worktree_path,a.branch,a.escalated_from_sequence,a.escalation_reason,a.retained_detail FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id WHERE s.repository_key=?1 AND a.prd_id=?2 ORDER BY a.started_at DESC,a.sequence DESC LIMIT 1",
            params![repository_key, prd_id],
            |row| Ok(DriverAttempt {
                sequence: row.get(0)?, prd_id: row.get(1)?, prd_path: row.get(2)?,
                execution_id: row.get(3)?, started_at: row.get(4)?, ended_at: row.get(5)?,
                outcome: row.get(6)?, retained_reason: row.get(7)?,
                known_cost_microusd: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                duration_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                adapter_id: row.get(10)?, model: row.get(11)?, exit_code: row.get(12)?,
                signal: row.get(13)?, last_durable_phase: row.get(14)?,
                review_configuration_source: row.get(15)?, execution_context_configuration_source: row.get(16)?,
                component_id: row.get(17)?, worktree_path: row.get(18)?, branch: row.get(19)?,
                escalated_from_sequence: row.get(20)?, escalation_reason: row.get(21)?,
                    retained_detail: row.get(22)?,
            }),
        ).optional().map_err(db)
    }

    /// How many sessions this repository has, for saying what a truncated
    /// chart is truncating.
    pub fn count_sessions(&self, repository_key: &str) -> familiar_ai_core::Result<usize> {
        self.conn
            .query_row(
                "SELECT count(*) FROM driver_sessions WHERE repository_key=?1",
                params![repository_key],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .map_err(db)
    }

    /// Every attempt in this repository's most recent `limit` sessions, in one
    /// read.
    ///
    /// The rounds view needs each PRD placed against each session, so asking
    /// per session would be one round trip per column — twenty-odd queries to
    /// draw one chart, growing with every session the driver ever opened.
    pub fn rounds(
        &self,
        repository_key: &str,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<RoundAttempt>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT a.session_id,a.sequence,a.prd_id,a.prd_path,a.started_at,a.ended_at,\
                 a.outcome,a.retained_reason,a.duration_ms,a.retained_detail FROM driver_attempts a \
                 JOIN driver_sessions s ON s.session_id=a.session_id \
                 WHERE s.repository_key=?1 AND s.session_id IN \
                   (SELECT session_id FROM driver_sessions WHERE repository_key=?1 \
                    ORDER BY started_at DESC, session_id DESC LIMIT ?2) \
                 ORDER BY s.started_at, s.session_id, a.sequence",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(params![repository_key, limit as i64], |row| {
                Ok(RoundAttempt {
                    session_id: row.get(0)?,
                    sequence: row.get(1)?,
                    prd_id: row.get(2)?,
                    prd_path: row.get(3)?,
                    started_at: row.get(4)?,
                    ended_at: row.get(5)?,
                    outcome: row.get(6)?,
                    retained_reason: row.get(7)?,
                    retained_detail: row.get(9)?,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                })
            })
            .map_err(db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db)
    }
}

fn redact_sensitive(mut value: String) -> String {
    for (name, secret) in std::env::vars() {
        let name = name.to_ascii_uppercase();
        if secret.len() >= 4
            && ["SECRET", "TOKEN", "PASSWORD", "API_KEY", "CANARY"]
                .iter()
                .any(|marker| name.contains(marker))
        {
            value = value.replace(&secret, "[REDACTED]");
        }
    }
    value
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<DriverSession> {
    Ok(DriverSession {
        session_id: row.get(0)?,
        repository_key: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        termination_reason: row.get(4)?,
        warrant_json: row.get(5)?,
        termination_detail: row.get(6)?,
    })
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    /// The chart reads one row per repository and one column per session, so
    /// the query has to be scoped and ordered, and its limit has to take the
    /// newest sessions rather than the oldest.
    #[test]
    fn rounds_are_scoped_ordered_and_limited_to_the_newest_sessions() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        for (session, repo) in [
            ("s1", "/repo/.git"),
            ("s2", "/repo/.git"),
            ("s3", "/repo/.git"),
            ("other", "/elsewhere/.git"),
        ] {
            repository.open_session(session, repo, "{}").unwrap();
            // `started_at` is the sort key and defaults to now, so the
            // sessions need distinguishable timestamps.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for (session, prd) in [
            ("s1", "PRD-1"),
            ("s2", "PRD-2"),
            ("s3", "PRD-3"),
            ("other", "PRD-9"),
        ] {
            repository
                .record_attempt_started(session, prd, &format!("docs/prds/{prd}.md"), None)
                .unwrap();
        }

        let all = repository.rounds("/repo/.git", 10).unwrap();
        assert_eq!(
            all.iter().map(|a| a.prd_id.as_str()).collect::<Vec<_>>(),
            ["PRD-1", "PRD-2", "PRD-3"],
            "oldest session first, and the other repository's work is not ours"
        );

        let newest = repository.rounds("/repo/.git", 2).unwrap();
        assert_eq!(
            newest.iter().map(|a| a.prd_id.as_str()).collect::<Vec<_>>(),
            ["PRD-2", "PRD-3"],
            "a truncated chart drops the oldest rounds, not the newest"
        );

        assert_eq!(repository.count_sessions("/repo/.git").unwrap(), 3);
        assert_eq!(repository.count_sessions("/elsewhere/.git").unwrap(), 1);
    }

    /// The taxonomy token and the stopping error are separate columns on
    /// purpose: a classifier and a recovery lookup both key off the token, so
    /// detail folded into it silently costs the row its class. Both must
    /// survive the round trip, and the token must come back clean.
    #[test]
    fn the_stopping_error_is_stored_beside_its_class_not_inside_it() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository.open_session("s1", "/repo/.git", "{}").unwrap();
        let stopped = repository
            .record_attempt_started("s1", "PRD-1", "docs/prds/PRD-1.md", None)
            .unwrap();
        repository
            .record_attempt_finished_with_detail(
                "s1",
                stopped,
                "retained",
                Some("verification_failed"),
                Some("cargo test --workspace: 3 failed in familiar-ai-daemon"),
                None,
                Some(2_000),
            )
            .unwrap();

        let attempt = &repository.attempts("s1").unwrap()[0];
        assert_eq!(
            attempt.retained_reason.as_deref(),
            Some("verification_failed")
        );
        assert_eq!(
            attempt.retained_detail.as_deref(),
            Some("cargo test --workspace: 3 failed in familiar-ai-daemon"),
        );
        // The rounds view carries it too, or the dashboard cannot show it.
        let rounds = repository.rounds("/repo/.git", 10).unwrap();
        assert_eq!(
            rounds[0].retained_detail.as_deref(),
            Some("cargo test --workspace: 3 failed in familiar-ai-daemon"),
        );

        // The plain entry point still works and records no detail rather than
        // an empty string standing in for one.
        let bare = repository
            .record_attempt_started("s1", "PRD-2", "docs/prds/PRD-2.md", None)
            .unwrap();
        repository
            .record_attempt_finished("s1", bare, "completed", None, None, Some(1))
            .unwrap();
        let attempts = repository.attempts("s1").unwrap();
        let completed = attempts.iter().find(|a| a.prd_id == "PRD-2").unwrap();
        assert_eq!(completed.retained_detail, None);
    }

    /// An unfinished attempt has no outcome. The chart distinguishes that from
    /// a finished one, so the query must carry the null rather than defaulting
    /// it to something that reads as an ending.
    #[test]
    fn rounds_carry_the_outcome_and_its_absence() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository.open_session("s1", "/repo/.git", "{}").unwrap();
        let done = repository
            .record_attempt_started("s1", "PRD-1", "docs/prds/PRD-1.md", None)
            .unwrap();
        repository
            .record_attempt_started("s1", "PRD-2", "docs/prds/PRD-2.md", None)
            .unwrap();
        repository
            .record_attempt_finished("s1", done, "retained", Some("a gate"), None, Some(2_000))
            .unwrap();

        let rounds = repository.rounds("/repo/.git", 10).unwrap();
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].outcome.as_deref(), Some("retained"));
        assert_eq!(rounds[0].retained_reason.as_deref(), Some("a gate"));
        assert_eq!(rounds[0].duration_ms, Some(2_000));
        assert_eq!(rounds[1].outcome, None, "still running");
    }

    #[test]
    fn session_and_attempts_round_trip() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session("session-1", "/repo/.git", r#"{"max_prds":2}"#)
            .unwrap();
        let first = repository
            .record_component_attempt_started_with_sources(
                "session-1",
                "PRD-17",
                "docs/prds/PRD-017.md",
                Some("exec-1"),
                "global",
                "global",
                Some("component-PRD-017"),
                Some("/state/worktrees/session-1/component-PRD-017"),
                Some("familiar/session-1/component-PRD-017"),
            )
            .unwrap();
        let second = repository
            .record_attempt_started(
                "session-1",
                "PRD-18",
                "docs/prds/PRD-018.md",
                Some("exec-2"),
            )
            .unwrap();
        assert_eq!((first, second), (1, 2));
        repository
            .record_attempt_finished("session-1", first, "completed", None, Some(1_234), Some(50))
            .unwrap();
        repository
            .record_attempt_finished(
                "session-1",
                second,
                "retained",
                Some("scope_broadened"),
                None,
                Some(75),
            )
            .unwrap();
        repository
            .finish_session("session-1", "backlog_empty")
            .unwrap();

        let session = repository.get_session("session-1").unwrap().unwrap();
        assert_eq!(session.termination_reason.as_deref(), Some("backlog_empty"));
        assert!(session.ended_at.is_some());
        assert_eq!(session.warrant_json, r#"{"max_prds":2}"#);

        let attempts = repository.attempts("session-1").unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].outcome.as_deref(), Some("completed"));
        assert_eq!(attempts[0].known_cost_microusd, Some(1_234));
        assert_eq!(
            attempts[0].component_id.as_deref(),
            Some("component-PRD-017")
        );
        assert_eq!(
            attempts[0].worktree_path.as_deref(),
            Some("/state/worktrees/session-1/component-PRD-017")
        );
        assert_eq!(
            attempts[0].branch.as_deref(),
            Some("familiar/session-1/component-PRD-017")
        );
        assert_eq!(
            attempts[1].retained_reason.as_deref(),
            Some("scope_broadened")
        );
        // Unknown cost stays unknown: never coerced to zero.
        assert_eq!(attempts[1].known_cost_microusd, None);
    }

    #[test]
    fn interrupted_session_is_reconstructible_from_rows() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session("session-2", "/repo/.git", "{}")
            .unwrap();
        let sequence = repository
            .record_attempt_started(
                "session-2",
                "PRD-17",
                "docs/prds/PRD-017.md",
                Some("exec-9"),
            )
            .unwrap();

        // No finish_session, no attempt completion: the process died here.
        let session = repository.get_session("session-2").unwrap().unwrap();
        assert!(session.ended_at.is_none());
        assert!(session.termination_reason.is_none());
        let attempts = repository.attempts("session-2").unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].sequence, sequence);
        assert!(attempts[0].ended_at.is_none());
        assert!(attempts[0].outcome.is_none());
        assert_eq!(attempts[0].execution_id.as_deref(), Some("exec-9"));
    }

    #[test]
    fn recovery_closes_open_session_and_attempt_with_explicit_reason() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session("session-recover", "/repo/.git", "{}")
            .unwrap();
        repository
            .record_attempt_started("session-recover", "PRD-17", "docs/prd.md", None)
            .unwrap();

        assert_eq!(repository.recover_incomplete().unwrap(), 1);
        let session = repository.get_session("session-recover").unwrap().unwrap();
        assert_eq!(session.termination_reason.as_deref(), Some("interrupted"));
        assert!(session.ended_at.is_some());
        let attempt = &repository.attempts("session-recover").unwrap()[0];
        assert_eq!(attempt.outcome.as_deref(), Some("retained"));
        assert_eq!(attempt.retained_reason.as_deref(), Some("interrupted"));
        assert!(attempt.ended_at.is_some());

        // Recovery is idempotent and does not rewrite completed records.
        assert_eq!(repository.recover_incomplete().unwrap(), 0);
    }

    #[test]
    fn latest_session_selects_the_most_recent_and_missing_rows_fail_closed() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        assert!(repository.latest_session().unwrap().is_none());
        repository
            .open_session("older", "/repo/.git", "{}")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        repository
            .open_session("newer", "/repo/.git", "{}")
            .unwrap();
        assert_eq!(
            repository.latest_session().unwrap().unwrap().session_id,
            "newer"
        );
        assert!(repository.get_session("absent").unwrap().is_none());
        assert!(repository
            .finish_session("absent", "backlog_empty")
            .is_err());
        assert!(repository
            .record_attempt_finished("older", 99, "completed", None, None, None)
            .is_err());
    }

    #[test]
    fn list_sessions_by_repository_is_scoped_ordered_and_paginated() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository.open_session("s1", "/repo/.git", "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        repository.open_session("s2", "/repo/.git", "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        repository.open_session("s3", "/repo/.git", "{}").unwrap();
        // A session from a different repository must never leak into a
        // repository-scoped listing.
        repository
            .open_session("other-1", "/other/.git", "{}")
            .unwrap();

        let all = repository
            .list_sessions_by_repository("/repo/.git", None, 10)
            .unwrap();
        assert_eq!(
            all.iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["s3", "s2", "s1"]
        );

        let first_page = repository
            .list_sessions_by_repository("/repo/.git", None, 1)
            .unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].session_id, "s3");

        let second_page = repository
            .list_sessions_by_repository("/repo/.git", Some(&first_page[0].session_id), 10)
            .unwrap();
        assert_eq!(
            second_page
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["s2", "s1"]
        );

        // An unknown cursor yields an empty continuation rather than
        // silently restarting the sequence.
        assert!(repository
            .list_sessions_by_repository("/repo/.git", Some("nope"), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn attempts_page_is_ordered_by_sequence_and_paginated() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session("session-page", "/repo/.git", "{}")
            .unwrap();
        for n in 1..=3 {
            repository
                .record_attempt_started(
                    "session-page",
                    &format!("PRD-{n}"),
                    &format!("docs/prds/PRD-{n}.md"),
                    None,
                )
                .unwrap();
        }

        let all = repository.attempts_page("session-page", None, 10).unwrap();
        assert_eq!(
            all.iter().map(|a| a.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );

        let first_page = repository.attempts_page("session-page", None, 1).unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].sequence, 1);

        let rest = repository
            .attempts_page("session-page", Some(first_page[0].sequence), 10)
            .unwrap();
        assert_eq!(rest.iter().map(|a| a.sequence).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn escalation_evidence_is_linked_and_cannot_be_duplicated() {
        let db = database();
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session("escalation", "/repo/.git", "{}")
            .unwrap();
        let cheap = repository
            .record_attempt_started("escalation", "PRD-41", "docs/prds/PRD-041.md", None)
            .unwrap();
        let stronger = repository
            .record_escalated_attempt_started_with_sources(
                "escalation",
                "PRD-41",
                "docs/prds/PRD-041.md",
                None,
                "global",
                "global",
                None,
                None,
                None,
                Some(cheap),
                Some("required_verification_failed"),
            )
            .unwrap();
        assert_eq!(stronger, cheap + 1);
        let attempts = repository.attempts("escalation").unwrap();
        assert_eq!(attempts[1].escalated_from_sequence, Some(cheap));
        assert_eq!(
            attempts[1].escalation_reason.as_deref(),
            Some("required_verification_failed")
        );
        assert!(repository
            .record_escalated_attempt_started_with_sources(
                "escalation",
                "PRD-41",
                "docs/prds/PRD-041.md",
                None,
                "global",
                "global",
                None,
                None,
                None,
                Some(cheap),
                Some("required_verification_failed"),
            )
            .is_err());
    }
}
