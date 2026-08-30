use familiar_ai_core::FamiliarError;
use rusqlite::params;

use super::now_rfc3339;
use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDecision {
    pub command: String,
    pub actor: String,
    pub before_hash: String,
    pub after_hash: String,
    pub created_at: String,
}

pub struct ConfigDecisionRepository<'a> {
    db: &'a Database,
}

impl<'a> ConfigDecisionRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn record(
        &self,
        command: &str,
        actor: &str,
        before_hash: &str,
        after_hash: &str,
    ) -> familiar_ai_core::Result<()> {
        self.db
            .conn()
            .execute(
                "INSERT INTO config_decisions(command,actor,before_hash,after_hash,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![command, actor, before_hash, after_hash, now_rfc3339()],
            )
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        Ok(())
    }

    pub fn list(&self, limit: usize) -> familiar_ai_core::Result<Vec<ConfigDecision>> {
        let mut statement = self.db.conn().prepare(
            "SELECT command,actor,before_hash,after_hash,created_at FROM config_decisions ORDER BY id DESC LIMIT ?1",
        ).map_err(|error| FamiliarError::Database(error.to_string()))?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(ConfigDecision {
                    command: row.get(0)?,
                    actor: row.get(1)?,
                    before_hash: row.get(2)?,
                    after_hash: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| FamiliarError::Database(error.to_string()))
    }
}
