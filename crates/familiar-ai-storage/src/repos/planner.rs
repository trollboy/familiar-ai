use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

use super::{now_rfc3339, parse_dt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerBatchRecord {
    pub batch_id: String,
    pub repository_key: String,
    pub status: String,
    pub actor: String,
    pub reason: Option<String>,
    pub file_hashes: Vec<(String, String)>,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

pub struct PlannerBatchRepository<'a> {
    conn: &'a Connection,
}
impl<'a> PlannerBatchRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record(
        &self,
        batch_id: &str,
        repository_key: &str,
        status: &str,
        actor: &str,
        reason: Option<&str>,
        file_hashes: &[(String, String)],
    ) -> familiar_ai_core::Result<()> {
        let json = serde_json::to_string(file_hashes).map_err(|e| {
            FamiliarError::Database(format!("failed to serialize planner hashes: {e}"))
        })?;
        self.conn.execute(
            "INSERT INTO planner_batches(batch_id,repository_key,status,actor,reason,file_hashes_json,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![batch_id, repository_key, status, actor, reason, json, now_rfc3339()],
        ).map_err(|e| FamiliarError::Database(format!("failed to record planner batch: {e}")))?;
        Ok(())
    }

    pub fn get(&self, batch_id: &str) -> familiar_ai_core::Result<Option<PlannerBatchRecord>> {
        self.conn.query_row(
            "SELECT batch_id,repository_key,status,actor,reason,file_hashes_json,recorded_at FROM planner_batches WHERE batch_id=?1",
            [batch_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?))
        ).optional().map_err(|e| FamiliarError::Database(e.to_string()))?.map(|r| Ok(PlannerBatchRecord {
            batch_id:r.0, repository_key:r.1, status:r.2, actor:r.3, reason:r.4,
            file_hashes: serde_json::from_str(&r.5).map_err(|e| FamiliarError::Database(e.to_string()))?,
            recorded_at: parse_dt(&r.6)?,
        })).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = PlannerBatchRepository::new(db.conn());
        repo.record(
            "batch-1",
            "repo",
            "approved",
            "human:a",
            None,
            &[("PRD-001.md".into(), "abc".into())],
        )
        .unwrap();
        let got = repo.get("batch-1").unwrap().unwrap();
        assert_eq!(got.actor, "human:a");
        assert_eq!(got.file_hashes[0].1, "abc");
    }
}
