use familiar_ai_core::models::{NewSessionRollup, SessionRollup};
use familiar_ai_core::FamiliarError;
use rusqlite::{params, OptionalExtension};

use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

pub trait SessionRollupRepository {
    fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> familiar_ai_core::Result<SessionRollup>;
    fn get_session_rollup_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<SessionRollup>>;
    fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<SessionRollup>>;
    fn delete_session_rollup(&self, id: i64) -> familiar_ai_core::Result<()>;
}

pub(crate) fn row_to_session_rollup(row: &rusqlite::Row) -> rusqlite::Result<SessionRollup> {
    let related_files_json: String = row.get("related_files_json")?;
    let next_steps_json: String = row.get("next_steps_json")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;

    Ok(SessionRollup {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        summary: row.get("summary")?,
        related_files: json_to_vec(&related_files_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        next_steps: json_to_vec(&next_steps_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_at: parse_dt(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: parse_dt(&updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

impl SessionRollupRepository for Database {
    fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> familiar_ai_core::Result<SessionRollup> {
        let now = now_rfc3339();
        let related_json = vec_to_json(&rollup.related_files)?;
        let next_steps_json = vec_to_json(&rollup.next_steps)?;

        self.conn()
            .execute(
                sql::INSERT_SESSION_ROLLUP,
                params![
                    rollup.project_id,
                    rollup.summary,
                    related_json,
                    next_steps_json,
                    now,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let id = self.conn().last_insert_rowid();
        self.get_session_rollup_by_id(id)?.ok_or_else(|| {
            FamiliarError::Database("failed to read back created session rollup".into())
        })
    }

    fn get_session_rollup_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<SessionRollup>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_SESSION_ROLLUP_BY_ID)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        stmt.query_row(params![id], row_to_session_rollup)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))
    }

    fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<SessionRollup>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_SESSION_ROLLUPS_BY_PROJECT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![project_id, limit as i64], row_to_session_rollup)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn delete_session_rollup(&self, id: i64) -> familiar_ai_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_SESSION_ROLLUP, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::project::ProjectRepository;
    use familiar_ai_core::models::NewProject;

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn create_test_project(db: &Database) -> i64 {
        db.create_project(&NewProject {
            name: "test".into(),
            repo_root: "/test/project".into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap()
        .id
    }

    #[test]
    fn create_and_get_by_id() {
        let db = test_db();
        let pid = create_test_project(&db);
        let rollup = db
            .create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: "Implemented auth token rotation".into(),
                related_files: vec!["src/auth/token.rs".into()],
                next_steps: vec!["Add integration tests".into(), "Update docs".into()],
            })
            .unwrap();

        assert_eq!(rollup.summary, "Implemented auth token rotation");
        assert_eq!(rollup.related_files, vec!["src/auth/token.rs"]);
        assert_eq!(
            rollup.next_steps,
            vec!["Add integration tests", "Update docs"]
        );

        let fetched = db.get_session_rollup_by_id(rollup.id).unwrap().unwrap();
        assert_eq!(fetched.id, rollup.id);
    }

    #[test]
    fn list_ordered_by_created_desc() {
        let db = test_db();
        let pid = create_test_project(&db);

        for i in 0..3 {
            db.create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: format!("Rollup {i}"),
                related_files: vec![],
                next_steps: vec![],
            })
            .unwrap();
        }

        let rollups = db.list_session_rollups_by_project(pid, 100).unwrap();
        assert_eq!(rollups.len(), 3);
        assert_eq!(rollups[0].summary, "Rollup 2");
        assert_eq!(rollups[2].summary, "Rollup 0");
    }

    #[test]
    fn delete_rollup() {
        let db = test_db();
        let pid = create_test_project(&db);
        let rollup = db
            .create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: "Test".into(),
                related_files: vec![],
                next_steps: vec![],
            })
            .unwrap();
        db.delete_session_rollup(rollup.id).unwrap();
        assert!(db.get_session_rollup_by_id(rollup.id).unwrap().is_none());
    }
}
