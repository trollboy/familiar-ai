use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryEffect {
    pub effect_kind: String,
    pub status: String,
    pub external_reference: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalEvidence {
    pub passed: bool,
    pub target: Option<String>,
    pub revision: Option<String>,
    pub output: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAuthorityDecision {
    pub prd_id: String,
    pub mode: String,
    pub actor: String,
    pub decision: String,
    pub assurance_label: Option<String>,
    pub findings_json: String,
    pub stop_reasons_json: String,
    pub warrant_consumed: u64,
}

/// One delivery authority decision, keyed for the repository-scoped
/// stewardship query surface (adds identity and ordering fields the
/// per-session [`DeliveryAuthorityDecision`] read does not need).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryDecisionRow {
    pub decision_id: String,
    pub session_id: String,
    pub prd_id: String,
    pub mode: String,
    pub actor: String,
    pub decision: String,
    pub assurance_label: Option<String>,
    pub findings_json: String,
    pub stop_reasons_json: String,
    pub warrant_json: Option<String>,
    pub warrant_consumed: u64,
    pub created_at: String,
}

pub struct DeliveryRepository<'a> {
    conn: &'a Connection,
}

impl<'a> DeliveryRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_authority_decision(
        &self,
        decision_id: &str,
        repository_key: &str,
        session_id: &str,
        prd_id: &str,
        mode: &str,
        actor: &str,
        decision: &str,
        assurance_label: Option<&str>,
        findings_json: &str,
        stop_reasons_json: &str,
        warrant_json: Option<&str>,
        warrant_consumed: u64,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO delivery_authority_decisions(decision_id,repository_key,session_id,prd_id,mode,actor,decision,assurance_label,findings_json,stop_reasons_json,warrant_json,warrant_consumed,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![decision_id,repository_key,session_id,prd_id,mode,actor,decision,assurance_label,findings_json,stop_reasons_json,warrant_json,warrant_consumed,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn begin_effect(
        &self,
        effect_id: &str,
        repository_key: &str,
        session_id: &str,
        prd_id: &str,
        effect_kind: &str,
        idempotency_key: &str,
    ) -> familiar_ai_core::Result<DeliveryEffect> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT OR IGNORE INTO delivery_external_effects(effect_id,repository_key,session_id,prd_id,effect_kind,idempotency_key,status,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'intent',?7)",params![effect_id,repository_key,session_id,prd_id,effect_kind,idempotency_key,now]).map_err(db)?;
        self.effect(idempotency_key)?
            .ok_or_else(|| FamiliarError::Database("delivery effect disappeared".into()))
    }

    pub fn finish_effect(
        &self,
        idempotency_key: &str,
        succeeded: bool,
        external_reference: Option<&str>,
        detail: Option<&str>,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("UPDATE delivery_external_effects SET status=?1,external_reference=COALESCE(?2,external_reference),detail=?3,updated_at=?4 WHERE idempotency_key=?5",params![if succeeded {"succeeded"} else {"failed"},external_reference,detail,Utc::now().to_rfc3339(),idempotency_key]).map_err(db)?;
        Ok(())
    }

    pub fn finish_evidence(
        &self,
        idempotency_key: &str,
        succeeded: bool,
        target: &str,
        revision: &str,
        output: &[u8],
        detail: Option<&str>,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute(
            "UPDATE delivery_external_effects SET status=?1,target=?2,revision=?3,output=?4,detail=?5,updated_at=?6 WHERE idempotency_key=?7",
            params![if succeeded {"succeeded"} else {"failed"},target,revision,output,detail,Utc::now().to_rfc3339(),idempotency_key],
        ).map_err(db)?;
        Ok(())
    }

    /// Resolve only Familiar-owned evidence. Unknown namespaces and missing
    /// rows fail closed at the caller.
    pub fn resolve_internal_gate(
        &self,
        repository_key: &str,
        gate: &str,
    ) -> familiar_ai_core::Result<Option<InternalEvidence>> {
        if let Some(role) = gate
            .strip_prefix("deploy:")
            .and_then(|v| v.strip_suffix("-smoke-passing"))
        {
            return self.conn.query_row(
                "SELECT status,target,revision,COALESCE(output,X'') FROM delivery_external_effects WHERE repository_key=?1 AND effect_kind='smoke' AND external_reference=?2 ORDER BY updated_at DESC LIMIT 1",
                params![repository_key, role],
                |row| Ok(InternalEvidence { passed: row.get::<_,String>(0)? == "succeeded", target: row.get(1)?, revision: row.get(2)?, output: row.get(3)? }),
            ).optional().map_err(db);
        }
        if let Some(check_id) = gate.strip_prefix("verification:") {
            // Exact check and canonical repository identity are persisted on
            // the evidence row. Missing attribution therefore fails closed.
            return self
                .conn
                .query_row(
                    "SELECT evidence_json FROM review_verification_evidence evidence \
                 WHERE evidence.repository_key=?1 AND evidence.check_id=?2 \
                 ORDER BY evidence.rowid DESC LIMIT 1",
                    params![repository_key, check_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(db)
                .and_then(|value| {
                    value
                        .map(|json| {
                            let evidence: familiar_ai_review::VerificationEvidence =
                                serde_json::from_str(&json).map_err(|error| {
                                    familiar_ai_core::FamiliarError::Database(format!(
                                        "invalid stored verification evidence: {error}"
                                    ))
                                })?;
                            Ok(InternalEvidence {
                                passed: evidence.status
                                    == familiar_ai_review::VerificationStatus::Passed,
                                target: None,
                                revision: None,
                                output: json.into_bytes(),
                            })
                        })
                        .transpose()
                });
        }
        Ok(None)
    }

    pub fn effect(&self, key: &str) -> familiar_ai_core::Result<Option<DeliveryEffect>> {
        self.conn.query_row("SELECT effect_kind,status,external_reference,detail FROM delivery_external_effects WHERE idempotency_key=?1",[key],|row| Ok(DeliveryEffect { effect_kind:row.get(0)?,status:row.get(1)?,external_reference:row.get(2)?,detail:row.get(3)? })).optional().map_err(db)
    }

    pub fn decisions_for_session(
        &self,
        session_id: &str,
    ) -> familiar_ai_core::Result<Vec<DeliveryAuthorityDecision>> {
        let mut statement=self.conn.prepare("SELECT prd_id,mode,actor,decision,assurance_label,findings_json,stop_reasons_json,warrant_consumed FROM delivery_authority_decisions WHERE session_id=?1 ORDER BY created_at,decision_id").map_err(db)?;
        let decisions = statement
            .query_map([session_id], |row| {
                Ok(DeliveryAuthorityDecision {
                    prd_id: row.get(0)?,
                    mode: row.get(1)?,
                    actor: row.get(2)?,
                    decision: row.get(3)?,
                    assurance_label: row.get(4)?,
                    findings_json: row.get(5)?,
                    stop_reasons_json: row.get(6)?,
                    warrant_consumed: row.get(7)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(decisions)
    }

    /// Deterministic, repository-scoped, cursor-paginated listing of every
    /// delivery authority decision across sessions, ordered by
    /// `(created_at, decision_id)`. `after` is the opaque cursor returned by
    /// a previous page (the last delivered row's `decision_id`).
    pub fn list_decisions(
        &self,
        repository_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<DeliveryDecisionRow>> {
        let boundary: (String, String) = match after {
            Some(decision_id) => {
                let created_at: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT created_at FROM delivery_authority_decisions WHERE decision_id=?1",
                        params![decision_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db)?;
                match created_at {
                    Some(created_at) => (created_at, decision_id.to_string()),
                    None => return Ok(Vec::new()),
                }
            }
            None => (String::new(), String::new()),
        };
        let mut statement = self.conn.prepare(
            "SELECT decision_id,session_id,prd_id,mode,actor,decision,assurance_label,findings_json,stop_reasons_json,warrant_json,warrant_consumed,created_at \
             FROM delivery_authority_decisions WHERE repository_key=?1 AND (created_at,decision_id)>(?2,?3) \
             ORDER BY created_at,decision_id LIMIT ?4",
        ).map_err(db)?;
        let rows = statement
            .query_map(
                params![repository_key, boundary.0, boundary.1, limit as i64],
                |row| {
                    Ok(DeliveryDecisionRow {
                        decision_id: row.get(0)?,
                        session_id: row.get(1)?,
                        prd_id: row.get(2)?,
                        mode: row.get(3)?,
                        actor: row.get(4)?,
                        decision: row.get(5)?,
                        assurance_label: row.get(6)?,
                        findings_json: row.get(7)?,
                        stop_reasons_json: row.get(8)?,
                        warrant_json: row.get(9)?,
                        warrant_consumed: row.get(10)?,
                        created_at: row.get(11)?,
                    })
                },
            )
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }
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

    #[test]
    fn verification_gate_never_accepts_evidence_from_another_repository() {
        let db = database();
        let evidence = r#"{"check_id":"verify","argv":[],"working_directory":".","environment_identity":{},"tool_identity":null,"tested_identity":"revision","started_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:00:01Z","duration_ms":1,"exit_code":0,"signal":null,"status":"passed","required":true,"summary":"ok","stdout":null,"stderr":null,"truncated":false}"#;
        db.conn()
            .execute_batch(&format!(
                "INSERT INTO driver_sessions(session_id,repository_key,started_at,warrant_json,created_at) VALUES('other-session','/other/.git','2026-01-01T00:00:00Z','{{}}','2026-01-01T00:00:00Z');
                 INSERT INTO driver_attempts(session_id,sequence,prd_id,prd_path,execution_id,started_at) VALUES('other-session',1,'PRD-1','docs/prds/PRD-001.md','other-execution','2026-01-01T00:00:00Z');
                 INSERT INTO review_tasks(task_id,task_json,policy_json,created_at) VALUES('task','{{}}','{{}}','2026-01-01T00:00:00Z');
                 INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at) VALUES('other-execution-cycle','task',1,'complete','clean','{{}}','2026-01-01T00:00:00Z');
                 INSERT INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json,repository_key) VALUES('other-execution-cycle','verify','attempt-0','{}','/other/.git');",
                evidence.replace('\'', "''")
            ))
            .unwrap();

        let repository = DeliveryRepository::new(db.conn());
        assert!(repository
            .resolve_internal_gate("/repo/.git", "verification:verify")
            .unwrap()
            .is_none());
        assert!(
            repository
                .resolve_internal_gate("/other/.git", "verification:verify")
                .unwrap()
                .unwrap()
                .passed
        );
    }

    #[test]
    fn list_decisions_is_repository_scoped_ordered_and_paginated() {
        let db = database();
        let repository = DeliveryRepository::new(db.conn());
        for (n, decision_id) in ["d1", "d2", "d3"].into_iter().enumerate() {
            repository
                .record_authority_decision(
                    decision_id,
                    "/repo/.git",
                    "session-1",
                    &format!("PRD-{n}"),
                    "manual",
                    "human:tester",
                    "approved",
                    None,
                    "[]",
                    "[]",
                    None,
                    0,
                )
                .unwrap();
        }
        // A decision from a different repository must never leak into a
        // repository-scoped listing.
        repository
            .record_authority_decision(
                "other-1",
                "/other/.git",
                "session-1",
                "PRD-0",
                "manual",
                "human:tester",
                "approved",
                None,
                "[]",
                "[]",
                None,
                0,
            )
            .unwrap();

        let all = repository.list_decisions("/repo/.git", None, 10).unwrap();
        assert_eq!(
            all.iter()
                .map(|d| d.decision_id.as_str())
                .collect::<Vec<_>>(),
            ["d1", "d2", "d3"]
        );

        let first_page = repository.list_decisions("/repo/.git", None, 1).unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].decision_id, "d1");

        let rest = repository
            .list_decisions("/repo/.git", Some(&first_page[0].decision_id), 10)
            .unwrap();
        assert_eq!(
            rest.iter()
                .map(|d| d.decision_id.as_str())
                .collect::<Vec<_>>(),
            ["d2", "d3"]
        );

        // An unknown cursor yields an empty continuation rather than
        // silently restarting the sequence.
        assert!(repository
            .list_decisions("/repo/.git", Some("nope"), 10)
            .unwrap()
            .is_empty());
    }
}
