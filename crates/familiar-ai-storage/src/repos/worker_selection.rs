use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection};

pub struct WorkerSelectionRepository<'a> {
    conn: &'a Connection,
}

pub struct WorkerSelectionRecord<'a> {
    pub selection_id: &'a str,
    pub execution_id: Option<&'a str>,
    pub stage: &'a str,
    pub rule: &'a str,
    pub selected_identity: &'a str,
    pub selected_empirical_version: &'a str,
    pub candidates_json: &'a str,
    pub risk_classes_json: &'a str,
    pub expected_file_count: u64,
}

impl<'a> WorkerSelectionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    pub fn record(&self, record: &WorkerSelectionRecord<'_>) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT INTO worker_selections(selection_id,execution_id,stage,rule,selected_identity,candidates_json,risk_classes_json,expected_file_count,recorded_at,selected_spec_identity,selected_empirical_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?5,?10)", params![record.selection_id,record.execution_id,record.stage,record.rule,record.selected_identity,record.candidates_json,record.risk_classes_json,record.expected_file_count,Utc::now().to_rfc3339(),record.selected_empirical_version]).map_err(|e| FamiliarError::Database(format!("worker selection write failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_routing_inputs_for_recovery() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        WorkerSelectionRepository::new(db.conn())
            .record(&WorkerSelectionRecord {
                selection_id: "selection-1",
                execution_id: Some("execution-1"),
                stage: "implementation",
                rule: "high-risk",
                selected_identity: "strong-worker",
                selected_empirical_version: "strong-worker-v1",
                candidates_json: "[]",
                risk_classes_json: r#"["security","routing"]"#,
                expected_file_count: 1,
            })
            .unwrap();
        let stored: (String, u64) = db
            .conn()
            .query_row(
                "SELECT risk_classes_json, expected_file_count FROM worker_selections WHERE selection_id = ?1",
                ["selection-1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (r#"["security","routing"]"#.into(), 1));
    }
}
