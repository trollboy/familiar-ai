use chrono::Utc;
use familiar_ai_core::FamiliarError;
use familiar_ai_review::{
    ArtifactRef, BlockingPolicy, EvidenceRef, LessonClassification, LessonProposal, LessonStatus,
    ReviewCycle, ReviewStore, ReviewTask, ReviewWaiver,
};
use rusqlite::{params, Connection, OptionalExtension};

pub struct ReviewRepository<'a> {
    conn: &'a Connection,
}
impl<'a> ReviewRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    pub fn insert_task(
        &self,
        task: &ReviewTask,
        policy: &BlockingPolicy,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO review_tasks(task_id,task_json,policy_json,created_at) VALUES(?1,?2,?3,?4)",params![task.task_id,json(task)?,json(policy)?,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }
    pub fn get_cycle(&self, id: &str) -> familiar_ai_core::Result<Option<ReviewCycle>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT cycle_json FROM review_cycles WHERE cycle_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        raw.map(|v| serde_json::from_str(&v).map_err(|e| FamiliarError::Database(e.to_string())))
            .transpose()
    }
    /// Attribute a human exception to one exact open finding. The row and the
    /// reportable cycle snapshot are committed together.
    pub fn waive_finding(
        &self,
        cycle_id: &str,
        finding_id: &str,
        actor: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<ReviewWaiver> {
        if !actor.starts_with("human:") || actor.trim() == "human:" || reason.trim().is_empty() {
            return Err(FamiliarError::Database(
                "waiver requires actor and reason".into(),
            ));
        }
        let mut cycle = self
            .get_cycle(cycle_id)?
            .ok_or_else(|| FamiliarError::Database(format!("review cycle {cycle_id} not found")))?;
        let open: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_findings WHERE cycle_id=?1 AND finding_id=?2 AND status='open' AND (blocking=1 OR acceptance_criterion_id IS NOT NULL))",
            params![cycle_id, finding_id], |row| row.get(0)).map_err(db)?;
        if !open {
            return Err(FamiliarError::Database(format!(
                "finding {finding_id} is not an open blocking or acceptance-criterion finding"
            )));
        }
        // Substance identity survives reviewer id/prose rotation between
        // attempts (FAM-BUG-044); completion matches by id OR substance.
        let substance: String = self
            .conn
            .query_row(
                "SELECT finding_json FROM review_findings WHERE cycle_id=?1 AND finding_id=?2",
                params![cycle_id, finding_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db)?
            .and_then(|json| serde_json::from_str::<familiar_ai_review::ReviewFinding>(&json).ok())
            .map(|finding| familiar_ai_review::review_finding_substance_json(&finding))
            .unwrap_or_default();
        let waiver = ReviewWaiver {
            waiver_id: format!("{cycle_id}:{finding_id}"),
            cycle_id: cycle_id.into(),
            finding_id: finding_id.into(),
            actor: actor.into(),
            reason: reason.into(),
            created_at: Utc::now().to_rfc3339(),
        };
        cycle.waivers.retain(|value| value.finding_id != finding_id);
        cycle.waivers.push(waiver.clone());
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT INTO review_finding_waivers(waiver_id,cycle_id,finding_id,finding_substance,actor,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(cycle_id,finding_id) DO UPDATE SET waiver_id=excluded.waiver_id,finding_substance=excluded.finding_substance,actor=excluded.actor,reason=excluded.reason,created_at=excluded.created_at", params![waiver.waiver_id, cycle_id, finding_id, substance, actor, reason, waiver.created_at]).map_err(db)?;
        tx.execute(
            "UPDATE review_cycles SET cycle_json=?2 WHERE cycle_id=?1",
            params![cycle_id, json(&cycle)?],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(waiver)
    }
    pub fn recover_incomplete(&self) -> familiar_ai_core::Result<usize> {
        let mut stmt=self.conn.prepare("SELECT cycle_id,cycle_json FROM review_cycles WHERE state NOT IN ('completed','human_review_required','interrupted')").map_err(db)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let mut count = 0;
        for (id, raw) in rows {
            let mut cycle: ReviewCycle =
                serde_json::from_str(&raw).map_err(|e| FamiliarError::Database(e.to_string()))?;
            cycle.state = familiar_ai_review::ReviewCycleState::Interrupted;
            cycle.disposition = familiar_ai_review::ReviewDisposition::HumanReviewRequired;
            cycle
                .stop_reasons
                .push(familiar_ai_review::ReviewStopReason::Interrupted);
            cycle.ended_at = Some(Utc::now().to_rfc3339());
            // Recovery only marks the cycle interrupted; a full save_cycle
            // would rewrite the evidence tables wholesale and refuses cycles
            // whose JSON predates repository_key — one legacy row would then
            // wedge every future attempt at startup (FAM-BUG-032).
            let updated = serde_json::to_string(&cycle)
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            self.conn
                .execute(
                    "UPDATE review_cycles SET state=?2,disposition=?3,cycle_json=?4,ended_at=?5 WHERE cycle_id=?1",
                    params![
                        id,
                        enum_json(&cycle.state).map_err(FamiliarError::Database)?,
                        enum_json(&cycle.disposition).map_err(FamiliarError::Database)?,
                        updated,
                        cycle.ended_at
                    ],
                )
                .map_err(db)?;
            count += 1;
        }
        Ok(count)
    }
    pub fn insert_lesson(&self, lesson: &LessonProposal) -> familiar_ai_core::Result<()> {
        let finding_status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM review_findings WHERE finding_id=?1",
                [&lesson.provenance.finding_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        if finding_status.as_deref() != Some("resolved") {
            return Err(FamiliarError::Database(
                "lesson requires a canonical resolved finding".into(),
            ));
        }
        if lesson.status != LessonStatus::Proposed || lesson.reviewed_by.is_some() {
            return Err(FamiliarError::Database(
                "new lesson must be an unreviewed proposal".into(),
            ));
        }
        let classification = enum_json(&lesson.classification).map_err(FamiliarError::Database)?;
        let status = enum_json(&lesson.status).map_err(FamiliarError::Database)?;
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT INTO lesson_proposals(lesson_id,project_id,finding_id,classification,status,proposal_json,proposed_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![lesson.lesson_id,lesson.project_id,lesson.provenance.finding_id,classification,status,json(lesson)?,lesson.proposed_at]).map_err(db)?;
        tx.execute("INSERT INTO lesson_proposal_events(lesson_id,sequence,status,occurred_at) VALUES(?1,1,'proposed',?2)",params![lesson.lesson_id,lesson.proposed_at]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }
    pub fn get_lesson(&self, id: &str) -> familiar_ai_core::Result<Option<LessonProposal>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT proposal_json FROM lesson_proposals WHERE lesson_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        raw.map(|value| {
            serde_json::from_str(&value).map_err(|error| FamiliarError::Database(error.to_string()))
        })
        .transpose()
    }
    pub fn approve_lesson(
        &self,
        id: &str,
        approval: familiar_ai_review::HumanApproval,
    ) -> familiar_ai_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Approved, approval)
    }
    pub fn reject_lesson(
        &self,
        id: &str,
        approval: familiar_ai_review::HumanApproval,
    ) -> familiar_ai_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Rejected, approval)
    }
    pub fn supersede_lesson(
        &self,
        id: &str,
        approval: familiar_ai_review::HumanApproval,
    ) -> familiar_ai_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Superseded, approval)
    }
    fn transition_lesson(
        &self,
        id: &str,
        status: LessonStatus,
        approval: familiar_ai_review::HumanApproval,
    ) -> familiar_ai_core::Result<()> {
        let mut lesson = self
            .get_lesson(id)?
            .ok_or_else(|| FamiliarError::Database(format!("lesson {id} not found")))?;
        if lesson.status != LessonStatus::Proposed && status != LessonStatus::Superseded {
            return Err(FamiliarError::Database(
                "lesson transition requires proposed state".into(),
            ));
        }
        if lesson.classification == LessonClassification::OneOffFinding
            && status == LessonStatus::Approved
        {
            return Err(FamiliarError::Database(
                "one-off findings cannot become approved guidance".into(),
            ));
        }
        if lesson.classification == LessonClassification::ProjectInvariant
            && status == LessonStatus::Approved
            && (approval.exact_statement != lesson.statement
                || approval.source_revision != lesson.provenance.source_revision)
        {
            return Err(FamiliarError::Database(
                "project invariant approval must identify the exact statement and source revision"
                    .into(),
            ));
        }
        lesson.status = status;
        lesson.reviewed_by = Some(approval.clone());
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute(
            "UPDATE lesson_proposals SET status=?2,proposal_json=?3 WHERE lesson_id=?1",
            params![
                id,
                enum_json(&status).map_err(FamiliarError::Database)?,
                json(&lesson)?
            ],
        )
        .map_err(db)?;
        tx.execute("INSERT INTO lesson_proposal_events(lesson_id,sequence,status,actor_json,occurred_at) SELECT ?1,COALESCE(MAX(sequence),0)+1,?2,?3,?4 FROM lesson_proposal_events WHERE lesson_id=?1",params![id,enum_json(&status).map_err(FamiliarError::Database)?,json(&approval)?,Utc::now().to_rfc3339()]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }
}
impl ReviewStore for ReviewRepository<'_> {
    fn load_cycle(&self, cycle_id: &str) -> Result<Option<ReviewCycle>, String> {
        self.get_cycle(cycle_id).map_err(|error| error.to_string())
    }
    fn save_cycle(&self, cycle: &ReviewCycle) -> Result<(), String> {
        let raw = serde_json::to_string(cycle).map_err(|e| e.to_string())?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at,ended_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(cycle_id) DO UPDATE SET attempt=excluded.attempt,state=excluded.state,disposition=excluded.disposition,cycle_json=excluded.cycle_json,ended_at=excluded.ended_at",params![cycle.cycle_id,cycle.task_id,cycle.attempt,enum_json(&cycle.state)?,enum_json(&cycle.disposition)?,raw,cycle.started_at,cycle.ended_at]).map_err(|e|e.to_string())?;
        if let Some(selection) = &cycle.tier_selection {
            tx.execute("INSERT INTO review_tier_selections(cycle_id,tier,selecting_rule,selection_json) VALUES(?1,?2,?3,?4) ON CONFLICT(cycle_id) DO UPDATE SET tier=excluded.tier,selecting_rule=excluded.selecting_rule,selection_json=excluded.selection_json", params![cycle.cycle_id, enum_json(&selection.tier)?, selection.selecting_rule, serde_json::to_string(selection).map_err(|error| error.to_string())?]).map_err(|error| error.to_string())?;
        }
        if let Some(result) = &cycle.review_result {
            // review_findings is the CURRENT view of the cycle; a replayed
            // review supersedes earlier attempts' rows entirely. The
            // append-only history stays in review_finding_events.
            tx.execute(
                "DELETE FROM review_findings WHERE cycle_id=?1",
                params![cycle.cycle_id],
            )
            .map_err(|e| e.to_string())?;
            for f in &result.findings {
                tx.execute("INSERT OR REPLACE INTO review_findings(finding_id,cycle_id,category,severity,blocking,status,finding_json,acceptance_criterion_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![f.finding_id,cycle.cycle_id,enum_json(&f.category)?,enum_json(&f.severity)?,f.blocking,enum_json(&f.status)?,serde_json::to_string(f).map_err(|e|e.to_string())?,f.acceptance_criterion_id]).map_err(|e|e.to_string())?;
                tx.execute("INSERT OR IGNORE INTO review_finding_events(cycle_id,finding_id,review_attempt,status,finding_json) VALUES(?1,?2,?3,?4,?5)",params![cycle.cycle_id,f.finding_id,cycle.attempt,enum_json(&f.status)?,serde_json::to_string(f).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
            }
        }
        for stage in cycle
            .implementation_execution
            .iter()
            .chain(&cycle.review_attempts)
            .chain(&cycle.remediation_attempts)
        {
            tx.execute("INSERT OR REPLACE INTO review_stage_executions(cycle_id,stage_id,stage_kind,observation_json) VALUES(?1,?2,?3,?4)",params![cycle.cycle_id,stage.stage_id,enum_json(&stage.kind)?,serde_json::to_string(stage).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        }
        // The evidence table is the CURRENT verification history of the
        // cycle; a replayed cycle supersedes earlier rounds' rows entirely
        // (stale higher-index phases otherwise wedge the completion
        // completeness check forever, exactly like review_findings).
        tx.execute(
            "DELETE FROM review_verification_evidence WHERE cycle_id=?1",
            params![cycle.cycle_id],
        )
        .map_err(|e| e.to_string())?;
        for (index, e) in cycle.verification_history.iter().enumerate() {
            let phase = format!("attempt-{index}");
            tx.execute("INSERT OR REPLACE INTO review_stage_executions(cycle_id,stage_id,stage_kind,observation_json) VALUES(?1,?2,'verification',?3)",params![cycle.cycle_id,format!("{phase}-{}",e.check_id),serde_json::to_string(e).map_err(|error|error.to_string())?]).map_err(|error|error.to_string())?;
            if cycle.repository_key.is_empty() {
                return Err("verification evidence requires repository identity".into());
            }
            tx.execute("INSERT OR REPLACE INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json,repository_key,environment_identity_json,classification) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![cycle.cycle_id,e.check_id,phase,serde_json::to_string(e).map_err(|e|e.to_string())?,cycle.repository_key,serde_json::to_string(&e.environment_identity).map_err(|e|e.to_string())?,enum_json(&e.status)?]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
    fn save_artifact(&self, kind: &str, value: &[u8]) -> Result<ArtifactRef, String> {
        let hash = familiar_ai_review::content_hash(value);
        self.conn.execute("INSERT OR IGNORE INTO review_artifacts(content_hash,kind,media_type,byte_size,content,created_at) VALUES(?1,?2,'application/json',?3,?4,?5)",params![hash,kind,u64::try_from(value.len()).map_err(|_|"artifact size overflow")?,value,Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        Ok(EvidenceRef {
            content_hash: hash.clone(),
            media_type: "application/json".into(),
            byte_size: u64::try_from(value.len()).map_err(|_| "artifact size overflow")?,
            repository: String::new(),
            revision: String::new(),
            storage_ref: format!("sqlite:review_artifacts/{hash}"),
            truncated: false,
            omitted_bytes: 0,
        })
    }
}
fn json<T: serde::Serialize>(v: &T) -> familiar_ai_core::Result<String> {
    serde_json::to_string(v).map_err(|e| FamiliarError::Database(e.to_string()))
}
fn enum_json<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v)
        .map(|s| s.trim_matches('"').to_owned())
        .map_err(|e| e.to_string())
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_review::{
        AgentAssignment, AgentObservation, AgentRole, ExecutionUsage, ExpectedFileMatch,
        ExpectedMatchKind, GitChangeKind, ReviewCycleState, ReviewDisposition, ScopeCheckResult,
        ScopeDecision, ScopeDisposition, ScopeFileClass, ScopeFinding, ScopeRuleSource,
    };
    use std::collections::BTreeMap;

    #[test]
    fn cycle_round_trips_and_recovery_marks_it_interrupted_without_replay() {
        let db = crate::Database::open_in_memory().unwrap();
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
        let cycle = ReviewCycle {
            cycle_id: "cycle".into(),
            task_id: task.task_id,
            repository_key: "repo/.git".into(),
            attempt: 0,
            state: ReviewCycleState::Verifying,
            implementation: AgentObservation {
                assignment: AgentAssignment {
                    adapter_id: "fake".into(),
                    agent_id: "fake".into(),
                    provider: None,
                    requested_model: None,
                    role: AgentRole::Implementation,
                    session_id: Some("implementation".into()),
                },
                agent_version: None,
                reported_model: None,
                unavailable_fields: BTreeMap::new(),
            },
            implementation_execution: None,
            reviewer: None,
            independence: None,
            review_request: None,
            review_result: None,
            remediation_request: None,
            remediation_result: None,
            verification_before_review: vec![],
            verification_after_remediation: vec![],
            verification_history: vec![],
            waivers: vec![],
            scope_policy_snapshot: Some(
                repository
                    .save_artifact(
                        "scope_policy_snapshot",
                        b"{\"schema_version\":\"scope-policy/1\"}",
                    )
                    .unwrap(),
            ),
            scope_evaluations: vec![ScopeCheckResult {
                added: vec!["src/new.rs".into()],
                modified: vec![],
                deleted: vec![],
                renamed: vec![],
                disposition: ScopeDisposition::Broadened,
                findings: vec![ScopeFinding {
                    finding_id: "added:-:src/new.rs#new".into(),
                    change_id: "added:-:src/new.rs".into(),
                    path: "src/new.rs".into(),
                    old_path: None,
                    change_kind: GitChangeKind::Added,
                    file_class: ScopeFileClass::OrdinarySource,
                    decision: ScopeDecision::UndeclaredScopeExpansion,
                    rule_id: "static_allowed_path_ceiling".into(),
                    rule_source: ScopeRuleSource::Configuration,
                    rule_detail: "declared at Expected Files line 7 but expansion disabled".into(),
                    expected_file_match: Some(ExpectedFileMatch {
                        normalized: "src/new.rs".into(),
                        source_line: 7,
                        match_kind: ExpectedMatchKind::ExactFile,
                    }),
                    allowed_path_match: None,
                    prohibited_rule_match: None,
                    policy_snapshot_hash: "sha256:policy".into(),
                }],
                policy_snapshot_hash: "sha256:policy".into(),
                phase: "initial".into(),
            }],
            tier_selection: None,
            batch_pending: false,
            aggregate_usage: ExecutionUsage::default(),
            aggregate_duration_ms: 0,
            started_at: "2026-08-03T00:00:00Z".into(),
            ended_at: None,
            disposition: ReviewDisposition::Pending,
            stop_reasons: vec![],
            review_attempts: vec![],
            remediation_attempts: vec![],
        };
        repository.save_cycle(&cycle).unwrap();
        assert_eq!(repository.get_cycle("cycle").unwrap(), Some(cycle.clone()));
        let reloaded = repository.get_cycle("cycle").unwrap().unwrap();
        assert_eq!(reloaded.scope_evaluations, cycle.scope_evaluations);
        assert_eq!(reloaded.scope_policy_snapshot, cycle.scope_policy_snapshot);
        assert_eq!(repository.recover_incomplete().unwrap(), 1);
        let recovered = repository.get_cycle("cycle").unwrap().unwrap();
        assert_eq!(recovered.state, ReviewCycleState::Interrupted);
        assert_eq!(
            recovered.disposition,
            ReviewDisposition::HumanReviewRequired
        );
    }

    #[test]
    fn recovery_survives_legacy_keyless_cycle_and_preserves_evidence_rows() {
        // FAM-BUG-032: a pre-repository_key cycle (empty key after serde
        // default) with verification history sat non-terminal in the machine
        // database; recovery re-saved it through save_cycle, which refuses
        // keyless verification evidence — wedging every future attempt.
        let db = crate::Database::open_in_memory().unwrap();
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
        let legacy = serde_json::json!({
            "cycle_id": "legacy-keyless",
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
            "aggregate_usage": ExecutionUsage::default(),
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
                "INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at,ended_at) VALUES('legacy-keyless','task',1,'awaiting_review','pending',?1,'2026-08-03T00:00:00Z',NULL)",
                params![legacy.to_string()],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json,repository_key,environment_identity_json,classification) VALUES('legacy-keyless','tests','attempt-0','{}','old/repo/.git','{}','passed')",
                [],
            )
            .unwrap();
        assert_eq!(repository.recover_incomplete().unwrap(), 1);
        let recovered = repository.get_cycle("legacy-keyless").unwrap().unwrap();
        assert_eq!(recovered.state, ReviewCycleState::Interrupted);
        assert_eq!(
            recovered.disposition,
            ReviewDisposition::HumanReviewRequired
        );
        assert_eq!(recovered.repository_key, "");
        // Recovery must not touch the evidence table it can no longer derive.
        let evidence: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM review_verification_evidence WHERE cycle_id='legacy-keyless'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(evidence, 1);
        // Idempotent: interrupted cycles are terminal for recovery.
        assert_eq!(repository.recover_incomplete().unwrap(), 0);
    }

    #[test]
    fn legacy_cycle_json_without_scope_fields_still_deserializes() {
        let db = crate::Database::open_in_memory().unwrap();
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
        let modern = serde_json::json!({
            "cycle_id": "legacy",
            "task_id": "task",
            "attempt": 1,
            "state": "verifying",
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
            "verification_history": [],
            "aggregate_usage": ExecutionUsage::default(),
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
                "INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at,ended_at) VALUES('legacy','task',1,'\"verifying\"','\"pending\"',?1,'2026-08-03T00:00:00Z',NULL)",
                params![modern.to_string()],
            )
            .unwrap();
        let cycle = repository.get_cycle("legacy").unwrap().unwrap();
        assert_eq!(cycle.scope_policy_snapshot, None);
        assert!(cycle.scope_evaluations.is_empty());
    }

    #[test]
    fn lesson_lifecycle_uses_canonical_finding_state_and_human_transitions() {
        use familiar_ai_review::{
            EvidenceRef, HumanApproval, LessonApplicability, LessonClassification, LessonProposal,
            LessonProvenance, LessonStatus,
        };
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db.conn().execute("INSERT INTO review_tasks(task_id,task_json,policy_json,created_at) VALUES('task','{}','{}','now')",[]).unwrap();
        db.conn().execute("INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at) VALUES('cycle','task',1,'completed','ready_for_human_approval','{}','now')",[]).unwrap();
        db.conn().execute("INSERT INTO review_findings(finding_id,cycle_id,category,severity,blocking,status,finding_json) VALUES('finding','cycle','correctness_defect','high',1,'open','{}')",[]).unwrap();
        let repository = ReviewRepository::new(db.conn());
        let evidence = EvidenceRef {
            content_hash: "sha256:evidence".into(),
            media_type: "text/plain".into(),
            byte_size: 1,
            repository: "repo".into(),
            revision: "revision".into(),
            storage_ref: "artifact".into(),
            truncated: false,
            omitted_bytes: 0,
        };
        let proposal = LessonProposal {
            lesson_id: "lesson".into(),
            project_id: "project".into(),
            classification: LessonClassification::ProjectInvariant,
            statement: "exact invariant".into(),
            rationale: "resolved defect".into(),
            applicability: LessonApplicability {
                project_id: "project".into(),
                paths: vec!["src/".into()],
                categories: vec![],
                exclusions: vec![],
                max_future_tokens: 10,
            },
            provenance: LessonProvenance {
                finding_id: "finding".into(),
                review_cycle_id: "cycle".into(),
                remediation_id: "remediation".into(),
                resolution_evidence: vec![evidence],
                source_revision: "revision".into(),
            },
            status: LessonStatus::Proposed,
            proposed_at: "2026-08-03T00:00:00Z".into(),
            reviewed_by: None,
        };
        assert!(repository.insert_lesson(&proposal).is_err());
        db.conn()
            .execute(
                "UPDATE review_findings SET status='resolved' WHERE finding_id='finding'",
                [],
            )
            .unwrap();
        repository.insert_lesson(&proposal).unwrap();
        let wrong = HumanApproval {
            human_id: "human".into(),
            approved_at: "now".into(),
            source_revision: "revision".into(),
            exact_statement: "different".into(),
        };
        assert!(repository.approve_lesson("lesson", wrong).is_err());
        let approval = HumanApproval {
            human_id: "human".into(),
            approved_at: "now".into(),
            source_revision: "revision".into(),
            exact_statement: "exact invariant".into(),
        };
        repository
            .approve_lesson("lesson", approval.clone())
            .unwrap();
        assert_eq!(
            repository.get_lesson("lesson").unwrap().unwrap().status,
            LessonStatus::Approved
        );
        let events: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM lesson_proposal_events WHERE lesson_id='lesson'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 2);
        let mut rejected = proposal.clone();
        rejected.lesson_id = "rejected".into();
        repository.insert_lesson(&rejected).unwrap();
        repository
            .reject_lesson("rejected", approval.clone())
            .unwrap();
        assert_eq!(
            repository.get_lesson("rejected").unwrap().unwrap().status,
            LessonStatus::Rejected
        );
        let mut superseded = proposal;
        superseded.lesson_id = "superseded".into();
        repository.insert_lesson(&superseded).unwrap();
        repository.supersede_lesson("superseded", approval).unwrap();
        assert_eq!(
            repository.get_lesson("superseded").unwrap().unwrap().status,
            LessonStatus::Superseded
        );
    }
}
