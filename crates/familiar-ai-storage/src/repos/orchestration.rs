use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MigrationReservation {
    pub repository_key: String,
    pub version: u64,
    pub session_id: String,
    pub prd_id: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScopeDecision {
    pub finding_hash: String,
    pub checkpoint_id: String,
    pub prd_id: String,
    pub candidate_hash: String,
    pub finding_json: String,
}

pub struct OrchestrationRepository<'a> {
    conn: &'a Connection,
}

impl<'a> OrchestrationRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn initialize_integration(
        &self,
        session: &str,
        revision: &str,
    ) -> familiar_ai_core::Result<()> {
        let changed = self.conn.execute(
            "UPDATE driver_sessions SET base_revision=COALESCE(base_revision,?1),integration_revision=COALESCE(integration_revision,?1) WHERE session_id=?2",
            params![revision, session],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "driver session {session} not found"
            )));
        }
        Ok(())
    }

    pub fn integration_revision(&self, session: &str) -> familiar_ai_core::Result<String> {
        self.conn
            .query_row(
                "SELECT integration_revision FROM driver_sessions WHERE session_id=?1",
                [session],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(db)?
            .flatten()
            .ok_or_else(|| {
                FamiliarError::Database(format!("session {session} has no integration revision"))
            })
    }

    /// Advance only from the revision the merger actually used. This CAS is
    /// the durable serialization point for review-completion-order landing.
    pub fn advance_integration(
        &self,
        session: &str,
        expected: &str,
        candidate: &str,
        sequence: i64,
    ) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        let changed = tx.execute(
            "UPDATE driver_sessions SET integration_revision=?1 WHERE session_id=?2 AND integration_revision=?3 AND ended_at IS NULL",
            params![candidate, session, expected],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(
                "integration revision changed during landing".into(),
            ));
        }
        let changed = tx.execute(
            "UPDATE driver_attempts SET candidate_revision=?1,integrated_at=?2,last_durable_phase='integrated' WHERE session_id=?3 AND sequence=?4 AND integrated_at IS NULL",
            params![candidate, now, session, sequence],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(
                "attempt was already integrated or is missing".into(),
            ));
        }
        tx.commit().map_err(db)
    }

    pub fn reserve_migration(
        &self,
        repository: &str,
        session: &str,
        prd: &str,
        first_available: u64,
    ) -> familiar_ai_core::Result<MigrationReservation> {
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        if let Some(existing) = tx.query_row(
            "SELECT repository_key,version,session_id,prd_id,state FROM migration_version_reservations WHERE session_id=?1 AND prd_id=?2",
            params![session, prd], map_reservation,
        ).optional().map_err(db)? {
            if existing.state == "reserved" {
                tx.commit().map_err(db)?;
                return Ok(existing);
            }
            if existing.state == "released" {
                let changed = tx.execute(
                    "UPDATE migration_version_reservations SET state='reserved',reserved_at=?1,resolved_at=NULL WHERE session_id=?2 AND prd_id=?3 AND state='released'",
                    params![Utc::now().to_rfc3339(), session, prd],
                ).map_err(db)?;
                if changed != 1 {
                    return Err(FamiliarError::Database(format!(
                        "migration reservation for {prd} changed during retry"
                    )));
                }
                tx.commit().map_err(db)?;
                return Ok(MigrationReservation { state: "reserved".into(), ..existing });
            }
            return Err(FamiliarError::Database(format!(
                "migration reservation for {prd} was already consumed"
            )));
        }
        let reserved: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version),0) FROM migration_version_reservations WHERE repository_key=?1",
            [repository], |r| r.get(0),
        ).map_err(db)?;
        let version = first_available.max((reserved as u64).saturating_add(1));
        tx.execute(
            "INSERT INTO migration_version_reservations(repository_key,version,session_id,prd_id,state,reserved_at) VALUES(?1,?2,?3,?4,'reserved',?5)",
            params![repository, version as i64, session, prd, Utc::now().to_rfc3339()],
        ).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(MigrationReservation {
            repository_key: repository.into(),
            version,
            session_id: session.into(),
            prd_id: prd.into(),
            state: "reserved".into(),
        })
    }

    pub fn resolve_migration(
        &self,
        session: &str,
        prd: &str,
        consume: bool,
    ) -> familiar_ai_core::Result<()> {
        let state = if consume { "consumed" } else { "released" };
        let changed = self.conn.execute(
            "UPDATE migration_version_reservations SET state=?1,resolved_at=?2 WHERE session_id=?3 AND prd_id=?4 AND state='reserved'",
            params![state, Utc::now().to_rfc3339(), session, prd],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "migration reservation for {prd} is missing or already resolved"
            )));
        }
        Ok(())
    }

    pub fn reservation(
        &self,
        session: &str,
        prd: &str,
    ) -> familiar_ai_core::Result<Option<MigrationReservation>> {
        self.conn.query_row(
            "SELECT repository_key,version,session_id,prd_id,state FROM migration_version_reservations WHERE session_id=?1 AND prd_id=?2",
            params![session, prd], map_reservation,
        ).optional().map_err(db)
    }

    pub fn pending_scope_decisions(
        &self,
        repository: &str,
    ) -> familiar_ai_core::Result<Vec<ScopeDecision>> {
        let mut stmt = self.conn.prepare("SELECT finding_hash,checkpoint_id,prd_id,candidate_hash,finding_json FROM scope_decisions WHERE repository_key=?1 AND decision IS NULL ORDER BY prd_id,finding_hash").map_err(db)?;
        let rows = stmt
            .query_map([repository], |r| {
                Ok(ScopeDecision {
                    finding_hash: r.get(0)?,
                    checkpoint_id: r.get(1)?,
                    prd_id: r.get(2)?,
                    candidate_hash: r.get(3)?,
                    finding_json: r.get(4)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn record_scope_finding(
        &self,
        repository: &str,
        checkpoint: &str,
        prd: &str,
        candidate_hash: &str,
        finding_hash: &str,
        finding_json: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO scope_decisions(finding_hash,repository_key,checkpoint_id,prd_id,candidate_hash,finding_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![finding_hash,repository,checkpoint,prd,candidate_hash,finding_json,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn decide_scope(
        &self,
        repository: &str,
        finding_hash: &str,
        candidate_hash: &str,
        approve: bool,
        actor: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<String> {
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        let checkpoint: Option<String> = tx.query_row(
            "SELECT checkpoint_id FROM scope_decisions WHERE repository_key=?1 AND finding_hash=?2 AND candidate_hash=?3 AND decision IS NULL",
            params![repository,finding_hash,candidate_hash], |r| r.get(0),
        ).optional().map_err(db)?;
        let Some(checkpoint) = checkpoint else {
            return Err(FamiliarError::Database(
                "pending scope finding/candidate binding not found".into(),
            ));
        };
        let decision = if approve { "approved" } else { "rejected" };
        tx.execute("UPDATE scope_decisions SET decision=?1,actor=?2,reason=?3,decided_at=?4 WHERE finding_hash=?5 AND decision IS NULL", params![decision,actor,reason,Utc::now().to_rfc3339(),finding_hash]).map_err(db)?;
        let pending: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM scope_decisions WHERE checkpoint_id=?1 AND candidate_hash=?2 AND decision IS NULL",
                params![checkpoint, candidate_hash],
                |row| row.get(0),
            )
            .map_err(db)?;
        let rejected: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM scope_decisions WHERE checkpoint_id=?1 AND candidate_hash=?2 AND decision='rejected'",
                params![checkpoint, candidate_hash],
                |row| row.get(0),
            )
            .map_err(db)?;
        if !approve || pending == 0 {
            // Scope pauses precede independent review, so a fully approved
            // candidate goes back to `implemented` — verification and review
            // are still owed and resume re-enters from there. Stamping
            // `reviewed` here both wedged resume ("reviewed cannot start
            // review") and let the completion path fire on an unreviewed
            // cycle (FAM-BUG-041).
            let phase = if approve && rejected == 0 {
                "implemented"
            } else {
                "blocked"
            };
            tx.execute(
                "UPDATE execution_checkpoints SET phase=?1,updated_at=?2 WHERE checkpoint_id=?3",
                params![phase, Utc::now().to_rfc3339(), checkpoint],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(checkpoint)
    }

    /// Content hashes of scope findings a human has durably approved for
    /// this repository (PRD-080). The review coordinator absorbs matching
    /// human-review findings instead of pausing again (FAM-BUG-041 flow).
    pub fn approved_scope_findings(
        &self,
        repository: &str,
    ) -> familiar_ai_core::Result<std::collections::BTreeSet<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT finding_json FROM scope_decisions WHERE repository_key=?1 AND decision='approved'",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map([repository], |row| row.get::<_, String>(0))
            .map_err(db)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(db)?;
        // Approvals are keyed by decision substance: the recorded finding
        // with its policy snapshot hash normalized out, so unrelated
        // landings that rotate the compiled policy hash cannot orphan a
        // human decision (PRD-080).
        Ok(rows
            .iter()
            .filter_map(|json| serde_json::from_str::<familiar_ai_review::ScopeFinding>(json).ok())
            .map(|finding| familiar_ai_review::scope_finding_substance_hash(&finding))
            .collect())
    }

    /// One snapshot of terminal PRDs from both authorities. Recovery callers
    /// use this set for the entire inventory they print.
    pub fn terminal_prds(
        &self,
        repository: &str,
    ) -> familiar_ai_core::Result<std::collections::BTreeSet<String>> {
        let mut terminal = std::collections::BTreeSet::new();
        let mut backlog = self
            .conn
            .prepare(
                "SELECT prd_path FROM backlog_prds WHERE repository_key=?1 AND status='completed'",
            )
            .map_err(db)?;
        for path in backlog
            .query_map([repository], |r| r.get::<_, String>(0))
            .map_err(db)?
        {
            let path = path.map_err(db)?;
            if let Some(id) = std::path::Path::new(&path)
                .file_stem()
                .and_then(|v| v.to_str())
            {
                terminal.insert(id.to_owned());
                // FAM-BUG-016 root cause: file stems are zero-padded
                // ("PRD-048") while checkpoint/attempt prd_ids use the
                // canonical rendering ("PRD-48"). Insert the canonical
                // spelling too so the filter actually matches.
                if let Some(rest) = id.strip_prefix("PRD-") {
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    let suffix: String = rest.chars().skip_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(number) = digits.parse::<u64>() {
                        terminal.insert(format!("PRD-{number}{suffix}"));
                    }
                }
            }
        }
        let mut integrated = self.conn.prepare("SELECT a.prd_id FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id WHERE s.repository_key=?1 AND a.integrated_at IS NOT NULL").map_err(db)?;
        for id in integrated
            .query_map([repository], |r| r.get::<_, String>(0))
            .map_err(db)?
        {
            terminal.insert(id.map_err(db)?);
        }
        Ok(terminal)
    }
}

fn map_reservation(r: &rusqlite::Row<'_>) -> rusqlite::Result<MigrationReservation> {
    Ok(MigrationReservation {
        repository_key: r.get(0)?,
        version: r.get::<_, i64>(1)? as u64,
        session_id: r.get(2)?,
        prd_id: r.get(3)?,
        state: r.get(4)?,
    })
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_are_distinct_persistent_and_resolve_once() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        crate::DriverRepository::new(db.conn())
            .open_session("s", "repo", "{}")
            .unwrap();
        let repo = OrchestrationRepository::new(db.conn());
        let a = repo.reserve_migration("repo", "s", "PRD-1", 32).unwrap();
        let b = repo.reserve_migration("repo", "s", "PRD-2", 32).unwrap();
        assert_eq!((a.version, b.version), (32, 33));
        assert_eq!(repo.reserve_migration("repo", "s", "PRD-1", 99).unwrap(), a);
        repo.resolve_migration("s", "PRD-1", true).unwrap();
        assert!(repo.resolve_migration("s", "PRD-1", false).is_err());
        assert_eq!(
            repo.reservation("s", "PRD-1").unwrap().unwrap().state,
            "consumed"
        );
    }

    #[test]
    fn released_reservation_is_reactivated_for_retry_then_consumed_once() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        crate::DriverRepository::new(db.conn())
            .open_session("s", "repo", "{}")
            .unwrap();
        let repo = OrchestrationRepository::new(db.conn());
        let first = repo.reserve_migration("repo", "s", "PRD-1", 32).unwrap();
        repo.resolve_migration("s", "PRD-1", false).unwrap();

        let retry = repo.reserve_migration("repo", "s", "PRD-1", 99).unwrap();
        assert_eq!(retry.version, first.version);
        assert_eq!(retry.state, "reserved");
        repo.resolve_migration("s", "PRD-1", true).unwrap();
        assert!(repo.resolve_migration("s", "PRD-1", true).is_err());
        assert_eq!(
            repo.reservation("s", "PRD-1").unwrap().unwrap().state,
            "consumed"
        );
    }

    #[test]
    fn integration_revision_advances_by_compare_and_swap() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let driver = crate::DriverRepository::new(db.conn());
        driver.open_session("s", "repo", "{}").unwrap();
        let seq = driver
            .record_attempt_started("s", "PRD-1", "docs/prds/PRD-1.md", None)
            .unwrap();
        let repo = OrchestrationRepository::new(db.conn());
        repo.initialize_integration("s", "base").unwrap();
        repo.advance_integration("s", "base", "merge", seq).unwrap();
        assert_eq!(repo.integration_revision("s").unwrap(), "merge");
        assert!(repo.advance_integration("s", "base", "other", seq).is_err());
    }

    #[test]
    fn scope_checkpoint_remains_gated_until_every_finding_is_approved() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db.conn().execute(
            "INSERT INTO execution_checkpoints(checkpoint_id,repository_key,prd_id,prd_path,phase,base_revision,worktree_path,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,created_at,updated_at) VALUES('cp','repo','PRD-1','docs/prds/PRD-1.md','implemented_pending_review','base','/tmp/worktree','candidate','[]','agent','{}','[]','now','now')",
            [],
        ).unwrap();
        let repo = OrchestrationRepository::new(db.conn());
        repo.record_scope_finding("repo", "cp", "PRD-1", "candidate", "finding-a", "{}")
            .unwrap();
        repo.record_scope_finding("repo", "cp", "PRD-1", "candidate", "finding-b", "{}")
            .unwrap();

        repo.decide_scope("repo", "finding-a", "candidate", true, "reviewer", "ok")
            .unwrap();
        let phase: String = db
            .conn()
            .query_row(
                "SELECT phase FROM execution_checkpoints WHERE checkpoint_id='cp'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, "implemented_pending_review");

        repo.decide_scope("repo", "finding-b", "candidate", true, "reviewer", "ok")
            .unwrap();
        let phase: String = db
            .conn()
            .query_row(
                "SELECT phase FROM execution_checkpoints WHERE checkpoint_id='cp'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // FAM-BUG-041: full approval re-opens the pipeline at `implemented`
        // (review is still owed); it must never stamp `reviewed`.
        assert_eq!(phase, "implemented");
    }
}
