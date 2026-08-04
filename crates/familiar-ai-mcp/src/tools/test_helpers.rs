//! Shared test helpers for tool tests. Only compiled in test builds.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use familiar_ai_core::config::Config;
use familiar_ai_core::models::{
    Decision, FileSummary, NewDecision, NewFileSummary, NewSessionRollup, Project, SessionRollup,
};
use familiar_ai_core::AppStatus;

use crate::storage::{Storage, StorageError};
use crate::tool::ToolContext;

pub struct DummyStorage;

#[async_trait]
impl Storage for DummyStorage {
    async fn list_decisions_by_project(
        &self,
        _: i64,
        _: usize,
    ) -> Result<Vec<Decision>, StorageError> {
        Ok(vec![])
    }
    async fn get_project_by_id(&self, _: i64) -> Result<Option<Project>, StorageError> {
        Ok(None)
    }
    async fn get_project_by_repo_root(&self, _: &str) -> Result<Option<Project>, StorageError> {
        Ok(None)
    }
    async fn list_active_projects(&self) -> Result<Vec<Project>, StorageError> {
        Ok(vec![])
    }
    async fn create_session_rollup(
        &self,
        _: &NewSessionRollup,
    ) -> Result<SessionRollup, StorageError> {
        Err(StorageError::Other("dummy".into()))
    }
    async fn list_session_rollups_by_project(
        &self,
        _: i64,
        _: usize,
    ) -> Result<Vec<SessionRollup>, StorageError> {
        Ok(vec![])
    }
    async fn create_decision(&self, _: &NewDecision) -> Result<Decision, StorageError> {
        Err(StorageError::Other("dummy".into()))
    }
    async fn get_file_summary(&self, _: i64, _: &str) -> Result<Option<FileSummary>, StorageError> {
        Ok(None)
    }
    async fn list_file_summaries_under(
        &self,
        _: i64,
        _: &str,
        _: usize,
    ) -> Result<Vec<FileSummary>, StorageError> {
        Ok(vec![])
    }
    async fn count_file_summaries_under(&self, _: i64, _: &str) -> Result<usize, StorageError> {
        Ok(0)
    }
    async fn search_file_summaries(
        &self,
        _: i64,
        _: &str,
        _: usize,
    ) -> Result<Vec<FileSummary>, StorageError> {
        Ok(vec![])
    }
    async fn search_decisions(
        &self,
        _: i64,
        _: &str,
        _: usize,
    ) -> Result<Vec<Decision>, StorageError> {
        Ok(vec![])
    }
    async fn create_or_update_file_summary(
        &self,
        _: &NewFileSummary,
    ) -> Result<FileSummary, StorageError> {
        Err(StorageError::Other("dummy".into()))
    }
}

pub fn dummy_ctx() -> ToolContext {
    ToolContext {
        storage: Arc::new(DummyStorage),
        status: Arc::new(Mutex::new(AppStatus::new())),
        config: Arc::new(Config::default()),
        router: None,
    }
}
