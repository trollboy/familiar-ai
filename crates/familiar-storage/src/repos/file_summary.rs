use familiar_core::models::{FileSummary, NewFileSummary};
use familiar_core::FamiliarError;
use rusqlite::{params, OptionalExtension};

use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

pub trait FileSummaryRepository {
    fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> familiar_core::Result<FileSummary>;
    fn get_file_summary_by_path(
        &self,
        project_id: i64,
        path: &str,
    ) -> familiar_core::Result<Option<FileSummary>>;
    fn list_file_summaries_by_project(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<Vec<FileSummary>>;
    fn delete_file_summary(&self, id: i64) -> familiar_core::Result<()>;
}

pub(crate) fn row_to_file_summary(row: &rusqlite::Row) -> rusqlite::Result<FileSummary> {
    let tags_json: String = row.get("tags_json")?;
    let extracted_symbols_json: Option<String> = row.get("extracted_symbols_json")?;
    let last_updated_at_str: String = row.get("last_updated_at")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;

    let extracted_symbols = match extracted_symbols_json {
        Some(s) if !s.is_empty() => json_to_vec(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        _ => Vec::new(),
    };

    Ok(FileSummary {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        path: row.get("path")?,
        summary: row.get("summary")?,
        tags: json_to_vec(&tags_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        extracted_symbols,
        last_known_mtime: row.get("last_known_mtime")?,
        last_known_size: row.get("last_known_size")?,
        last_updated_at: parse_dt(&last_updated_at_str).map_err(|e| {
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

impl FileSummaryRepository for Database {
    fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> familiar_core::Result<FileSummary> {
        let now = now_rfc3339();
        let tags_json = vec_to_json(&summary.tags)?;
        let symbols_json = vec_to_json(&summary.extracted_symbols)?;

        self.conn()
            .execute(
                sql::UPSERT_FILE_SUMMARY,
                params![
                    summary.project_id,
                    summary.path,
                    summary.summary,
                    tags_json,
                    symbols_json,
                    summary.last_known_mtime,
                    summary.last_known_size,
                    now,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        self.get_file_summary_by_path(summary.project_id, &summary.path)?
            .ok_or_else(|| FamiliarError::Database("failed to read back file summary".into()))
    }

    fn get_file_summary_by_path(
        &self,
        project_id: i64,
        path: &str,
    ) -> familiar_core::Result<Option<FileSummary>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_FILE_SUMMARY_BY_PATH)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        stmt.query_row(params![project_id, path], row_to_file_summary)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))
    }

    fn list_file_summaries_by_project(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<Vec<FileSummary>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_FILE_SUMMARIES_BY_PROJECT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![project_id], row_to_file_summary)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn delete_file_summary(&self, id: i64) -> familiar_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_FILE_SUMMARY, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

/// Query file summaries under a path prefix, with a SQL-side limit.
/// Used by the MCP `get_module_summary` tool.
pub fn list_file_summaries_under(
    db: &Database,
    project_id: i64,
    path_prefix: &str,
    limit: usize,
) -> familiar_core::Result<Vec<FileSummary>> {
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_FILE_SUMMARIES_UNDER)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![project_id, path_prefix, limit as i64],
            row_to_file_summary,
        )
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

/// Count file summaries under a path prefix without retrieving rows.
/// Used by the MCP `get_module_summary` tool to report true file_count.
pub fn count_file_summaries_under(
    db: &Database,
    project_id: i64,
    path_prefix: &str,
) -> familiar_core::Result<usize> {
    let mut stmt = db
        .conn()
        .prepare(sql::COUNT_FILE_SUMMARIES_UNDER)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let count: i64 = stmt
        .query_row(params![project_id, path_prefix], |row| row.get(0))
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    Ok(count as usize)
}

/// Search file summaries by keyword (case-insensitive LIKE across summary, symbols, path).
pub fn search_file_summaries(
    db: &Database,
    project_id: i64,
    query: &str,
    limit: usize,
) -> familiar_core::Result<Vec<FileSummary>> {
    let mut stmt = db
        .conn()
        .prepare(sql::SEARCH_FILE_SUMMARIES)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![project_id, query, limit as i64],
            row_to_file_summary,
        )
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
    use familiar_core::models::NewProject;

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

    fn sample_summary(project_id: i64) -> NewFileSummary {
        NewFileSummary {
            project_id,
            path: "src/main.rs".into(),
            summary: "Main entry point".into(),
            tags: vec!["entry".into(), "code".into()],
            extracted_symbols: vec!["main".into(), "Config".into()],
            last_known_mtime: Some(1_700_000_000),
            last_known_size: Some(1024),
        }
    }

    #[test]
    fn create_and_get_by_path() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        assert_eq!(created.path, "src/main.rs");
        assert_eq!(created.summary, "Main entry point");
        assert_eq!(created.tags, vec!["entry", "code"]);
        assert_eq!(created.extracted_symbols, vec!["main", "Config"]);
        assert_eq!(created.last_known_mtime, Some(1_700_000_000));
        assert_eq!(created.last_known_size, Some(1024));

        let fetched = db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.extracted_symbols, vec!["main", "Config"]);
    }

    #[test]
    fn upsert_updates_existing() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();

        let updated_input = NewFileSummary {
            project_id: pid,
            path: "src/main.rs".into(),
            summary: "Updated summary".into(),
            tags: vec!["updated".into()],
            extracted_symbols: vec!["new_fn".into()],
            last_known_mtime: Some(1_700_000_500),
            last_known_size: Some(2048),
        };
        let updated = db.create_or_update_file_summary(&updated_input).unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.summary, "Updated summary");
        assert_eq!(updated.tags, vec!["updated"]);
        assert_eq!(updated.extracted_symbols, vec!["new_fn"]);
        assert_eq!(updated.last_known_mtime, Some(1_700_000_500));
        assert_eq!(updated.last_known_size, Some(2048));
    }

    #[test]
    fn list_by_project() {
        let db = test_db();
        let pid = create_test_project(&db);
        db.create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/lib.rs".into(),
            summary: "Library root".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        let all = db.list_file_summaries_by_project(pid).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_under_prefix_with_limit() {
        let db = test_db();
        let pid = create_test_project(&db);
        for path in ["src/a.rs", "src/b.rs", "src/sub/c.rs", "tests/d.rs"] {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: path.into(),
                summary: "x".into(),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        let under_src = list_file_summaries_under(&db, pid, "src/", 100).unwrap();
        assert_eq!(under_src.len(), 3);

        let under_src_limited = list_file_summaries_under(&db, pid, "src/", 2).unwrap();
        assert_eq!(under_src_limited.len(), 2);

        let under_tests = list_file_summaries_under(&db, pid, "tests/", 100).unwrap();
        assert_eq!(under_tests.len(), 1);
    }

    #[test]
    fn count_under_prefix() {
        let db = test_db();
        let pid = create_test_project(&db);
        for path in ["src/a.rs", "src/b.rs", "src/sub/c.rs", "tests/d.rs"] {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: path.into(),
                summary: "x".into(),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        assert_eq!(count_file_summaries_under(&db, pid, "src/").unwrap(), 3);
        assert_eq!(count_file_summaries_under(&db, pid, "tests/").unwrap(), 1);
        assert_eq!(count_file_summaries_under(&db, pid, "nope/").unwrap(), 0);
    }

    #[test]
    fn project_isolation() {
        let db = test_db();
        let pid1 = create_test_project(&db);

        let pid2 = db
            .create_project(&NewProject {
                name: "other".into(),
                repo_root: "/other/project".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        db.create_or_update_file_summary(&sample_summary(pid1))
            .unwrap();
        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid2,
            path: "src/main.rs".into(),
            summary: "Other project main".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        let p1_summaries = db.list_file_summaries_by_project(pid1).unwrap();
        let p2_summaries = db.list_file_summaries_by_project(pid2).unwrap();
        assert_eq!(p1_summaries.len(), 1);
        assert_eq!(p2_summaries.len(), 1);
        assert_eq!(p1_summaries[0].summary, "Main entry point");
        assert_eq!(p2_summaries[0].summary, "Other project main");
    }

    #[test]
    fn delete_file_summary() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        db.delete_file_summary(created.id).unwrap();
        assert!(db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .is_none());
    }
}
