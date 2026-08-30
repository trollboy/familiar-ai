use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionCheckpoint {
    pub checkpoint_id: String,
    pub repository_key: String,
    pub prd_id: String,
    pub prd_path: String,
    pub execution_id: Option<String>,
    pub phase: String,
    pub base_revision: String,
    pub worktree_path: String,
    pub branch_name: Option<String>,
    pub diff_hash: String,
    pub changed_files_json: String,
    pub agent_identity: String,
    pub usage_json: String,
    pub test_evidence_json: String,
    pub invalid_reason: Option<String>,
}

pub struct CheckpointRepository<'a> {
    conn: &'a Connection,
}

impl<'a> CheckpointRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn schema_available(&self) -> familiar_ai_core::Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='execution_checkpoints')",
                [],
                |row| row.get(0),
            )
            .map_err(db)
    }

    pub fn put(&self, value: &ExecutionCheckpoint) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        let transaction = self.conn.unchecked_transaction().map_err(db)?;
        transaction.execute(
            "INSERT INTO execution_checkpoints(checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16) ON CONFLICT(repository_key,prd_id) DO UPDATE SET execution_id=excluded.execution_id,phase=excluded.phase,base_revision=excluded.base_revision,worktree_path=excluded.worktree_path,branch_name=excluded.branch_name,diff_hash=excluded.diff_hash,changed_files_json=excluded.changed_files_json,agent_identity=excluded.agent_identity,usage_json=excluded.usage_json,test_evidence_json=excluded.test_evidence_json,invalid_reason=excluded.invalid_reason,updated_at=excluded.updated_at",
            params![value.checkpoint_id,value.repository_key,value.prd_id,value.prd_path,value.execution_id,value.phase,value.base_revision,value.worktree_path,value.branch_name,value.diff_hash,value.changed_files_json,value.agent_identity,value.usage_json,value.test_evidence_json,value.invalid_reason,now]
        ).map_err(db)?;
        transaction.execute(
            "INSERT OR IGNORE INTO execution_checkpoint_events(event_id,checkpoint_id,event_type,prior_phase,resulting_phase,detail,recorded_at) VALUES(?1,?2,'checkpoint_created',NULL,?3,'implementation_checkpoint',?4)",
            params![format!("{}:created", value.checkpoint_id), value.checkpoint_id, value.phase, now],
        ).map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(())
    }

    pub fn get(
        &self,
        repository: &str,
        prd: &str,
    ) -> familiar_ai_core::Result<Option<ExecutionCheckpoint>> {
        self.conn.query_row("SELECT checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason FROM execution_checkpoints WHERE repository_key=?1 AND prd_id=?2", params![repository,prd], map).optional().map_err(db)
    }

    pub fn resumable(
        &self,
        repository: &str,
    ) -> familiar_ai_core::Result<Vec<ExecutionCheckpoint>> {
        let mut stmt=self.conn.prepare("SELECT checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason FROM execution_checkpoints WHERE repository_key=?1 AND phase NOT IN ('completed','integrated') ORDER BY prd_id,checkpoint_id").map_err(db)?;
        let result = stmt
            .query_map([repository], map)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db);
        result
    }

    pub fn all(&self, repository: &str) -> familiar_ai_core::Result<Vec<ExecutionCheckpoint>> {
        let mut stmt=self.conn.prepare("SELECT checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason FROM execution_checkpoints WHERE repository_key=?1 ORDER BY prd_id,checkpoint_id").map_err(db)?;
        let rows = stmt
            .query_map([repository], map)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    /// Deterministic, repository-scoped, cursor-paginated listing of
    /// checkpoints (worktree/branch identity plus recovery phase), ordered
    /// by `prd_id`. `after` is the last delivered `prd_id` (exclusive).
    pub fn page(
        &self,
        repository: &str,
        after: Option<&str>,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<ExecutionCheckpoint>> {
        let mut stmt = self.conn.prepare("SELECT checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason FROM execution_checkpoints WHERE repository_key=?1 AND prd_id>?2 ORDER BY prd_id,checkpoint_id LIMIT ?3").map_err(db)?;
        let rows = stmt
            .query_map(params![repository, after.unwrap_or(""), limit as i64], map)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn set_phase(&self, checkpoint_id: &str, phase: &str) -> familiar_ai_core::Result<()> {
        self.transition(checkpoint_id, phase, "phase_completed")
    }

    pub fn transition(
        &self,
        checkpoint_id: &str,
        phase: &str,
        detail: &str,
    ) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        let transaction = self.conn.unchecked_transaction().map_err(db)?;
        let prior: Option<String> = transaction
            .query_row(
                "SELECT phase FROM execution_checkpoints WHERE checkpoint_id=?1",
                [checkpoint_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        let Some(prior) = prior else {
            return Err(FamiliarError::Database(format!(
                "checkpoint {checkpoint_id} not found"
            )));
        };
        if prior == phase {
            return Ok(());
        }
        let changed = transaction.execute(
            "UPDATE execution_checkpoints SET phase=?1,invalid_reason=NULL,updated_at=?2 WHERE checkpoint_id=?3 AND phase=?4",
            params![phase, now, checkpoint_id, prior],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "checkpoint {checkpoint_id} changed during transition"
            )));
        }
        transaction.execute(
            "INSERT INTO execution_checkpoint_events(event_id,checkpoint_id,event_type,prior_phase,resulting_phase,detail,recorded_at) VALUES(?1,?2,'phase_transition',?3,?4,?5,?6)",
            params![format!("{checkpoint_id}:{phase}"), checkpoint_id, prior, phase, detail, now],
        ).map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(())
    }

    pub fn events(&self, checkpoint_id: &str) -> familiar_ai_core::Result<Vec<(String, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT resulting_phase,detail FROM execution_checkpoint_events WHERE checkpoint_id=?1 ORDER BY recorded_at,event_id",
        ).map_err(db)?;
        let events = statement
            .query_map([checkpoint_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(events)
    }
}

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionCheckpoint> {
    Ok(ExecutionCheckpoint {
        checkpoint_id: row.get(0)?,
        repository_key: row.get(1)?,
        prd_id: row.get(2)?,
        prd_path: row.get(3)?,
        execution_id: row.get(4)?,
        phase: row.get(5)?,
        base_revision: row.get(6)?,
        worktree_path: row.get(7)?,
        branch_name: row.get(8)?,
        diff_hash: row.get(9)?,
        changed_files_json: row.get(10)?,
        agent_identity: row.get(11)?,
        usage_json: row.get(12)?,
        test_evidence_json: row.get(13)?,
        invalid_reason: row.get(14)?,
    })
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn checkpoint(repository_key: &str, prd_id: &str) -> ExecutionCheckpoint {
        ExecutionCheckpoint {
            checkpoint_id: format!("{repository_key}:{prd_id}"),
            repository_key: repository_key.into(),
            prd_id: prd_id.into(),
            prd_path: format!("docs/prds/{prd_id}.md"),
            execution_id: Some(format!("exec-{prd_id}")),
            phase: "implemented".into(),
            base_revision: "deadbeef".into(),
            worktree_path: format!("/state/worktrees/{prd_id}"),
            branch_name: Some(format!("familiar/{prd_id}")),
            diff_hash: "sha256:abc".into(),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        }
    }

    #[test]
    fn page_is_repository_scoped_ordered_and_paginated() {
        let db = database();
        let repository = CheckpointRepository::new(db.conn());
        repository.put(&checkpoint("/repo/.git", "PRD-1")).unwrap();
        repository.put(&checkpoint("/repo/.git", "PRD-2")).unwrap();
        repository.put(&checkpoint("/repo/.git", "PRD-3")).unwrap();
        // A checkpoint from a different repository must never leak into a
        // repository-scoped listing.
        repository.put(&checkpoint("/other/.git", "PRD-1")).unwrap();

        let all = repository.page("/repo/.git", None, 10).unwrap();
        assert_eq!(
            all.iter().map(|c| c.prd_id.as_str()).collect::<Vec<_>>(),
            ["PRD-1", "PRD-2", "PRD-3"]
        );
        assert_eq!(all, repository.all("/repo/.git").unwrap());

        let first_page = repository.page("/repo/.git", None, 1).unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].prd_id, "PRD-1");

        let rest = repository
            .page("/repo/.git", Some(&first_page[0].prd_id), 10)
            .unwrap();
        assert_eq!(
            rest.iter().map(|c| c.prd_id.as_str()).collect::<Vec<_>>(),
            ["PRD-2", "PRD-3"]
        );
    }
}
