use familiar_ai_core::models::{Decision, NewDecision};
use familiar_ai_core::FamiliarError;
use rusqlite::{params, OptionalExtension};

use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

pub trait DecisionRepository {
    fn create_decision(&self, decision: &NewDecision) -> familiar_ai_core::Result<Decision>;
    fn get_decision_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<Decision>>;
    fn list_decisions_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<Decision>>;
    fn delete_decision(&self, id: i64) -> familiar_ai_core::Result<()>;
}

pub(crate) fn row_to_decision(row: &rusqlite::Row) -> rusqlite::Result<Decision> {
    let related_files_json: String = row.get("related_files_json")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;

    Ok(Decision {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        title: row.get("title")?,
        summary: row.get("summary")?,
        related_files: json_to_vec(&related_files_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        source_session: row.get("source_session")?,
        confidence: row.get("confidence")?,
        created_at: parse_dt(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: parse_dt(&updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

impl DecisionRepository for Database {
    fn create_decision(&self, decision: &NewDecision) -> familiar_ai_core::Result<Decision> {
        let now = now_rfc3339();
        let related_json = vec_to_json(&decision.related_files)?;

        self.conn()
            .execute(
                sql::INSERT_DECISION,
                params![
                    decision.project_id,
                    decision.title,
                    decision.summary,
                    related_json,
                    decision.source_session,
                    decision.confidence,
                    now,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let id = self.conn().last_insert_rowid();
        self.get_decision_by_id(id)?
            .ok_or_else(|| FamiliarError::Database("failed to read back created decision".into()))
    }

    fn get_decision_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<Decision>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_DECISION_BY_ID)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        stmt.query_row(params![id], row_to_decision)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))
    }

    fn list_decisions_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<Decision>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_DECISIONS_BY_PROJECT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![project_id, limit as i64], row_to_decision)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn delete_decision(&self, id: i64) -> familiar_ai_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_DECISION, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

/// Search decisions by keyword (case-insensitive LIKE across title + summary).
pub fn search_decisions(
    db: &Database,
    project_id: i64,
    query: &str,
    limit: usize,
) -> familiar_ai_core::Result<Vec<Decision>> {
    let mut stmt = db
        .conn()
        .prepare(sql::SEARCH_DECISIONS)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![project_id, query, limit as i64], row_to_decision)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
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
        let decision = db
            .create_decision(&NewDecision {
                project_id: pid,
                title: "Use Redis for sessions".into(),
                summary: "Redis is faster for session storage".into(),
                related_files: vec!["src/auth/session.rs".into()],
                source_session: Some("session-123".into()),
                confidence: Some("high".into()),
            })
            .unwrap();

        assert_eq!(decision.title, "Use Redis for sessions");
        assert_eq!(decision.related_files, vec!["src/auth/session.rs"]);
        assert_eq!(decision.source_session, Some("session-123".into()));
        assert_eq!(decision.confidence, Some("high".into()));

        let fetched = db.get_decision_by_id(decision.id).unwrap().unwrap();
        assert_eq!(fetched.id, decision.id);
        assert_eq!(fetched.confidence, Some("high".into()));
    }

    #[test]
    fn confidence_can_be_none() {
        let db = test_db();
        let pid = create_test_project(&db);
        let decision = db
            .create_decision(&NewDecision {
                project_id: pid,
                title: "no confidence".into(),
                summary: "x".into(),
                related_files: vec![],
                source_session: None,
                confidence: None,
            })
            .unwrap();
        assert!(decision.confidence.is_none());
    }

    #[test]
    fn list_ordered_by_created_desc() {
        let db = test_db();
        let pid = create_test_project(&db);

        for i in 0..3 {
            db.create_decision(&NewDecision {
                project_id: pid,
                title: format!("Decision {i}"),
                summary: format!("Summary {i}"),
                related_files: vec![],
                source_session: None,
                confidence: None,
            })
            .unwrap();
        }

        let decisions = db.list_decisions_by_project(pid, 100).unwrap();
        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[0].title, "Decision 2");
        assert_eq!(decisions[2].title, "Decision 0");
    }

    #[test]
    fn delete_decision() {
        let db = test_db();
        let pid = create_test_project(&db);
        let decision = db
            .create_decision(&NewDecision {
                project_id: pid,
                title: "Test".into(),
                summary: "Test".into(),
                related_files: vec![],
                source_session: None,
                confidence: None,
            })
            .unwrap();
        db.delete_decision(decision.id).unwrap();
        assert!(db.get_decision_by_id(decision.id).unwrap().is_none());
    }
}
