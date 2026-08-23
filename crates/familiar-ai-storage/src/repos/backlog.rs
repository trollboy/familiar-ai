use familiar_ai_core::{
    validate_recovery_attribution, BacklogEntry, BacklogRecoveryAction, BacklogStatus,
    BacklogStatusStore, BacklogStoreError, DiscoveredPrd, PrdId, PrdLocation, RepositoryIdentity,
    RepositoryPath,
};
use familiar_ai_review::{
    FindingStatus, ReviewCycle, ReviewCycleState, ReviewDisposition, ReviewFinding, ReviewRequest,
    ReviewStopReason, ReviewTask, VerificationEvidence, VerificationStatus,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub struct SqliteBacklogRepository<'a> {
    connection: &'a mut Connection,
}

type PersistedBacklogIdentityRow = (u64, Option<String>, String, String, Option<String>);

impl<'a> SqliteBacklogRepository<'a> {
    pub fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
    }

    pub fn recover(
        &mut self,
        repository: &RepositoryIdentity,
        target: &DiscoveredPrd,
        action: BacklogRecoveryAction,
        actor: &str,
        reason: &str,
    ) -> Result<BacklogEntry, BacklogStoreError> {
        let (actor, reason) = validate_recovery_attribution(action, actor, reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let row: Option<PersistedBacklogIdentityRow> = tx
            .query_row(
                "SELECT prd_number,prd_suffix,content_hash,status,missing_since FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",
                params![repository.key, target.path.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(storage)?;
        let (number, suffix, hash, status, missing) =
            row.ok_or_else(|| BacklogStoreError::NotFound(target.path.clone()))?;
        if status != "in_progress"
            || number != target.number
            || suffix.as_deref().and_then(|s| s.chars().next()) != target.id.suffix()
            || hash != target.content_hash
            || missing.is_some()
        {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "in_progress",
                actual: BacklogStatus::parse(&status)
                    .map(|value| value.as_str())
                    .unwrap_or("conflict"),
            });
        }
        let latest: Option<(String, String, String)> = tx
            .query_row(
                "SELECT old_status,new_status,actor FROM backlog_status_events WHERE repository_key=?1 AND prd_path=?2 ORDER BY event_id DESC LIMIT 1",
                params![repository.key, target.path.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(storage)?;
        if !matches!(latest, Some((ref old, ref new, ref claim_actor)) if old == "pending" && new == "in_progress" && validate_run_actor(claim_actor).is_ok())
        {
            return Err(BacklogStoreError::RecoveryAuditCorrupt(target.path.clone()));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let next = action.target_status();
        let changed = tx
            .execute(
                "UPDATE backlog_prds SET status=?3,updated_at=?4 WHERE repository_key=?1 AND prd_path=?2 AND status='in_progress' AND content_hash=?5 AND prd_number=?6 AND missing_since IS NULL",
                params![repository.key, target.path.as_str(), next.as_str(), now, target.content_hash, target.number],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "in_progress",
                actual: "conflict",
            });
        }
        tx.execute(
            "INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(?1,?2,'in_progress',?3,?4,?5)",
            params![repository.key, target.path.as_str(), next.as_str(), actor, now],
        )
        .map_err(storage)?;
        let event_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(?1,?2,?3)",
            params![event_id, action.as_str(), reason],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: target.clone(),
            status: next,
        })
    }

    /// A human declaration that a `pending` PRD was completed outside Familiar's
    /// tracking. Legal only from `pending`, and only once every declared
    /// dependency is itself `completed`.
    pub fn record_complete(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        target: &DiscoveredPrd,
        actor: &str,
        reason: &str,
    ) -> Result<BacklogEntry, BacklogStoreError> {
        let (actor, reason) =
            validate_recovery_attribution(BacklogRecoveryAction::RecordedComplete, actor, reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let now = chrono::Utc::now().to_rfc3339();
        let row: Option<PersistedBacklogIdentityRow> = tx
            .query_row(
                "SELECT prd_number,prd_suffix,content_hash,status,missing_since FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",
                params![repository.key, target.path.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(storage)?;
        let (number, suffix, hash, status, missing) =
            row.ok_or_else(|| BacklogStoreError::NotFound(target.path.clone()))?;
        if status != "pending"
            || number != target.number
            || suffix.as_deref().and_then(|s| s.chars().next()) != target.id.suffix()
            || hash != target.content_hash
            || missing.is_some()
        {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "pending",
                actual: BacklogStatus::parse(&status)
                    .map(|value| value.as_str())
                    .unwrap_or("conflict"),
            });
        }
        reconcile(&tx, repository, discovered, &now)?;
        let entries = discovered
            .iter()
            .map(|prd| read_entry(&tx, &repository.key, prd))
            .collect::<Result<Vec<_>, _>>()?;
        let statuses: std::collections::BTreeMap<_, _> = entries
            .iter()
            .map(|entry| (entry.prd.id.clone(), entry.status))
            .collect();
        let incomplete: Vec<_> = target
            .dependencies
            .iter()
            .filter(|id| statuses.get(*id) != Some(&BacklogStatus::Completed))
            .cloned()
            .collect();
        if !incomplete.is_empty() {
            return Err(BacklogStoreError::IncompleteDependencies {
                path: target.path.clone(),
                dependencies: incomplete
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            });
        }
        let changed = tx
            .execute(
                "UPDATE backlog_prds SET status='completed',updated_at=?4 WHERE repository_key=?1 AND prd_path=?2 AND status='pending' AND content_hash=?3 AND prd_number=?5 AND missing_since IS NULL",
                params![repository.key, target.path.as_str(), target.content_hash, now, target.number],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "pending",
                actual: "conflict",
            });
        }
        tx.execute(
            "INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(?1,?2,'pending','completed',?3,?4)",
            params![repository.key, target.path.as_str(), actor, now],
        )
        .map_err(storage)?;
        let event_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(?1,?2,?3)",
            params![
                event_id,
                BacklogRecoveryAction::RecordedComplete.as_str(),
                reason
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: target.clone(),
            status: BacklogStatus::Completed,
        })
    }

    pub fn claim_run(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        target: &DiscoveredPrd,
        actor: &str,
    ) -> Result<BacklogEntry, BacklogStoreError> {
        validate_run_actor(actor)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let now = chrono::Utc::now().to_rfc3339();
        let persisted: Option<(String,String,Option<String>)> = tx.query_row("SELECT status,content_hash,missing_since FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",params![repository.key,target.path.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
        if match persisted.as_ref() {
            Some((status, hash, missing)) => {
                status != "pending" || hash != &target.content_hash || missing.is_some()
            }
            None => true,
        } {
            return Err(BacklogStoreError::Storage(
                "claim discovery or expected-status conflict".into(),
            ));
        }
        reconcile(&tx, repository, discovered, &now)?;
        let entries = discovered
            .iter()
            .map(|prd| read_entry(&tx, &repository.key, prd))
            .collect::<Result<Vec<_>, _>>()?;
        familiar_ai_core::admit_run_prd(&entries, target)
            .map_err(|error| BacklogStoreError::Storage(error.to_string()))?;
        // The row must still represent the exact discovery record being claimed.
        let changed = tx.execute(
            "UPDATE backlog_prds SET status='in_progress',updated_at=?4 WHERE repository_key=?1 AND prd_path=?2 AND content_hash=?3 AND missing_since IS NULL AND status='pending'",
            params![repository.key,target.path.as_str(),target.content_hash,now],
        ).map_err(storage)?;
        if changed != 1 {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "pending",
                actual: "conflict",
            });
        }
        tx.execute("INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(?1,?2,'pending','in_progress',?3,?4)",params![repository.key,target.path.as_str(),actor,now]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: target.clone(),
            status: BacklogStatus::InProgress,
        })
    }

    pub fn complete_run(
        &mut self,
        repository: &RepositoryIdentity,
        target: &DiscoveredPrd,
        execution_id: &str,
        actor: &str,
        required_checks: &[String],
    ) -> Result<BacklogEntry, BacklogStoreError> {
        validate_run_actor(actor)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let row: Option<(String,String,Option<String>)> = tx.query_row(
            "SELECT status,content_hash,missing_since FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",
            params![repository.key,target.path.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
        let (status, hash, missing) =
            row.ok_or_else(|| BacklogStoreError::NotFound(target.path.clone()))?;
        if status != "in_progress" || hash != target.content_hash || missing.is_some() {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "in_progress",
                actual: BacklogStatus::parse(&status)
                    .map(|s| s.as_str())
                    .unwrap_or("conflict"),
            });
        }
        let latest_event: (String,String,String) = tx.query_row("SELECT old_status,new_status,actor FROM backlog_status_events WHERE repository_key=?1 AND prd_path=?2 ORDER BY event_id DESC LIMIT 1",params![repository.key,target.path.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(storage)?;
        if latest_event != ("pending".into(), "in_progress".into(), actor.into()) {
            return Err(BacklogStoreError::Storage(
                "completion claim actor does not own latest event".into(),
            ));
        }
        let task_raw: String = tx
            .query_row(
                "SELECT task_json FROM review_tasks WHERE task_id=?1",
                [execution_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        let task: ReviewTask = serde_json::from_str(&task_raw)
            .map_err(|e| BacklogStoreError::Storage(format!("invalid review task: {e}")))?;
        if task.task_id != execution_id
            || task.verification_plan_id != format!("{execution_id}-verification")
        {
            return Err(BacklogStoreError::Storage(
                "review task execution mismatch".into(),
            ));
        }
        let (cycle_raw,state,disposition): (String,String,String) = tx.query_row("SELECT cycle_json,state,disposition FROM review_cycles WHERE task_id=?1 ORDER BY attempt DESC LIMIT 1",[execution_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(storage)?;
        let cycle: ReviewCycle = serde_json::from_str(&cycle_raw)
            .map_err(|e| BacklogStoreError::Storage(format!("invalid review cycle: {e}")))?;
        if state != "completed" || disposition != "ready_for_human_approval" {
            return Err(BacklogStoreError::Storage(
                "persisted review cycle columns are inconsistent".into(),
            ));
        }
        validate_completion_cycle(&cycle, execution_id, required_checks)?;
        validate_persisted_review_request(&tx, &cycle)?;
        validate_persisted_evidence(&tx, &cycle)?;
        validate_persisted_findings(&tx, &cycle)?;
        let now = chrono::Utc::now().to_rfc3339();
        let changed=tx.execute("UPDATE backlog_prds SET status='completed',updated_at=?3 WHERE repository_key=?1 AND prd_path=?2 AND status='in_progress'",params![repository.key,target.path.as_str(),now]).map_err(storage)?;
        if changed != 1 {
            return Err(BacklogStoreError::Conflict {
                path: target.path.clone(),
                expected: "in_progress",
                actual: "conflict",
            });
        }
        tx.execute("INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at)VALUES(?1,?2,'in_progress','completed',?3,?4)",params![repository.key,target.path.as_str(),actor,now]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: target.clone(),
            status: BacklogStatus::Completed,
        })
    }
}

fn validate_persisted_review_request(
    tx: &Transaction<'_>,
    cycle: &ReviewCycle,
) -> Result<(), BacklogStoreError> {
    let reference = cycle.review_request.as_ref().ok_or_else(|| {
        BacklogStoreError::Storage("terminal review request evidence is missing".into())
    })?;
    let expected_storage_ref = format!("sqlite:review_artifacts/{}", reference.content_hash);
    if reference.media_type != "application/json"
        || reference.storage_ref != expected_storage_ref
        || reference.truncated
        || reference.omitted_bytes != 0
    {
        return Err(BacklogStoreError::Storage(
            "terminal review request reference is invalid".into(),
        ));
    }
    let (kind, media_type, byte_size, content): (String, String, u64, Vec<u8>) = tx
        .query_row(
            "SELECT kind,media_type,byte_size,content FROM review_artifacts WHERE content_hash=?1",
            [&reference.content_hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(storage)?;
    if kind != "review_request"
        || media_type != reference.media_type
        || byte_size != reference.byte_size
        || usize::try_from(byte_size).ok() != Some(content.len())
        || familiar_ai_review::content_hash(&content) != reference.content_hash
    {
        return Err(BacklogStoreError::Storage(
            "persisted terminal review request is corrupt".into(),
        ));
    }
    let request: ReviewRequest = serde_json::from_slice(&content).map_err(|error| {
        BacklogStoreError::Storage(format!(
            "invalid persisted terminal review request: {error}"
        ))
    })?;
    let result = cycle
        .review_result
        .as_ref()
        .ok_or_else(|| BacklogStoreError::Storage("terminal review result is missing".into()))?;
    let current = if cycle.verification_after_remediation.is_empty() {
        &cycle.verification_before_review
    } else {
        &cycle.verification_after_remediation
    };
    if request.task.task_id != cycle.task_id
        || result.review_id != request.review_id
        || result.reviewed_manifest_hash != request.manifest.manifest_hash
        || request.verification != *current
        || request
            .verification
            .iter()
            .any(|evidence| evidence.tested_identity != request.diff.content_hash)
    {
        return Err(BacklogStoreError::Storage(
            "terminal review request does not match accepted evidence".into(),
        ));
    }
    Ok(())
}

fn validate_persisted_evidence(
    tx: &Transaction<'_>,
    cycle: &ReviewCycle,
) -> Result<(), BacklogStoreError> {
    let mut statement = tx
        .prepare(
            "SELECT check_id,phase,evidence_json FROM review_verification_evidence \
             WHERE cycle_id=?1 ORDER BY phase,check_id",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([&cycle.cycle_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if rows.len() != cycle.verification_history.len() {
        return Err(BacklogStoreError::Storage(
            "persisted verification evidence is incomplete".into(),
        ));
    }
    for (index, expected) in cycle.verification_history.iter().enumerate() {
        let phase = format!("attempt-{index}");
        let row = rows
            .iter()
            .find(|(check_id, persisted_phase, _)| {
                check_id == &expected.check_id && persisted_phase == &phase
            })
            .ok_or_else(|| {
                BacklogStoreError::Storage("persisted verification evidence is incomplete".into())
            })?;
        let persisted: VerificationEvidence = serde_json::from_str(&row.2).map_err(|error| {
            BacklogStoreError::Storage(format!("invalid persisted verification evidence: {error}"))
        })?;
        if &persisted != expected {
            return Err(BacklogStoreError::Storage(
                "persisted verification evidence conflicts with terminal cycle".into(),
            ));
        }
    }
    Ok(())
}

fn validate_persisted_findings(
    tx: &Transaction<'_>,
    cycle: &ReviewCycle,
) -> Result<(), BacklogStoreError> {
    let expected = cycle
        .review_result
        .as_ref()
        .map_or(&[][..], |result| result.findings.as_slice());
    let mut statement = tx
        .prepare(
            "SELECT finding_id,blocking,status,finding_json FROM review_findings \
             WHERE cycle_id=?1 ORDER BY finding_id",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([&cycle.cycle_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if rows.len() != expected.len() {
        return Err(BacklogStoreError::Storage(
            "persisted review findings are incomplete".into(),
        ));
    }
    for finding in expected {
        let row = rows
            .iter()
            .find(|(finding_id, _, _, _)| finding_id == &finding.finding_id)
            .ok_or_else(|| {
                BacklogStoreError::Storage("persisted review findings are incomplete".into())
            })?;
        let persisted: ReviewFinding = serde_json::from_str(&row.3).map_err(|error| {
            BacklogStoreError::Storage(format!("invalid persisted review finding: {error}"))
        })?;
        if &persisted != finding
            || row.1 != finding.blocking
            || row.2
                != serde_json::to_string(&finding.status)
                    .map_err(|error| BacklogStoreError::Storage(error.to_string()))?
                    .trim_matches('"')
        {
            return Err(BacklogStoreError::Storage(
                "persisted review finding conflicts with terminal cycle".into(),
            ));
        }
    }
    if rows
        .iter()
        .any(|(_, blocking, status, _)| *blocking && status == "open")
    {
        return Err(BacklogStoreError::Storage(
            "terminal review has open blocking findings".into(),
        ));
    }
    Ok(())
}

fn validate_run_actor(actor: &str) -> Result<(), BacklogStoreError> {
    let id = actor
        .strip_prefix("system:familiar-ai-run:")
        .ok_or_else(|| BacklogStoreError::Storage("invalid run actor".into()))?;
    let parts = id.split('-').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0].len() != 20
        || parts[1].len() != 10
        || parts[2].len() != 6
        || !parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(BacklogStoreError::Storage("invalid run actor".into()));
    }
    Ok(())
}

fn validate_completion_cycle(
    cycle: &ReviewCycle,
    execution_id: &str,
    required_checks: &[String],
) -> Result<(), BacklogStoreError> {
    if cycle.task_id != execution_id
        || cycle.cycle_id != format!("{execution_id}-cycle")
        || cycle.state != ReviewCycleState::Completed
        || cycle.disposition != ReviewDisposition::ReadyForHumanApproval
        || cycle.stop_reasons != [ReviewStopReason::CleanReview]
    {
        return Err(BacklogStoreError::Storage(
            "review cycle is not a clean terminal result".into(),
        ));
    }
    if match cycle.review_result.as_ref() {
        Some(result) => result
            .findings
            .iter()
            .any(|f| f.blocking && f.status == FindingStatus::Open),
        None => true,
    } {
        return Err(BacklogStoreError::Storage(
            "terminal review has open blocking findings".into(),
        ));
    }
    let current = if cycle.verification_after_remediation.is_empty() {
        &cycle.verification_before_review
    } else {
        &cycle.verification_after_remediation
    };
    let mut accepted_identity: Option<&str> = None;
    for check in required_checks {
        let evidence = current
            .iter()
            .find(|e| &e.check_id == check && e.required && e.status == VerificationStatus::Passed)
            .ok_or_else(|| {
                BacklogStoreError::Storage(format!(
                    "required verification check {check} is not passed"
                ))
            })?;
        match accepted_identity {
            Some(identity) if identity != evidence.tested_identity => {
                return Err(BacklogStoreError::Storage(
                    "required verification candidate identities differ".into(),
                ))
            }
            None => accepted_identity = Some(&evidence.tested_identity),
            _ => {}
        }
        if evidence.tested_identity.is_empty() {
            return Err(BacklogStoreError::Storage(format!(
                "required verification check {check} has no candidate identity"
            )));
        }
    }
    Ok(())
}

fn reconcile(
    tx: &Transaction<'_>,
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    now: &str,
) -> Result<(), BacklogStoreError> {
    tx.execute("UPDATE backlog_prds SET missing_since=COALESCE(missing_since,?2),updated_at=CASE WHEN missing_since IS NULL THEN ?2 ELSE updated_at END WHERE repository_key=?1",params![repository.key,now]).map_err(storage)?;
    for prd in discovered {
        reconcile_prd(tx, repository, prd, now)?;
    }
    Ok(())
}

const ARCHIVED_RECONCILIATION_ACTOR: &str = "system:archived-prd-location";

fn reconcile_prd(
    tx: &Transaction<'_>,
    repository: &RepositoryIdentity,
    prd: &DiscoveredPrd,
    now: &str,
) -> Result<(), BacklogStoreError> {
    tx.execute("INSERT INTO backlog_prds(repository_key,prd_path,prd_number,prd_suffix,content_hash,status,discovered_at,last_seen_at,missing_since,created_at,updated_at)VALUES(?1,?2,?3,?4,?5,?6,?7,?7,NULL,?7,?7) ON CONFLICT(repository_key,prd_path) DO UPDATE SET prd_number=excluded.prd_number,prd_suffix=excluded.prd_suffix,content_hash=excluded.content_hash,last_seen_at=excluded.last_seen_at,missing_since=NULL",params![repository.key,prd.path.as_str(),prd.number.to_string(),prd.id.suffix().map(|c| c.to_string()),prd.content_hash,if prd.location == PrdLocation::Archived { "completed" } else { "pending" },now]).map_err(storage)?;
    if prd.location == PrdLocation::Archived {
        let old_status: String = tx
            .query_row(
                "SELECT status FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",
                params![repository.key, prd.path.as_str()],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if old_status != "completed" {
            tx.execute("UPDATE backlog_prds SET status='completed',updated_at=?3 WHERE repository_key=?1 AND prd_path=?2", params![repository.key, prd.path.as_str(), now]).map_err(storage)?;
            tx.execute("INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(?1,?2,?3,'completed',?4,?5)", params![repository.key, prd.path.as_str(), old_status, ARCHIVED_RECONCILIATION_ACTOR, now]).map_err(storage)?;
        }
    }
    Ok(())
}

fn storage(error: rusqlite::Error) -> BacklogStoreError {
    BacklogStoreError::Storage(error.to_string())
}

fn read_entry(
    tx: &Transaction<'_>,
    repository_key: &str,
    prd: &DiscoveredPrd,
) -> Result<BacklogEntry, BacklogStoreError> {
    let status: String = tx.query_row(
        "SELECT status FROM backlog_prds WHERE repository_key = ?1 AND prd_path = ?2 AND missing_since IS NULL",
        params![repository_key, prd.path.as_str()], |row| row.get(0),
    ).map_err(storage)?;
    Ok(BacklogEntry {
        prd: prd.clone(),
        status: BacklogStatus::parse(&status)?,
    })
}

impl BacklogStatusStore for SqliteBacklogRepository<'_> {
    fn reconcile_and_snapshot(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
    ) -> Result<Vec<BacklogEntry>, BacklogStoreError> {
        let tx = self.connection.transaction().map_err(storage)?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE backlog_prds SET missing_since = COALESCE(missing_since, ?2), updated_at = CASE WHEN missing_since IS NULL THEN ?2 ELSE updated_at END WHERE repository_key = ?1",
            params![repository.key, now],
        ).map_err(storage)?;
        for prd in discovered {
            reconcile_prd(&tx, repository, prd, &now)?;
        }
        let snapshot = discovered
            .iter()
            .map(|prd| read_entry(&tx, &repository.key, prd))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(storage)?;
        Ok(snapshot)
    }

    fn transition(
        &mut self,
        repository: &RepositoryIdentity,
        path: &RepositoryPath,
        expected: BacklogStatus,
        next: BacklogStatus,
        actor: &str,
    ) -> Result<BacklogEntry, BacklogStoreError> {
        if actor.trim().is_empty() {
            return Err(BacklogStoreError::EmptyActor);
        }
        let tx = self.connection.transaction().map_err(storage)?;
        let row: Option<(u64, Option<String>, String, String)> = tx.query_row(
            "SELECT prd_number, prd_suffix, content_hash, status FROM backlog_prds WHERE repository_key = ?1 AND prd_path = ?2",
            params![repository.key, path.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional().map_err(storage)?;
        let (number, suffix, content_hash, current_text) =
            row.ok_or_else(|| BacklogStoreError::NotFound(path.clone()))?;
        let current = BacklogStatus::parse(&current_text)?;
        if current != expected {
            return Err(BacklogStoreError::Conflict {
                path: path.clone(),
                expected: expected.as_str(),
                actual: current.as_str(),
            });
        }
        if current != next {
            let now = chrono::Utc::now().to_rfc3339();
            let changed = tx.execute(
                "UPDATE backlog_prds SET status = ?3, updated_at = ?4 WHERE repository_key = ?1 AND prd_path = ?2 AND status = ?5",
                params![repository.key, path.as_str(), next.as_str(), now, expected.as_str()],
            ).map_err(storage)?;
            if changed != 1 {
                return Err(BacklogStoreError::Conflict {
                    path: path.clone(),
                    expected: expected.as_str(),
                    actual: current.as_str(),
                });
            }
            tx.execute(
                "INSERT INTO backlog_status_events (repository_key, prd_path, old_status, new_status, actor, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![repository.key, path.as_str(), current.as_str(), next.as_str(), actor, now],
            ).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: DiscoveredPrd {
                id: PrdId::with_suffix(number, suffix.and_then(|s| s.chars().next())),
                number,
                path: path.clone(),
                location: PrdLocation::Active,
                title: String::new(),
                dependencies: Vec::new(),
                content_hash,
            },
            status: next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use familiar_ai_review::{
        AgentAssignment, AgentObservation, AgentRole, ExecutionUsage, IndependenceKind,
        ReviewerIndependence, VerificationStatus,
    };
    use std::collections::BTreeMap;

    fn prd() -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(9),
            number: 9,
            path: RepositoryPath::new("docs/prds/PRD-009.md").unwrap(),
            location: PrdLocation::Active,
            title: "Nine".into(),
            dependencies: vec![],
            content_hash: "abc".into(),
        }
    }

    #[test]
    fn suffixed_identities_round_trip_and_sql_order_matches_domain() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repository = RepositoryIdentity {
            worktree: std::path::PathBuf::from("/repo"),
            key: "repo".into(),
        };
        let identities = [
            (139, None),
            (140, None),
            (139, Some('d')),
            (139, Some('a')),
            (139, Some('b')),
        ];
        let discovered: Vec<_> = identities
            .into_iter()
            .map(|(number, suffix)| {
                let spelling = format!(
                    "{number:04}{}",
                    suffix.map(|c| c.to_string()).unwrap_or_default()
                );
                DiscoveredPrd {
                    id: PrdId::numbered_slug(number, suffix, 4),
                    number,
                    path: RepositoryPath::new(format!("todo/{spelling}-work.md")).unwrap(),
                    location: PrdLocation::Active,
                    title: spelling,
                    dependencies: vec![],
                    content_hash: format!("hash-{number}-{}", suffix.unwrap_or('_')),
                }
            })
            .collect();
        let snapshot = SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(&repository, &discovered)
            .unwrap();
        let mut domain: Vec<_> = snapshot.into_iter().map(|entry| entry.prd.id).collect();
        domain.sort();
        let sql: Vec<(u64, Option<String>)> = {
            let mut stmt = db.conn().prepare("SELECT prd_number,prd_suffix FROM backlog_prds WHERE repository_key='repo' ORDER BY prd_number, CASE WHEN prd_suffix IS NULL THEN 1 ELSE 0 END, prd_suffix").unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            sql.iter()
                .map(|(n, s)| PrdId::with_suffix(*n, s.as_deref().and_then(|v| v.chars().next())))
                .collect::<Vec<_>>(),
            domain
        );
        assert_eq!(
            domain.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "PRD 0139a",
                "PRD 0139b",
                "PRD 0139d",
                "PRD 0139",
                "PRD 0140"
            ]
        );
    }
    fn repo() -> RepositoryIdentity {
        RepositoryIdentity {
            worktree: "/tmp/work".into(),
            key: "/tmp/repo/.git".into(),
        }
    }

    fn cycle_with_evidence() -> ReviewCycle {
        let evidence = VerificationEvidence {
            check_id: "required".into(),
            argv: vec!["true".into()],
            working_directory: ".".into(),
            environment_identity: BTreeMap::new(),
            tool_identity: Some("true".into()),
            tested_identity: "candidate".into(),
            started_at: "2026-08-03T00:00:00Z".into(),
            ended_at: "2026-08-03T00:00:01Z".into(),
            duration_ms: 1,
            exit_code: Some(0),
            signal: None,
            status: VerificationStatus::Passed,
            required: true,
            summary: "passed".into(),
            stdout: None,
            stderr: None,
            truncated: false,
        };
        let observation = AgentObservation {
            assignment: AgentAssignment {
                adapter_id: "fake".into(),
                agent_id: "fake".into(),
                provider: Some("fake".into()),
                requested_model: None,
                role: AgentRole::Implementation,
                session_id: None,
            },
            agent_version: None,
            reported_model: None,
            unavailable_fields: BTreeMap::new(),
        };
        ReviewCycle {
            cycle_id: "cycle".into(),
            task_id: "task".into(),
            attempt: 1,
            state: ReviewCycleState::Completed,
            implementation: observation,
            implementation_execution: None,
            reviewer: None,
            independence: Some(ReviewerIndependence {
                kind: IndependenceKind::IndependentProviderOrModel,
                evidence: vec!["different provider".into()],
            }),
            review_request: None,
            review_result: None,
            remediation_request: None,
            remediation_result: None,
            verification_before_review: vec![evidence.clone()],
            verification_after_remediation: Vec::new(),
            verification_history: vec![evidence],
            scope_policy_snapshot: None,
            scope_evaluations: Vec::new(),
            aggregate_usage: ExecutionUsage::default(),
            aggregate_duration_ms: 1,
            started_at: "2026-08-03T00:00:00Z".into(),
            ended_at: Some("2026-08-03T00:00:01Z".into()),
            disposition: ReviewDisposition::ReadyForHumanApproval,
            stop_reasons: vec![ReviewStopReason::CleanReview],
            review_attempts: Vec::new(),
            remediation_attempts: Vec::new(),
        }
    }
    #[test]
    fn reconcile_preserves_status_and_transitions_are_checked() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Pending
        );
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "test",
            )
            .unwrap();
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Completed
        );
        assert!(matches!(
            storage.transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Blocked,
                "test"
            ),
            Err(BacklogStoreError::Conflict { .. })
        ));
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Completed,
                BacklogStatus::Completed,
                "test",
            )
            .unwrap();
        let events: i64 = storage
            .connection
            .query_row("SELECT count(*) FROM backlog_status_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(events, 1);
    }
    #[test]
    fn archived_location_corrects_status_with_system_event() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut archived = prd();
        archived.path = RepositoryPath::new("docs/prds/done/PRD-009.md").unwrap();
        archived.location = PrdLocation::Archived;
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage
            .connection
            .execute(
                "INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,created_at,updated_at) VALUES(?1,?2,?3,?4,'pending','before','before','before','before')",
                params![repo().key, archived.path.as_str(), archived.number, archived.content_hash],
            )
            .unwrap();
        let snapshot = storage
            .reconcile_and_snapshot(&repo(), &[archived])
            .unwrap();
        assert_eq!(snapshot[0].status, BacklogStatus::Completed);
        let event: (String, String, String) = storage
            .connection
            .query_row(
                "SELECT old_status,new_status,actor FROM backlog_status_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            event,
            (
                "pending".into(),
                "completed".into(),
                ARCHIVED_RECONCILIATION_ACTOR.into()
            )
        );
    }
    #[test]
    fn missing_and_reappearing_entry_retains_status() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Blocked,
                "test",
            )
            .unwrap();
        assert!(storage
            .reconcile_and_snapshot(&repo(), &[])
            .unwrap()
            .is_empty());
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Blocked
        );
    }

    #[test]
    fn transition_rejects_empty_actor_and_isolates_repositories() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        assert!(matches!(
            storage.transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "  "
            ),
            Err(BacklogStoreError::EmptyActor)
        ));
        let other = RepositoryIdentity {
            worktree: "/tmp/other".into(),
            key: "/tmp/other/.git".into(),
        };
        assert!(matches!(
            storage.transition(
                &other,
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "test"
            ),
            Err(BacklogStoreError::NotFound(_))
        ));
    }

    #[test]
    fn run_claim_is_atomic_and_owned_by_one_actor() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let actor = "system:familiar-ai-run:00001785772020811891-0000057947-000001";
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        let entry = storage.claim_run(&repo(), &[prd()], &prd(), actor).unwrap();
        assert_eq!(entry.status, BacklogStatus::InProgress);
        assert!(storage.claim_run(&repo(), &[prd()], &prd(), actor).is_err());
        let event: (String, String, String) = storage
            .connection
            .query_row(
                "SELECT old_status,new_status,actor FROM backlog_status_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            event,
            ("pending".into(), "in_progress".into(), actor.into())
        );
    }

    #[test]
    fn recovery_is_atomic_and_preserves_the_claim_event() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let claim_actor = "system:familiar-ai-run:00001785772020811891-0000057947-000001";
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        storage
            .claim_run(&repo(), &[prd()], &prd(), claim_actor)
            .unwrap();
        let recovered = storage
            .recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::Release,
                "ops:alice",
                "review was disabled",
            )
            .unwrap();
        assert_eq!(recovered.status, BacklogStatus::Pending);
        let events: Vec<(String, String, String)> = storage
            .connection
            .prepare(
                "SELECT old_status,new_status,actor FROM backlog_status_events ORDER BY event_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            events,
            vec![
                ("pending".into(), "in_progress".into(), claim_actor.into()),
                ("in_progress".into(), "pending".into(), "ops:alice".into())
            ]
        );
        let recovery: (String, String) = storage
            .connection
            .query_row(
                "SELECT action,reason FROM backlog_recovery_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(recovery, ("release".into(), "review was disabled".into()));
        assert!(storage
            .recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::Release,
                "ops:alice",
                "repeat",
            )
            .is_err());
    }

    #[test]
    fn manual_completion_requires_human_authority() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let claim_actor = "system:familiar-ai-run:00001785772020811891-0000057947-000001";
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        storage
            .claim_run(&repo(), &[prd()], &prd(), claim_actor)
            .unwrap();
        assert!(matches!(
            storage.recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::ManualCompleteOverride,
                claim_actor,
                "looks complete",
            ),
            Err(BacklogStoreError::HumanAuthorityRequired)
        ));
        let recovered = storage
            .recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::ManualCompleteOverride,
                "human:alice",
                "accepted outside normal review",
            )
            .unwrap();
        assert_eq!(recovered.status, BacklogStatus::Completed);
    }

    fn dependent() -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(10),
            number: 10,
            path: RepositoryPath::new("docs/prds/PRD-010.md").unwrap(),
            location: PrdLocation::Active,
            title: "Ten".into(),
            dependencies: vec![PrdId::new(9)],
            content_hash: "def".into(),
        }
    }

    #[test]
    fn record_complete_transitions_pending_to_completed_with_audit_rows() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        let recorded = storage
            .record_complete(
                &repo(),
                &[prd()],
                &prd(),
                "human:trollboy",
                "implemented, reviewed, and merged before this database existed",
            )
            .unwrap();
        assert_eq!(recorded.status, BacklogStatus::Completed);
        let event: (String, String, String) = storage
            .connection
            .query_row(
                "SELECT old_status,new_status,actor FROM backlog_status_events ORDER BY event_id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            event,
            (
                "pending".into(),
                "completed".into(),
                "human:trollboy".into()
            )
        );
        let recovery: (String, String) = storage
            .connection
            .query_row(
                "SELECT action,reason FROM backlog_recovery_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            recovery,
            (
                "recorded_complete".into(),
                "implemented, reviewed, and merged before this database existed".into()
            )
        );
    }

    #[test]
    fn record_complete_refuses_non_pending_statuses_and_unknown_entries() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let claim_actor = "system:familiar-ai-run:00001785772020811891-0000057947-000001";
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        storage
            .claim_run(&repo(), &[prd()], &prd(), claim_actor)
            .unwrap();
        assert!(matches!(
            storage.record_complete(&repo(), &[prd()], &prd(), "human:alice", "declared"),
            Err(BacklogStoreError::Conflict {
                expected: "pending",
                actual: "in_progress",
                ..
            })
        ));

        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::InProgress,
                BacklogStatus::Blocked,
                "ops",
            )
            .unwrap();
        assert!(matches!(
            storage.record_complete(&repo(), &[prd()], &prd(), "human:alice", "declared"),
            Err(BacklogStoreError::Conflict {
                expected: "pending",
                actual: "blocked",
                ..
            })
        ));

        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Blocked,
                BacklogStatus::Completed,
                "ops",
            )
            .unwrap();
        assert!(matches!(
            storage.record_complete(&repo(), &[prd()], &prd(), "human:alice", "declared"),
            Err(BacklogStoreError::Conflict {
                expected: "pending",
                actual: "completed",
                ..
            })
        ));

        let unknown = DiscoveredPrd {
            id: PrdId::new(99),
            number: 99,
            path: RepositoryPath::new("docs/prds/PRD-099.md").unwrap(),
            location: PrdLocation::Active,
            title: "Unknown".into(),
            dependencies: vec![],
            content_hash: "zzz".into(),
        };
        assert!(matches!(
            storage.record_complete(
                &repo(),
                &[prd(), unknown.clone()],
                &unknown,
                "human:alice",
                "declared"
            ),
            Err(BacklogStoreError::NotFound(_))
        ));
    }

    #[test]
    fn record_complete_refuses_incomplete_dependencies_then_succeeds_in_dependency_order() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        let discovered = [prd(), dependent()];
        storage
            .reconcile_and_snapshot(&repo(), &discovered)
            .unwrap();

        let refused = storage
            .record_complete(
                &repo(),
                &discovered,
                &dependent(),
                "human:alice",
                "reversed order",
            )
            .unwrap_err();
        match refused {
            BacklogStoreError::IncompleteDependencies { dependencies, .. } => {
                assert_eq!(dependencies, "PRD-9");
            }
            other => panic!("expected IncompleteDependencies, got {other:?}"),
        }
        let status: String = storage
            .connection
            .query_row(
                "SELECT status FROM backlog_prds WHERE prd_path=?1",
                [dependent().path.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
        let events: i64 = storage
            .connection
            .query_row(
                "SELECT count(*) FROM backlog_status_events WHERE prd_path=?1",
                [dependent().path.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 0);

        storage
            .record_complete(
                &repo(),
                &discovered,
                &prd(),
                "human:alice",
                "dependency merged earlier",
            )
            .unwrap();
        let completed = storage
            .record_complete(
                &repo(),
                &discovered,
                &dependent(),
                "human:alice",
                "dependent merged after",
            )
            .unwrap();
        assert_eq!(completed.status, BacklogStatus::Completed);
    }

    #[test]
    fn record_complete_requires_human_actor_and_non_empty_reason_without_writes() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        assert!(matches!(
            storage.record_complete(&repo(), &[prd()], &prd(), "ops:alice", "declared complete"),
            Err(BacklogStoreError::HumanAuthorityRequired)
        ));
        assert!(matches!(
            storage.record_complete(&repo(), &[prd()], &prd(), "human:alice", "   "),
            Err(BacklogStoreError::InvalidRecoveryReason)
        ));
        let status: String = storage
            .connection
            .query_row(
                "SELECT status FROM backlog_prds WHERE prd_path=?1",
                [prd().path.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn recovery_rejects_wrong_status_and_corrupt_claim_lineage_without_writes() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let claim_actor = "system:familiar-ai-run:00001785772020811891-0000057947-000001";
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        let wrong_status = storage
            .recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::Release,
                "ops:alice",
                "retry",
            )
            .unwrap_err();
        assert!(matches!(wrong_status, BacklogStoreError::Conflict { .. }));
        storage
            .claim_run(&repo(), &[prd()], &prd(), claim_actor)
            .unwrap();
        storage
            .connection
            .execute(
                "UPDATE backlog_status_events SET old_status='completed'",
                [],
            )
            .unwrap();
        let corrupt = storage
            .recover(
                &repo(),
                &prd(),
                BacklogRecoveryAction::Release,
                "ops:alice",
                "retry",
            )
            .unwrap_err();
        assert!(matches!(
            corrupt,
            BacklogStoreError::RecoveryAuditCorrupt(_)
        ));
        let state: (String, i64, i64) = storage
            .connection
            .query_row(
                "SELECT status,(SELECT count(*) FROM backlog_status_events),\
                 (SELECT count(*) FROM backlog_recovery_events) FROM backlog_prds",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, ("in_progress".into(), 1, 0));
    }

    #[test]
    fn completion_evidence_validation_fails_closed_on_normalized_row_corruption() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let cycle = cycle_with_evidence();
        db.conn().execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let tx = db.conn_mut().transaction().unwrap();
        tx.execute(
            "INSERT INTO review_verification_evidence(cycle_id,check_id,phase,evidence_json) \
             VALUES(?1,?2,'attempt-0',?3)",
            params![
                cycle.cycle_id,
                cycle.verification_history[0].check_id,
                serde_json::to_string(&cycle.verification_history[0]).unwrap()
            ],
        )
        .unwrap();
        assert!(validate_persisted_evidence(&tx, &cycle).is_ok());
        tx.execute(
            "UPDATE review_verification_evidence SET evidence_json='{}' WHERE cycle_id=?1",
            [&cycle.cycle_id],
        )
        .unwrap();
        assert!(validate_persisted_evidence(&tx, &cycle).is_err());
    }

    #[test]
    fn completion_rejects_a_terminal_cycle_without_review_request_evidence() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let cycle = cycle_with_evidence();
        let tx = db.conn_mut().transaction().unwrap();

        assert!(validate_persisted_review_request(&tx, &cycle).is_err());
    }

    #[test]
    fn completion_finding_validation_rejects_unrepresented_persisted_findings() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let cycle = cycle_with_evidence();
        db.conn().execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let tx = db.conn_mut().transaction().unwrap();
        tx.execute(
            "INSERT INTO review_findings(finding_id,cycle_id,category,severity,blocking,status,finding_json) \
             VALUES('unexpected',?1,'correctness_defect','high',1,'open','{}')",
            [&cycle.cycle_id],
        )
        .unwrap();
        assert!(validate_persisted_findings(&tx, &cycle).is_err());
    }
}
