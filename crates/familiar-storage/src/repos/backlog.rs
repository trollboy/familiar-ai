use familiar_core::{
    BacklogEntry, BacklogStatus, BacklogStatusStore, BacklogStoreError, DiscoveredPrd, PrdId,
    RepositoryIdentity, RepositoryPath,
};
use familiar_review::{
    FindingStatus, ReviewCycle, ReviewCycleState, ReviewDisposition, ReviewStopReason, ReviewTask,
    VerificationStatus,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub struct SqliteBacklogRepository<'a> {
    connection: &'a mut Connection,
}

impl<'a> SqliteBacklogRepository<'a> {
    pub fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
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
        familiar_core::admit_run_prd(&entries, target)
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
        let persisted_evidence: i64 = tx
            .query_row(
                "SELECT count(*) FROM review_verification_evidence WHERE cycle_id=?1",
                [&cycle.cycle_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if persisted_evidence
            != i64::try_from(cycle.verification_history.len())
                .map_err(|_| BacklogStoreError::Storage("verification evidence overflow".into()))?
        {
            return Err(BacklogStoreError::Storage(
                "persisted verification evidence is incomplete".into(),
            ));
        }
        let persisted_findings: i64 = tx
            .query_row(
                "SELECT count(*) FROM review_findings WHERE cycle_id=?1",
                [&cycle.cycle_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        let expected_findings = cycle.review_result.as_ref().map_or(0, |r| r.findings.len());
        if persisted_findings
            != i64::try_from(expected_findings)
                .map_err(|_| BacklogStoreError::Storage("review finding count overflow".into()))?
        {
            return Err(BacklogStoreError::Storage(
                "persisted review findings are incomplete".into(),
            ));
        }
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

fn validate_run_actor(actor: &str) -> Result<(), BacklogStoreError> {
    let id = actor
        .strip_prefix("system:familiar-run:")
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
        tx.execute("INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,missing_since,created_at,updated_at)VALUES(?1,?2,?3,?4,'pending',?5,?5,NULL,?5,?5) ON CONFLICT(repository_key,prd_path) DO UPDATE SET prd_number=excluded.prd_number,content_hash=excluded.content_hash,last_seen_at=excluded.last_seen_at,missing_since=NULL",params![repository.key,prd.path.as_str(),prd.number.to_string(),prd.content_hash,now]).map_err(storage)?;
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
            tx.execute(
                "INSERT INTO backlog_prds (repository_key, prd_path, prd_number, content_hash, status, discovered_at, last_seen_at, missing_since, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5, NULL, ?5, ?5)
                 ON CONFLICT(repository_key, prd_path) DO UPDATE SET
                    prd_number = excluded.prd_number,
                    content_hash = excluded.content_hash,
                    last_seen_at = excluded.last_seen_at,
                    missing_since = NULL",
                params![repository.key, prd.path.as_str(), prd.number.to_string(), prd.content_hash, now],
            ).map_err(storage)?;
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
        let row: Option<(u64, String, String)> = tx.query_row(
            "SELECT prd_number, content_hash, status FROM backlog_prds WHERE repository_key = ?1 AND prd_path = ?2",
            params![repository.key, path.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(storage)?;
        let (number, content_hash, current_text) =
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
                id: PrdId::new(number),
                number,
                path: path.clone(),
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

    fn prd() -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(9),
            number: 9,
            path: RepositoryPath::new("docs/prds/PRD-009.md").unwrap(),
            title: "Nine".into(),
            dependencies: vec![],
            content_hash: "abc".into(),
        }
    }
    fn repo() -> RepositoryIdentity {
        RepositoryIdentity {
            worktree: "/tmp/work".into(),
            key: "/tmp/repo/.git".into(),
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
        let actor = "system:familiar-run:00001785772020811891-0000057947-000001";
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
}
