use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::session_rollup_query::{fetch_session_rollups, input_schema_value};

pub struct GetSessionRollupsTool;

#[async_trait]
impl Tool for GetSessionRollupsTool {
    fn name(&self) -> &'static str {
        "context.get_session_rollups"
    }

    fn description(&self) -> &'static str {
        "Returns recent session rollups (compacted Claude conversations) for a project. \
         Accepts either project_id or repo_root."
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
    use familiar_ai_core::config::Config;
    use familiar_ai_core::models::{NewProject, NewSessionRollup};
    use familiar_ai_core::AppStatus;
    use familiar_ai_storage::{Database, ProjectRepository, SessionRollupRepository};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_rollups(count: usize) -> (ToolContext, i64, String) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();

        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: "/test/repo".into(),
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
        (ctx, pid, "/test/repo".to_string())
    }

    #[tokio::test]
    async fn by_project_id() {
        let (ctx, pid, _) = make_ctx_with_rollups(3);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({"project_id": pid}), &ctx).await.unwrap();
        assert_eq!(result["project_id"], pid);
        assert_eq!(result["returned_rollups"], 3);
        assert_eq!(result["rollups"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn by_repo_root() {
        let (ctx, _, repo) = make_ctx_with_rollups(2);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({"repo_root": repo}), &ctx).await.unwrap();
        assert_eq!(result["returned_rollups"], 2);
    }

    #[tokio::test]
    async fn missing_both_errors() {
        let (ctx, _, _) = make_ctx_with_rollups(1);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn unknown_repo_root_errors() {
        let (ctx, _, _) = make_ctx_with_rollups(1);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({"repo_root": "/nope"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn ordering_newest_first() {
        let (ctx, pid, _) = make_ctx_with_rollups(3);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({"project_id": pid}), &ctx).await.unwrap();
        let rollups = result["rollups"].as_array().unwrap();
        // Most recently inserted should be first
        assert_eq!(rollups[0]["summary"], "rollup 2");
        assert_eq!(rollups[2]["summary"], "rollup 0");
    }

    #[tokio::test]
    async fn limit_enforced() {
        let (ctx, pid, _) = make_ctx_with_rollups(10);
        let tool = GetSessionRollupsTool;
        let result = tool
            .call(json!({"project_id": pid, "limit": 5}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["returned_rollups"], 5);
    }

    #[tokio::test]
    async fn default_limit_applied() {
        // Default is 20; create 25 and verify only 20 returned
        let (ctx, pid, _) = make_ctx_with_rollups(25);
        let tool = GetSessionRollupsTool;
        let result = tool.call(json!({"project_id": pid}), &ctx).await.unwrap();
        assert_eq!(result["returned_rollups"], 20);
    }

    #[tokio::test]
    async fn limit_zero_clamps_to_one() {
        let (ctx, pid, _) = make_ctx_with_rollups(5);
        let tool = GetSessionRollupsTool;
        let result = tool
            .call(json!({"project_id": pid, "limit": 0}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["returned_rollups"], 1);
    }

    #[tokio::test]
    async fn limit_above_hard_ceiling_clamped() {
        let (ctx, pid, _) = make_ctx_with_rollups(5);
        let tool = GetSessionRollupsTool;
        // Request 9999 — should clamp to HARD_LIMIT (100), capped further by what exists (5)
        let result = tool
            .call(json!({"project_id": pid, "limit": 9999}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["returned_rollups"], 5);
    }
}
