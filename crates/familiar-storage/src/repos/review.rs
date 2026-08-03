use chrono::Utc;
use familiar_core::FamiliarError;
use familiar_review::{
    ArtifactRef, BlockingPolicy, EvidenceRef, LessonClassification, LessonProposal, LessonStatus,
    ReviewCycle, ReviewStore, ReviewTask,
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
    ) -> familiar_core::Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO review_tasks(task_id,task_json,policy_json,created_at) VALUES(?1,?2,?3,?4)",params![task.task_id,json(task)?,json(policy)?,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }
    pub fn get_cycle(&self, id: &str) -> familiar_core::Result<Option<ReviewCycle>> {
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
    pub fn recover_incomplete(&self) -> familiar_core::Result<usize> {
        let mut stmt=self.conn.prepare("SELECT cycle_id,cycle_json FROM review_cycles WHERE state NOT IN ('completed','human_review_required','interrupted')").map_err(db)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let mut count = 0;
        for (_id, raw) in rows {
            let mut cycle: ReviewCycle =
                serde_json::from_str(&raw).map_err(|e| FamiliarError::Database(e.to_string()))?;
            cycle.state = familiar_review::ReviewCycleState::Interrupted;
            cycle.disposition = familiar_review::ReviewDisposition::HumanReviewRequired;
            cycle
                .stop_reasons
                .push(familiar_review::ReviewStopReason::Interrupted);
            cycle.ended_at = Some(Utc::now().to_rfc3339());
            self.save_cycle(&cycle).map_err(FamiliarError::Database)?;
            count += 1;
        }
        Ok(count)
    }
    pub fn insert_lesson(&self, lesson: &LessonProposal) -> familiar_core::Result<()> {
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
    pub fn get_lesson(&self, id: &str) -> familiar_core::Result<Option<LessonProposal>> {
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
        approval: familiar_review::HumanApproval,
    ) -> familiar_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Approved, approval)
    }
    pub fn reject_lesson(
        &self,
        id: &str,
        approval: familiar_review::HumanApproval,
    ) -> familiar_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Rejected, approval)
    }
    pub fn supersede_lesson(
        &self,
        id: &str,
        approval: familiar_review::HumanApproval,
    ) -> familiar_core::Result<()> {
        self.transition_lesson(id, LessonStatus::Superseded, approval)
    }
    fn transition_lesson(
        &self,
        id: &str,
        status: LessonStatus,
        approval: familiar_review::HumanApproval,
    ) -> familiar_core::Result<()> {
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
    fn save_cycle(&self, cycle: &ReviewCycle) -> Result<(), String> {
        let raw = serde_json::to_string(cycle).map_err(|e| e.to_string())?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at,ended_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(cycle_id) DO UPDATE SET attempt=excluded.attempt,state=excluded.state,disposition=excluded.disposition,cycle_json=excluded.cycle_json,ended_at=excluded.ended_at",params![cycle.cycle_id,cycle.task_id,cycle.attempt,enum_json(&cycle.state)?,enum_json(&cycle.disposition)?,raw,cycle.started_at,cycle.ended_at]).map_err(|e|e.to_string())?;
        if let Some(result) = &cycle.review_result {
            for f in &result.findings {
                tx.execute("INSERT OR REPLACE INTO review_findings(finding_id,cycle_id,category,severity,blocking,status,finding_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![f.finding_id,cycle.cycle_id,enum_json(&f.category)?,enum_json(&f.severity)?,f.blocking,enum_json(&f.status)?,serde_json::to_string(f).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
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
        for (index, e) in cycle.verification_history.iter().enumerate() {
            let phase = format!("attempt-{index}");
            tx.execute("INSERT OR REPLACE INTO review_stage_executions(cycle_id,stage_id,stage_kind,observation_json) VALUES(?1,?2,'verification',?3)",params![cycle.cycle_id,format!("{phase}-{}",e.check_id),serde_json::to_string(e).map_err(|error|error.to_string())?]).map_err(|error|error.to_string())?;
            tx.execute("INSERT OR REPLACE INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json) VALUES(?1,?2,?3,?4)",params![cycle.cycle_id,e.check_id,phase,serde_json::to_string(e).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
    fn save_artifact(&self, kind: &str, value: &[u8]) -> Result<ArtifactRef, String> {
        let hash = familiar_review::content_hash(value);
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
fn json<T: serde::Serialize>(v: &T) -> familiar_core::Result<String> {
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
    use familiar_review::{
        AgentAssignment, AgentObservation, AgentRole, ExecutionUsage, ReviewCycleState,
        ReviewDisposition,
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
        assert_eq!(repository.get_cycle("cycle").unwrap(), Some(cycle));
        assert_eq!(repository.recover_incomplete().unwrap(), 1);
        let recovered = repository.get_cycle("cycle").unwrap().unwrap();
        assert_eq!(recovered.state, ReviewCycleState::Interrupted);
        assert_eq!(
            recovered.disposition,
            ReviewDisposition::HumanReviewRequired
        );
    }

    #[test]
    fn lesson_lifecycle_uses_canonical_finding_state_and_human_transitions() {
        use familiar_review::{
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
