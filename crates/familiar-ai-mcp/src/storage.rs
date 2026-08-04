use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use familiar_ai_core::models::{
    Decision, FileSummary, NewDecision, NewFileSummary, NewSessionRollup, Project, SessionRollup,
};
use familiar_ai_storage::repos::decision::search_decisions as repo_search_decisions;
use familiar_ai_storage::repos::file_summary::{
    count_file_summaries_under as repo_count_under, list_file_summaries_under as repo_list_under,
    search_file_summaries as repo_search_summaries,
};
use familiar_ai_storage::{
    Database, DecisionRepository, FileSummaryRepository, ProjectRepository, SessionRollupRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not found")]
    NotFound,
    #[error("storage error: {0}")]
    Other(String),
}

/// Storage abstraction for tools.
///
/// TODO: methods are async even though SQLite is sync. This is intentional —
/// future backends (remote storage, caching, vector search service, Postgres
/// migration) will need true async, and trait signatures should not change.
/// For now the SqliteStorage impl just wraps sync rusqlite calls.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn list_decisions_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> Result<Vec<Decision>, StorageError>;

    async fn get_project_by_id(&self, id: i64) -> Result<Option<Project>, StorageError>;

    async fn get_project_by_repo_root(
        &self,
        repo_root: &str,
    ) -> Result<Option<Project>, StorageError>;

    async fn list_active_projects(&self) -> Result<Vec<Project>, StorageError>;

    async fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> Result<SessionRollup, StorageError>;

    async fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> Result<Vec<SessionRollup>, StorageError>;

    async fn create_decision(&self, decision: &NewDecision) -> Result<Decision, StorageError>;

    async fn get_file_summary(
        &self,
        project_id: i64,
        path: &str,
    ) -> Result<Option<FileSummary>, StorageError>;

    async fn list_file_summaries_under(
        &self,
        project_id: i64,
        path_prefix: &str,
        limit: usize,
    ) -> Result<Vec<FileSummary>, StorageError>;

    async fn count_file_summaries_under(
        &self,
        project_id: i64,
        path_prefix: &str,
    ) -> Result<usize, StorageError>;

    async fn search_file_summaries(
        &self,
        project_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FileSummary>, StorageError>;

    async fn search_decisions(
        &self,
        project_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Decision>, StorageError>;

    async fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> Result<FileSummary, StorageError>;
}

/// SQLite-backed Storage implementation.
pub struct SqliteStorage {
    db: Arc<Mutex<Database>>,
}

impl SqliteStorage {
    pub fn new(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }
}

fn map_err<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Other(e.to_string())
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn list_decisions_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> Result<Vec<Decision>, StorageError> {
        let db = self.db.lock().unwrap();
        db.list_decisions_by_project(project_id, limit)
            .map_err(map_err)
    }

    async fn get_project_by_id(&self, id: i64) -> Result<Option<Project>, StorageError> {
        let db = self.db.lock().unwrap();
        db.get_project_by_id(id).map_err(map_err)
    }

    async fn get_project_by_repo_root(
        &self,
        repo_root: &str,
    ) -> Result<Option<Project>, StorageError> {
        let db = self.db.lock().unwrap();
        db.get_project_by_repo_root(repo_root).map_err(map_err)
    }

    async fn list_active_projects(&self) -> Result<Vec<Project>, StorageError> {
        let db = self.db.lock().unwrap();
        db.list_active_projects().map_err(map_err)
    }

    async fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> Result<SessionRollup, StorageError> {
        let db = self.db.lock().unwrap();
        db.create_session_rollup(rollup).map_err(map_err)
    }

    async fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> Result<Vec<SessionRollup>, StorageError> {
        let db = self.db.lock().unwrap();
        db.list_session_rollups_by_project(project_id, limit)
            .map_err(map_err)
    }

    async fn create_decision(&self, decision: &NewDecision) -> Result<Decision, StorageError> {
        let db = self.db.lock().unwrap();
        db.create_decision(decision).map_err(map_err)
    }

    async fn get_file_summary(
        &self,
        project_id: i64,
        path: &str,
    ) -> Result<Option<FileSummary>, StorageError> {
        let db = self.db.lock().unwrap();
        db.get_file_summary_by_path(project_id, path)
            .map_err(map_err)
    }

    async fn list_file_summaries_under(
        &self,
        project_id: i64,
        path_prefix: &str,
        limit: usize,
    ) -> Result<Vec<FileSummary>, StorageError> {
        let db = self.db.lock().unwrap();
        repo_list_under(&db, project_id, path_prefix, limit).map_err(map_err)
    }

    async fn count_file_summaries_under(
        &self,
        project_id: i64,
        path_prefix: &str,
    ) -> Result<usize, StorageError> {
        let db = self.db.lock().unwrap();
        repo_count_under(&db, project_id, path_prefix).map_err(map_err)
    }

    async fn search_file_summaries(
        &self,
        project_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FileSummary>, StorageError> {
        let db = self.db.lock().unwrap();
        repo_search_summaries(&db, project_id, query, limit).map_err(map_err)
    }

    async fn search_decisions(
        &self,
        project_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Decision>, StorageError> {
        let db = self.db.lock().unwrap();
        repo_search_decisions(&db, project_id, query, limit).map_err(map_err)
    }

    async fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> Result<FileSummary, StorageError> {
        let db = self.db.lock().unwrap();
        db.create_or_update_file_summary(summary).map_err(map_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::models::{NewDecision, NewProject};

    fn make_db() -> Arc<Mutex<Database>> {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        Arc::new(Mutex::new(db))
    }

    #[tokio::test]
    async fn list_decisions_with_limit() {
        let db = make_db();
        let storage = SqliteStorage::new(db.clone());

        let pid = {
            let d = db.lock().unwrap();
            d.create_project(&NewProject {
                name: "p".into(),
                repo_root: "/p".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id
        };

        for i in 0..5 {
            let d = db.lock().unwrap();
            d.create_decision(&NewDecision {
                project_id: pid,
                title: format!("d{i}"),
                summary: "s".into(),
                related_files: vec![],
                source_session: None,
                confidence: None,
            })
            .unwrap();
        }

        let result = storage.list_decisions_by_project(pid, 3).await.unwrap();
        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn get_project_by_repo_root() {
        let db = make_db();
        let storage = SqliteStorage::new(db.clone());

        {
            let d = db.lock().unwrap();
            d.create_project(&NewProject {
                name: "p".into(),
                repo_root: "/test/repo".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap();
        }

        let p = storage
            .get_project_by_repo_root("/test/repo")
            .await
            .unwrap();
        assert!(p.is_some());

        let none = storage.get_project_by_repo_root("/nope").await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn create_session_rollup() {
        let db = make_db();
        let storage = SqliteStorage::new(db.clone());

        let pid = {
            let d = db.lock().unwrap();
            d.create_project(&NewProject {
                name: "p".into(),
                repo_root: "/p".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id
        };

        let rollup = storage
            .create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: "test".into(),
                related_files: vec!["a".into()],
                next_steps: vec!["b".into()],
            })
            .await
            .unwrap();

        assert_eq!(rollup.summary, "test");
    }
}
