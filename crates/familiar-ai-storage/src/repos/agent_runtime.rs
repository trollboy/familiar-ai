//! PRD-058 persistence: the write-ahead tool journal, per-attempt
//! reservation binding, and execution evidence for the Familiar-owned
//! raw-model agent loop. Every write here is append-only, matching the
//! `no_update`/`no_delete` trigger discipline in migration 055.

use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension};

use familiar_ai_core::FamiliarError;

fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| FamiliarError::Database("secure agent-runtime id generation failed".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResultOutcome {
    Succeeded { result_hash: String },
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolIntentRow {
    pub call_id: String,
    pub capability: String,
    pub argument_hash: String,
    pub side_effect_class: String,
    pub recorded_at: String,
}

pub struct AgentRuntimeRepository<'a> {
    conn: &'a Connection,
}

impl<'a> AgentRuntimeRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Every inference submission is its own attempt row, recorded before
    /// (or in the same durable step as) the submission itself so a crash
    /// can never lose which attempts were ever made.
    pub fn record_attempt(
        &self,
        attempt_id: &str,
        execution_id: &str,
        reservation_id: Option<&str>,
        provider_request_id: Option<&str>,
        provider_idempotency_key: Option<&str>,
        ambiguous: bool,
    ) -> familiar_ai_core::Result<()> {
        self.conn
            .execute(
                "INSERT INTO agent_runtime_attempts(attempt_id,execution_id,reservation_id,provider_request_id,provider_idempotency_key,ambiguous,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    attempt_id,
                    execution_id,
                    reservation_id,
                    provider_request_id,
                    provider_idempotency_key,
                    ambiguous,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Write-ahead: must be durable before the tool call executes.
    pub fn record_tool_intent(
        &self,
        execution_id: &str,
        call_id: &str,
        capability: &str,
        argument_hash: &str,
        side_effect_class: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn
            .execute(
                "INSERT INTO agent_runtime_tool_intents(intent_id,execution_id,call_id,capability,argument_hash,side_effect_class,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    format!("tin_{}", random_hex()?),
                    execution_id,
                    call_id,
                    capability,
                    argument_hash,
                    side_effect_class,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn record_tool_result(
        &self,
        execution_id: &str,
        call_id: &str,
        outcome: &ToolResultOutcome,
    ) -> familiar_ai_core::Result<()> {
        let (outcome_str, result_hash, failure_detail) = match outcome {
            ToolResultOutcome::Succeeded { result_hash } => {
                ("succeeded", Some(result_hash.as_str()), None)
            }
            ToolResultOutcome::Failed { detail } => ("failed", None, Some(detail.as_str())),
        };
        self.conn
            .execute(
                "INSERT INTO agent_runtime_tool_results(result_id,execution_id,call_id,outcome,result_hash,failure_detail,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    format!("tres_{}", random_hex()?),
                    execution_id,
                    call_id,
                    outcome_str,
                    result_hash,
                    failure_detail,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn tool_result(
        &self,
        execution_id: &str,
        call_id: &str,
    ) -> familiar_ai_core::Result<Option<ToolResultOutcome>> {
        self.conn
            .query_row(
                "SELECT outcome,result_hash,failure_detail FROM agent_runtime_tool_results WHERE execution_id=?1 AND call_id=?2",
                params![execution_id, call_id],
                |row| {
                    let outcome: String = row.get(0)?;
                    let result_hash: Option<String> = row.get(1)?;
                    let failure_detail: Option<String> = row.get(2)?;
                    Ok(match outcome.as_str() {
                        "succeeded" => ToolResultOutcome::Succeeded {
                            result_hash: result_hash.unwrap_or_default(),
                        },
                        _ => ToolResultOutcome::Failed {
                            detail: failure_detail.unwrap_or_default(),
                        },
                    })
                },
            )
            .optional()
            .map_err(db)
    }

    /// Intents durably recorded with no matching result — the exact set a
    /// resumed loop must reason about (never repeats a destructive call in
    /// this state; a read-only/idempotent-write call may re-run).
    /// Total intents ever recorded for this execution, used only as the
    /// evidence resume high-water mark (informational; resume correctness
    /// itself comes from `pending_intents`/`tool_result`, not this count).
    pub fn intent_count(&self, execution_id: &str) -> familiar_ai_core::Result<u64> {
        self.conn
            .query_row(
                "SELECT count(*) FROM agent_runtime_tool_intents WHERE execution_id=?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(db)
    }

    pub fn pending_intents(
        &self,
        execution_id: &str,
    ) -> familiar_ai_core::Result<Vec<ToolIntentRow>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT i.call_id,i.capability,i.argument_hash,i.side_effect_class,i.recorded_at \
                 FROM agent_runtime_tool_intents i \
                 WHERE i.execution_id=?1 \
                 AND NOT EXISTS (SELECT 1 FROM agent_runtime_tool_results r WHERE r.execution_id=i.execution_id AND r.call_id=i.call_id) \
                 ORDER BY i.recorded_at, i.intent_id",
            )
            .map_err(db)?;
        let rows = statement
            .query_map(params![execution_id], |row| {
                Ok(ToolIntentRow {
                    call_id: row.get(0)?,
                    capability: row.get(1)?,
                    argument_hash: row.get(2)?,
                    side_effect_class: row.get(3)?,
                    recorded_at: row.get(4)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_evidence(
        &self,
        execution_id: &str,
        prompt_template_version: &str,
        worker_spec_identity: &str,
        worker_empirical_version: &str,
        offered_tools_json: &str,
        calls_json: &str,
        stop_reason: &str,
        stop_reason_detail_json: Option<&str>,
        iterations: u32,
        resume_conversation_messages: u64,
        resume_journal_high_water_mark: u64,
    ) -> familiar_ai_core::Result<String> {
        let evidence_id = format!("agev_{}", random_hex()?);
        self.conn
            .execute(
                "INSERT INTO agent_runtime_evidence(evidence_id,execution_id,prompt_template_version,worker_spec_identity,worker_empirical_version,offered_tools_json,calls_json,stop_reason,stop_reason_detail_json,iterations,resume_conversation_messages,resume_journal_high_water_mark,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    evidence_id,
                    execution_id,
                    prompt_template_version,
                    worker_spec_identity,
                    worker_empirical_version,
                    offered_tools_json,
                    calls_json,
                    stop_reason,
                    stop_reason_detail_json,
                    iterations,
                    resume_conversation_messages,
                    resume_journal_high_water_mark,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db)?;
        Ok(evidence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::run_migrations;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES('exec_1',?1,'raw-runtime','running','repo','wt','docs/prds/PRD-058.md','[]')",
            params![Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn
    }

    #[test]
    fn journal_intent_without_result_is_pending() {
        let conn = setup();
        let repo = AgentRuntimeRepository::new(&conn);
        repo.record_tool_intent(
            "exec_1",
            "call_1",
            "apply-edit",
            "hash1",
            "idempotent-write",
        )
        .unwrap();
        let pending = repo.pending_intents("exec_1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].call_id, "call_1");
        assert!(repo.tool_result("exec_1", "call_1").unwrap().is_none());
    }

    #[test]
    fn recorded_result_clears_pending_and_is_retrievable() {
        let conn = setup();
        let repo = AgentRuntimeRepository::new(&conn);
        repo.record_tool_intent(
            "exec_1",
            "call_1",
            "apply-edit",
            "hash1",
            "idempotent-write",
        )
        .unwrap();
        repo.record_tool_result(
            "exec_1",
            "call_1",
            &ToolResultOutcome::Succeeded {
                result_hash: "rh1".into(),
            },
        )
        .unwrap();
        assert!(repo.pending_intents("exec_1").unwrap().is_empty());
        assert_eq!(
            repo.tool_result("exec_1", "call_1").unwrap(),
            Some(ToolResultOutcome::Succeeded {
                result_hash: "rh1".into()
            })
        );
    }

    #[test]
    fn journal_rows_are_append_only() {
        let conn = setup();
        let repo = AgentRuntimeRepository::new(&conn);
        repo.record_tool_intent(
            "exec_1",
            "call_1",
            "apply-edit",
            "hash1",
            "idempotent-write",
        )
        .unwrap();
        let err = conn
            .execute(
                "UPDATE agent_runtime_tool_intents SET argument_hash='tampered' WHERE call_id='call_1'",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("append-only"));
    }

    #[test]
    fn attempt_and_evidence_round_trip() {
        let conn = setup();
        let repo = AgentRuntimeRepository::new(&conn);
        repo.record_attempt("att_1", "exec_1", Some("res_1"), Some("req_1"), None, false)
            .unwrap();
        let evidence_id = repo
            .record_evidence(
                "exec_1",
                "agent-loop-prompt/1",
                "wspec-sha256:test",
                "wver-sha256:test",
                "[]",
                "[]",
                "completed",
                None,
                1,
                2,
                0,
            )
            .unwrap();
        assert!(evidence_id.starts_with("agev_"));
    }
}
