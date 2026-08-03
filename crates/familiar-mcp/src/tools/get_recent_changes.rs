use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::session_rollup_query::{fetch_session_rollups, input_schema_value};

// TODO: Currently aliased to context.get_session_rollups. True filesystem-change
// retrieval (recent FileChanged events from the watcher with timestamps) may be
// added in a future PRD via an in-memory ring buffer or a recent_changes table.

pub struct GetRecentChangesTool;

#[async_trait]
impl Tool for GetRecentChangesTool {
    fn name(&self) -> &'static str {
        "context.get_recent_changes"
    }

    fn description(&self) -> &'static str {
        "Returns recent session rollups for a project. \
         Currently aliased to context.get_session_rollups; \
         true filesystem-change retrieval may come in a future PRD."
    }

    fn input_schema(&self) -> Value {
        input_schema_value()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        fetch_session_rollups(args, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use crate::tools::get_session_rollups::GetSessionRollupsTool;
    use familiar_core::config::Config;
    use familiar_core::models::{NewProject, NewSessionRollup};
    use familiar_core::AppStatus;
    use familiar_storage::{Database, ProjectRepository, SessionRollupRepository};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_rollups(count: usize) -> (ToolContext, i64) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: "/test".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;
        for i in 0..count {
            db.create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: format!("rollup {i}"),
                related_files: vec![],
                next_steps: vec![],
            })
            .unwrap();
        }
        let arc_db = Arc::new(Mutex::new(db));
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(arc_db));
        let ctx = ToolContext {
            storage,
            status: Arc::new(Mutex::new(AppStatus::new())),
            config: Arc::new(Config::default()),
            router: None,
        };
        (ctx, pid)
    }

    #[tokio::test]
    async fn alias_returns_rollups() {
        let (ctx, pid) = make_ctx_with_rollups(3);
        let tool = GetRecentChangesTool;
        let result = tool.call(json!({"project_id": pid}), &ctx).await.unwrap();
        assert_eq!(result["returned_rollups"], 3);
    }

    #[tokio::test]
    async fn alias_matches_canonical_tool() {
        let (ctx, pid) = make_ctx_with_rollups(2);
        let canonical = GetSessionRollupsTool;
        let alias = GetRecentChangesTool;
        let args = json!({"project_id": pid});
        let canonical_result = canonical.call(args.clone(), &ctx).await.unwrap();
        let alias_result = alias.call(args, &ctx).await.unwrap();
        assert_eq!(canonical_result, alias_result);
    }
}
