use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection};

pub struct WorkerSelectionRepository<'a> {
    conn: &'a Connection,
}
impl<'a> WorkerSelectionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    pub fn record(
        &self,
        selection_id: &str,
        execution_id: Option<&str>,
        stage: &str,
        rule: &str,
        selected_identity: &str,
        candidates_json: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT INTO worker_selections(selection_id,execution_id,stage,rule,selected_identity,candidates_json,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![selection_id,execution_id,stage,rule,selected_identity,candidates_json,Utc::now().to_rfc3339()]).map_err(|e| FamiliarError::Database(format!("worker selection write failed: {e}")))?;
        Ok(())
    }
}
