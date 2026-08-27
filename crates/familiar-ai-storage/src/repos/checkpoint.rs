use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub fn put(&self, value: &ExecutionCheckpoint) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO execution_checkpoints(checkpoint_id,repository_key,prd_id,prd_path,execution_id,phase,base_revision,worktree_path,branch_name,diff_hash,changed_files_json,agent_identity,usage_json,test_evidence_json,invalid_reason,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16) ON CONFLICT(repository_key,prd_id) DO UPDATE SET execution_id=excluded.execution_id,phase=excluded.phase,base_revision=excluded.base_revision,worktree_path=excluded.worktree_path,branch_name=excluded.branch_name,diff_hash=excluded.diff_hash,changed_files_json=excluded.changed_files_json,agent_identity=excluded.agent_identity,usage_json=excluded.usage_json,test_evidence_json=excluded.test_evidence_json,invalid_reason=excluded.invalid_reason,updated_at=excluded.updated_at",
            params![value.checkpoint_id,value.repository_key,value.prd_id,value.prd_path,value.execution_id,value.phase,value.base_revision,value.worktree_path,value.branch_name,value.diff_hash,value.changed_files_json,value.agent_identity,value.usage_json,value.test_evidence_json,value.invalid_reason,now]
        ).map_err(db)?;
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

    pub fn set_phase(&self, checkpoint_id: &str, phase: &str) -> familiar_ai_core::Result<()> {
        let changed = self.conn.execute(
            "UPDATE execution_checkpoints SET phase=?1,invalid_reason=NULL,updated_at=?2 WHERE checkpoint_id=?3",
            params![phase, Utc::now().to_rfc3339(), checkpoint_id],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "checkpoint {checkpoint_id} not found"
            )));
        }
        Ok(())
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
