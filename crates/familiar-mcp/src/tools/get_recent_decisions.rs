use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolContext, ToolError};

const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(default)]
    project_id: Option<i64>,
    #[serde(default)]
    repo_root: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct GetRecentDecisionsTool;

#[async_trait]
impl Tool for GetRecentDecisionsTool {
    fn name(&self) -> &'static str {
        "context.get_recent_decisions"
    }

    fn description(&self) -> &'static str {
        "Returns the most recent decisions for a project (by project_id or repo_root)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "repo_root": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
            },
            "anyOf": [
                {"required": ["project_id"]},
                {"required": ["repo_root"]},
            ],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        let project_id = match (parsed.project_id, parsed.repo_root.as_deref()) {
            (Some(id), _) => id,
            (None, Some(repo)) => {
                let project = ctx
                    .storage
                    .get_project_by_repo_root(repo)
                    .await
                    .map_err(|e| ToolError::Internal(e.to_string()))?
                    .ok_or_else(|| {
                        ToolError::InvalidParams(format!("no project found for repo_root: {repo}"))
                    })?;
                project.id
            }
            (None, None) => {
                return Err(ToolError::InvalidParams(
                    "must provide project_id or repo_root".into(),
                ));
            }
        };

        let limit = parsed.limit.unwrap_or(DEFAULT_LIMIT);
        let decisions = ctx
            .storage
            .list_decisions_by_project(project_id, limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        Ok(json!({ "decisions": decisions }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_core::config::Config;
    use familiar_core::models::{NewDecision, NewProject};
    use familiar_core::AppStatus;
    use familiar_storage::{Database, DecisionRepository, ProjectRepository};
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_data() -> (ToolContext, i64, String) {
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

        for i in 0..3 {
            db.create_decision(&NewDecision {
                project_id: pid,
                title: format!("d{i}"),
                summary: "s".into(),
                related_files: vec![],
                source_session: None,
                confidence: None,
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
        let (ctx, pid, _) = make_ctx_with_data();
        let tool = GetRecentDecisionsTool;
        let result = tool
            .call(json!({"project_id": pid, "limit": 10}), &ctx)
            .await
            .unwrap();
        let arr = result["decisions"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[tokio::test]
    async fn by_repo_root() {
        let (ctx, _, repo) = make_ctx_with_data();
        let tool = GetRecentDecisionsTool;
        let result = tool.call(json!({"repo_root": repo}), &ctx).await.unwrap();
        let arr = result["decisions"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[tokio::test]
    async fn missing_both_errors() {
        let (ctx, _, _) = make_ctx_with_data();
        let tool = GetRecentDecisionsTool;
        let result = tool.call(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn unknown_repo_root_errors() {
        let (ctx, _, _) = make_ctx_with_data();
        let tool = GetRecentDecisionsTool;
        let result = tool.call(json!({"repo_root": "/nope"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn limit_pushed_into_storage() {
        let (ctx, pid, _) = make_ctx_with_data();
        let tool = GetRecentDecisionsTool;
        let result = tool
            .call(json!({"project_id": pid, "limit": 2}), &ctx)
            .await
            .unwrap();
        let arr = result["decisions"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }
}
