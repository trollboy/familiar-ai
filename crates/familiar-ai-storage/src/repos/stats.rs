//! Dashboard stats queries — global counts and recent items.

use familiar_ai_core::models::{Decision, FileSummary, SessionRollup};
use familiar_ai_core::FamiliarError;
use rusqlite::params;

use crate::sql;
use crate::Database;

#[derive(Debug, Clone)]
pub struct GlobalStats {
    pub projects: usize,
    pub active_projects: usize,
    pub file_summaries: usize,
    pub decisions: usize,
    pub session_rollups: usize,
}

#[derive(Debug, Clone)]
pub struct ProjectWithCounts {
    pub id: i64,
    pub name: String,
    pub repo_root: String,
    pub active: bool,
    pub last_used_at: String,
    pub file_summaries: usize,
    pub decisions: usize,
    pub session_rollups: usize,
}

fn count_query(db: &Database, sql: &str) -> familiar_ai_core::Result<usize> {
    let count: i64 = db
        .conn()
        .query_row(sql, [], |row| row.get(0))
        .map_err(|e| FamiliarError::Database(e.to_string()))?;
    Ok(count as usize)
}

pub fn global_stats(db: &Database) -> familiar_ai_core::Result<GlobalStats> {
    Ok(GlobalStats {
        projects: count_query(db, sql::COUNT_PROJECTS)?,
        active_projects: count_query(db, sql::COUNT_ACTIVE_PROJECTS)?,
        file_summaries: count_query(db, sql::COUNT_FILE_SUMMARIES)?,
        decisions: count_query(db, sql::COUNT_DECISIONS)?,
        session_rollups: count_query(db, sql::COUNT_SESSION_ROLLUPS)?,
    })
}

pub fn projects_with_counts(db: &Database) -> familiar_ai_core::Result<Vec<ProjectWithCounts>> {
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_PROJECTS_WITH_COUNTS)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map([], |row| {
            let last_used_at: String = row.get("last_used_at")?;
            let active_int: i64 = row.get("active")?;
            Ok(ProjectWithCounts {
                id: row.get("id")?,
                name: row.get("name")?,
                repo_root: row.get("repo_root")?,
                active: active_int != 0,
                last_used_at,
                file_summaries: row.get::<_, i64>("file_count")? as usize,
                decisions: row.get::<_, i64>("decision_count")? as usize,
                session_rollups: row.get::<_, i64>("rollup_count")? as usize,
            })
        })
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

pub fn recent_file_summaries(
    db: &Database,
    limit: usize,
) -> familiar_ai_core::Result<Vec<FileSummary>> {
    use super::file_summary::row_to_file_summary;
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_RECENT_FILE_SUMMARIES)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![limit as i64], row_to_file_summary)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

pub fn recent_decisions(db: &Database, limit: usize) -> familiar_ai_core::Result<Vec<Decision>> {
    use super::decision::row_to_decision;
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_RECENT_DECISIONS)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![limit as i64], row_to_decision)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

pub fn recent_session_rollups(
    db: &Database,
    limit: usize,
) -> familiar_ai_core::Result<Vec<SessionRollup>> {
    use super::session_rollup::row_to_session_rollup;
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_RECENT_SESSION_ROLLUPS)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![limit as i64], row_to_session_rollup)
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
    use crate::repos::decision::DecisionRepository;
    use crate::repos::file_summary::FileSummaryRepository;
    use crate::repos::project::ProjectRepository;
    use familiar_ai_core::models::{NewDecision, NewFileSummary, NewProject};

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    #[test]
    fn global_stats_on_empty_db() {
        let db = test_db();
        let stats = global_stats(&db).unwrap();
        assert_eq!(stats.projects, 0);
        assert_eq!(stats.active_projects, 0);
        assert_eq!(stats.file_summaries, 0);
        assert_eq!(stats.decisions, 0);
        assert_eq!(stats.session_rollups, 0);
    }

    #[test]
    fn global_stats_with_data() {
        let db = test_db();
        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: "/p".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "a.rs".into(),
            summary: "x".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        db.create_decision(&NewDecision {
            project_id: pid,
            title: "d".into(),
            summary: "x".into(),
            related_files: vec![],
            source_session: None,
            confidence: None,
        })
        .unwrap();

        let stats = global_stats(&db).unwrap();
        assert_eq!(stats.projects, 1);
        assert_eq!(stats.active_projects, 1);
        assert_eq!(stats.file_summaries, 1);
        assert_eq!(stats.decisions, 1);
    }

    #[test]
    fn projects_with_counts_returns_aggregates() {
        let db = test_db();
        let pid = db
            .create_project(&NewProject {
                name: "test".into(),
                repo_root: "/test".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        for i in 0..3 {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: format!("f{i}.rs"),
                summary: "x".into(),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        let projects = projects_with_counts(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].file_summaries, 3);
        assert_eq!(projects[0].name, "test");
    }

    #[test]
    fn recent_queries_return_latest() {
        let db = test_db();
        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: "/p".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        for i in 0..5 {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: format!("f{i}.rs"),
                summary: format!("s{i}"),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        let recent = recent_file_summaries(&db, 3).unwrap();
        assert_eq!(recent.len(), 3);
    }
}
