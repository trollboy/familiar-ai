use familiar_core::models::{NewProject, Project};
use familiar_core::FamiliarError;
use rusqlite::params;

use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

pub trait ProjectRepository {
    fn create_project(&self, project: &NewProject) -> familiar_core::Result<Project>;
    fn get_project_by_id(&self, id: i64) -> familiar_core::Result<Option<Project>>;
    fn get_project_by_repo_root(&self, repo_root: &str) -> familiar_core::Result<Option<Project>>;
    fn list_active_projects(&self) -> familiar_core::Result<Vec<Project>>;
    fn update_project(&self, project: &Project) -> familiar_core::Result<()>;
    fn delete_project(&self, id: i64) -> familiar_core::Result<()>;
}

fn row_to_project(row: &rusqlite::Row) -> rusqlite::Result<Project> {
    let ignored_paths_json: String = row.get("ignored_paths_json")?;
    let last_used_at_str: String = row.get("last_used_at")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;
    let active_int: i64 = row.get("active")?;

    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        repo_root: row.get("repo_root")?,
        active: active_int != 0,
        last_used_at: parse_dt(&last_used_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        ignored_paths: json_to_vec(&ignored_paths_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        token_budget: row.get("token_budget")?,
        created_at: parse_dt(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: parse_dt(&updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

impl ProjectRepository for Database {
    fn create_project(&self, project: &NewProject) -> familiar_core::Result<Project> {
        let now = now_rfc3339();
        let ignored_json = vec_to_json(&project.ignored_paths)?;

        self.conn()
            .execute(
                sql::INSERT_PROJECT,
                params![
                    project.name,
                    project.repo_root,
                    now,
                    ignored_json,
                    project.token_budget
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let id = self.conn().last_insert_rowid();
        self.get_project_by_id(id)?
            .ok_or_else(|| FamiliarError::Database("failed to read back created project".into()))
    }

    fn get_project_by_id(&self, id: i64) -> familiar_core::Result<Option<Project>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_PROJECT_BY_ID)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let result = stmt
            .query_row(params![id], row_to_project)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        Ok(result)
    }

    fn get_project_by_repo_root(&self, repo_root: &str) -> familiar_core::Result<Option<Project>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_PROJECT_BY_REPO_ROOT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let result = stmt
            .query_row(params![repo_root], row_to_project)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        Ok(result)
    }

    fn list_active_projects(&self) -> familiar_core::Result<Vec<Project>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_ACTIVE_PROJECTS)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], row_to_project)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(projects)
    }

    fn update_project(&self, project: &Project) -> familiar_core::Result<()> {
        let now = now_rfc3339();
        let ignored_json = vec_to_json(&project.ignored_paths)?;

        self.conn()
            .execute(
                sql::UPDATE_PROJECT,
                params![
                    project.name,
                    project.repo_root,
                    project.active as i64,
                    project.last_used_at.to_rfc3339(),
                    ignored_json,
                    project.token_budget,
                    now,
                    project.id,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        Ok(())
    }

    fn delete_project(&self, id: i64) -> familiar_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_PROJECT, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn sample_project() -> NewProject {
        NewProject {
            name: "test-project".into(),
            repo_root: "/home/user/projects/test".into(),
            ignored_paths: vec!["target/".into(), ".git/".into()],
            token_budget: Some(5000),
        }
    }

    #[test]
    fn create_and_get_by_id() {
        let db = test_db();
        let created = db.create_project(&sample_project()).unwrap();
        assert_eq!(created.name, "test-project");
        assert_eq!(created.repo_root, "/home/user/projects/test");
        assert!(created.active);
        assert_eq!(created.ignored_paths, vec!["target/", ".git/"]);
        assert_eq!(created.token_budget, Some(5000));

        let fetched = db.get_project_by_id(created.id).unwrap().unwrap();
        assert_eq!(fetched.name, created.name);
        assert_eq!(fetched.id, created.id);
    }

    #[test]
    fn get_by_repo_root() {
        let db = test_db();
        let created = db.create_project(&sample_project()).unwrap();
        let fetched = db
            .get_project_by_repo_root("/home/user/projects/test")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.id, created.id);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let db = test_db();
        assert!(db.get_project_by_id(999).unwrap().is_none());
        assert!(db.get_project_by_repo_root("/nope").unwrap().is_none());
    }

    #[test]
    fn list_active_projects() {
        let db = test_db();
        let p1 = db.create_project(&sample_project()).unwrap();

        let mut p2_input = sample_project();
        p2_input.repo_root = "/other/project".into();
        p2_input.name = "other".into();
        let mut p2 = db.create_project(&p2_input).unwrap();

        // Deactivate p2
        p2.active = false;
        db.update_project(&p2).unwrap();

        let active = db.list_active_projects().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, p1.id);
    }

    #[test]
    fn update_project() {
        let db = test_db();
        let mut project = db.create_project(&sample_project()).unwrap();
        project.name = "renamed".into();
        project.token_budget = Some(10000);
        db.update_project(&project).unwrap();

        let fetched = db.get_project_by_id(project.id).unwrap().unwrap();
        assert_eq!(fetched.name, "renamed");
        assert_eq!(fetched.token_budget, Some(10000));
    }

    #[test]
    fn delete_project() {
        let db = test_db();
        let project = db.create_project(&sample_project()).unwrap();
        db.delete_project(project.id).unwrap();
        assert!(db.get_project_by_id(project.id).unwrap().is_none());
    }

    #[test]
    fn duplicate_repo_root_fails() {
        let db = test_db();
        db.create_project(&sample_project()).unwrap();
        let result = db.create_project(&sample_project());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNIQUE"), "expected UNIQUE error, got: {err}");
    }
}
