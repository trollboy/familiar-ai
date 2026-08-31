use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use familiar_ai_core::models::{
    Decision, FileSummary, NewDecision, NewFileSummary, NewSessionRollup, Project, SessionRollup,
};
use familiar_ai_core::{
    BacklogEntry, BacklogRecoveryAction, BacklogStatusStore, BootstrapRollbackResult,
    DiscoveredPrd, RepositoryIdentity,
};
use familiar_ai_storage::repos::decision::search_decisions as repo_search_decisions;
use familiar_ai_storage::repos::file_summary::{
    count_file_summaries_under as repo_count_under, list_file_summaries_under as repo_list_under,
    search_file_summaries as repo_search_summaries,
};
use familiar_ai_storage::{
    BacklogEntryRow, BudgetSummary, DeliveryDecisionRow, DeliveryRepository, DriverAttempt,
    DriverRepository, DriverSession, ExecutionCheckpoint, PendingGate, RecoveryEventRow,
    ReviewFindingsRow, UsageSeriesPoint, UsageSeriesRequest,
};
use familiar_ai_storage::{
    CheckpointRepository, Database, DecisionRepository, FileSummaryRepository, ProjectRepository,
    SessionRollupRepository, SqliteBacklogRepository, SqliteBootstrapRepository,
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
    async fn usage_series(
        &self,
        _request: &UsageSeriesRequest,
    ) -> Result<Vec<UsageSeriesPoint>, StorageError> {
        Ok(Vec::new())
    }
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

    // --- Stewardship state (PRD-035): repository-scoped, read-first ---
    // Reads default to an empty/absent result rather than an error so a
    // future non-SQLite backend or test double is not forced to implement
    // every method before it can compile.

    async fn list_backlog(
        &self,
        _repository_key: &str,
        _status: Option<&str>,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<BacklogEntryRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_driver_sessions(
        &self,
        _repository_key: &str,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<DriverSession>, StorageError> {
        Ok(Vec::new())
    }

    /// Used to check repository ownership before disclosing a session's
    /// attempts or budget to a caller who only supplied a `session_id`.
    async fn get_driver_session(
        &self,
        _session_id: &str,
    ) -> Result<Option<DriverSession>, StorageError> {
        Ok(None)
    }

    async fn list_driver_attempts(
        &self,
        _session_id: &str,
        _after: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<DriverAttempt>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_checkpoints(
        &self,
        _repository_key: &str,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<ExecutionCheckpoint>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_recovery_events(
        &self,
        _repository_key: &str,
        _after: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<RecoveryEventRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_delivery_decisions(
        &self,
        _repository_key: &str,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<DeliveryDecisionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn get_budget_summary(
        &self,
        _session_id: &str,
    ) -> Result<Option<BudgetSummary>, StorageError> {
        Ok(None)
    }

    async fn list_review_findings(
        &self,
        _repository_key: &str,
        _session_id: &str,
    ) -> Result<Vec<ReviewFindingsRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_pending_human_gates(
        &self,
        _repository_key: &str,
        _limit: usize,
    ) -> Result<Vec<PendingGate>, StorageError> {
        Ok(Vec::new())
    }

    // --- Stewardship mutations (PRD-035) ---
    // Each wraps exactly the same audited domain call the `familiar-ai`
    // CLI's `backlog release`/`backlog complete`/`backlog record-complete`/
    // `backlog bootstrap rollback` commands make — same actor/reason
    // validation, same storage transaction, same audit trail.

    async fn backlog_recover(
        &self,
        _repository: &RepositoryIdentity,
        _target: &DiscoveredPrd,
        _action: BacklogRecoveryAction,
        _actor: &str,
        _reason: &str,
    ) -> Result<BacklogEntry, StorageError> {
        Err(StorageError::Other(
            "backlog recovery not implemented".into(),
        ))
    }

    async fn backlog_record_complete(
        &self,
        _repository: &RepositoryIdentity,
        _discovered: &[DiscoveredPrd],
        _target: &DiscoveredPrd,
        _actor: &str,
        _reason: &str,
    ) -> Result<BacklogEntry, StorageError> {
        Err(StorageError::Other(
            "backlog record-complete not implemented".into(),
        ))
    }

    async fn bootstrap_rollback(
        &self,
        _repository: &RepositoryIdentity,
        _discovered: &[DiscoveredPrd],
        _run_id: &str,
        _actor: &str,
        _reason: &str,
    ) -> Result<BootstrapRollbackResult, StorageError> {
        Err(StorageError::Other(
            "bootstrap rollback not implemented".into(),
        ))
    }
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
    async fn usage_series(
        &self,
        request: &UsageSeriesRequest,
    ) -> Result<Vec<UsageSeriesPoint>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::AccountingRepository::new(db.conn())
            .usage_series(request)
            .map_err(map_err)
    }
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

    async fn list_backlog(
        &self,
        repository_key: &str,
        status: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BacklogEntryRow>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::list_backlog_entries(db.conn(), repository_key, status, after, limit)
            .map_err(map_err)
    }

    async fn list_driver_sessions(
        &self,
        repository_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DriverSession>, StorageError> {
        let db = self.db.lock().unwrap();
        DriverRepository::new(db.conn())
            .list_sessions_by_repository(repository_key, after, limit)
            .map_err(map_err)
    }

    async fn list_driver_attempts(
        &self,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Vec<DriverAttempt>, StorageError> {
        let db = self.db.lock().unwrap();
        DriverRepository::new(db.conn())
            .attempts_page(session_id, after, limit)
            .map_err(map_err)
    }

    async fn get_driver_session(
        &self,
        session_id: &str,
    ) -> Result<Option<DriverSession>, StorageError> {
        let db = self.db.lock().unwrap();
        DriverRepository::new(db.conn())
            .get_session(session_id)
            .map_err(map_err)
    }

    async fn list_checkpoints(
        &self,
        repository_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExecutionCheckpoint>, StorageError> {
        let db = self.db.lock().unwrap();
        CheckpointRepository::new(db.conn())
            .page(repository_key, after, limit)
            .map_err(map_err)
    }

    async fn list_recovery_events(
        &self,
        repository_key: &str,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Vec<RecoveryEventRow>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::list_recovery_events(db.conn(), repository_key, after, limit)
            .map_err(map_err)
    }

    async fn list_delivery_decisions(
        &self,
        repository_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DeliveryDecisionRow>, StorageError> {
        let db = self.db.lock().unwrap();
        DeliveryRepository::new(db.conn())
            .list_decisions(repository_key, after, limit)
            .map_err(map_err)
    }

    async fn get_budget_summary(
        &self,
        session_id: &str,
    ) -> Result<Option<BudgetSummary>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::budget_summary(db.conn(), session_id).map_err(map_err)
    }

    async fn list_review_findings(
        &self,
        repository_key: &str,
        session_id: &str,
    ) -> Result<Vec<ReviewFindingsRow>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::review_findings_for_session(db.conn(), repository_key, session_id)
            .map_err(map_err)
    }

    async fn list_pending_human_gates(
        &self,
        repository_key: &str,
        limit: usize,
    ) -> Result<Vec<PendingGate>, StorageError> {
        let db = self.db.lock().unwrap();
        familiar_ai_storage::pending_human_gates(db.conn(), repository_key, limit).map_err(map_err)
    }

    async fn backlog_recover(
        &self,
        repository: &RepositoryIdentity,
        target: &DiscoveredPrd,
        action: BacklogRecoveryAction,
        actor: &str,
        reason: &str,
    ) -> Result<BacklogEntry, StorageError> {
        let mut db = self.db.lock().unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .recover(repository, target, action, actor, reason)
            .map_err(map_err)
    }

    async fn backlog_record_complete(
        &self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        target: &DiscoveredPrd,
        actor: &str,
        reason: &str,
    ) -> Result<BacklogEntry, StorageError> {
        let mut db = self.db.lock().unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(repository, discovered)
            .map_err(map_err)?;
        SqliteBacklogRepository::new(db.conn_mut())
            .record_complete(repository, discovered, target, actor, reason)
            .map_err(map_err)
    }

    async fn bootstrap_rollback(
        &self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        run_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<BootstrapRollbackResult, StorageError> {
        let mut db = self.db.lock().unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(repository, discovered)
            .map_err(map_err)?;
        SqliteBootstrapRepository::new(db.conn_mut())
            .rollback(repository, discovered, run_id, actor, reason)
            .map_err(map_err)
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
