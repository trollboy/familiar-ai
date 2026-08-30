use familiar_ai_core::FamiliarError;
use rusqlite::{params, OptionalExtension};

use super::now_rfc3339;
use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamiliarTomlDecision {
    pub decision: String,
    pub actor: String,
    pub content_hash: String,
    pub content: String,
    pub created_at: String,
}

pub struct FamiliarTomlRepository<'a> {
    db: &'a Database,
}

impl<'a> FamiliarTomlRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn record(
        &self,
        repository_key: &str,
        decision: &str,
        actor: &str,
        content_hash: &str,
        content: &str,
    ) -> familiar_ai_core::Result<()> {
        if !matches!(decision, "approve" | "revoke") {
            return Err(FamiliarError::Database(
                "invalid familiar.toml decision".into(),
            ));
        }
        self.db.conn().execute(
            "INSERT INTO project_config_decisions(repository_key,decision,actor,content_hash,content,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![repository_key, decision, actor, content_hash, content, now_rfc3339()],
        ).map_err(|error| FamiliarError::Database(error.to_string()))?;
        Ok(())
    }

    pub fn latest(
        &self,
        repository_key: &str,
    ) -> familiar_ai_core::Result<Option<FamiliarTomlDecision>> {
        self.db.conn().query_row(
            "SELECT decision,actor,content_hash,content,created_at FROM project_config_decisions WHERE repository_key=?1 ORDER BY id DESC LIMIT 1",
            [repository_key], |row| Ok(FamiliarTomlDecision { decision: row.get(0)?, actor: row.get(1)?, content_hash: row.get(2)?, content: row.get(3)?, created_at: row.get(4)? })
        ).optional().map_err(|error| FamiliarError::Database(error.to_string()))
    }
}
