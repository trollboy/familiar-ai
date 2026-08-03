use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolContext, ToolError};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

// TODO: future PRD — add relevance-based ordering for module summaries:
// deprioritize files tagged "test", prioritize files with more extracted
// symbols, files nearer the module root, recent mtimes. v1 returns files
// in path order, which produces junk results once repos get large.

#[derive(Debug, Deserialize)]
struct Args {
    project_id: i64,
    module_path: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct GetModuleSummaryTool;

#[async_trait]
impl Tool for GetModuleSummaryTool {
    fn name(&self) -> &'static str {
        "context.get_module_summary"
    }

    fn description(&self) -> &'static str {
        "Returns aggregated file summaries under a directory path. \
         Computed on-demand from file_summaries — no separate module table."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "module_path": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT},
            },
            "required": ["project_id", "module_path"],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        let limit = parsed.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

        // Normalize the prefix to end with '/' so "src" doesn't match "src_other".
        // Empty prefix is allowed and means "match everything in the project".
        let prefix = if parsed.module_path.is_empty() || parsed.module_path.ends_with('/') {
            parsed.module_path.clone()
        } else {
            format!("{}/", parsed.module_path)
        };

        let total = ctx
            .storage
            .count_file_summaries_under(parsed.project_id, &prefix)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let files = ctx
            .storage
            .list_file_summaries_under(parsed.project_id, &prefix, limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let returned = files.len();
        let truncated = total > returned;

        let file_views: Vec<Value> = files
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "summary": f.summary,
                    "tags": f.tags,
                    "extracted_symbols": f.extracted_symbols,
                })
            })
            .collect();

        Ok(json!({
            "module_path": parsed.module_path,
            "file_count": total,
            "returned_files": returned,
            "truncated": truncated,
            "files": file_views,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_core::config::Config;
    use familiar_core::models::{NewFileSummary, NewProject};
    use familiar_core::AppStatus;
    use familiar_storage::{Database, FileSummaryRepository, ProjectRepository};
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_files(paths: &[&str]) -> (ToolContext, i64) {
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

        for path in paths {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: (*path).into(),
                summary: format!("summary for {path}"),
                tags: vec!["rust".into(), "code".into()],
                extracted_symbols: vec!["foo".into()],
                last_known_mtime: None,
                last_known_size: None,
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
    async fn returns_files_under_module() {
        let (ctx, pid) =
            make_ctx_with_files(&["src/a.rs", "src/b.rs", "src/sub/c.rs", "tests/d.rs"]);
        let tool = GetModuleSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "module_path": "src"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["file_count"], 3);
        assert_eq!(result["returned_files"], 3);
        assert_eq!(result["truncated"], false);
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    async fn applies_default_limit_and_truncates() {
        let paths: Vec<String> = (0..150).map(|i| format!("src/f{i:03}.rs")).collect();
        let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        let (ctx, pid) = make_ctx_with_files(&path_refs);

        let tool = GetModuleSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "module_path": "src"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["file_count"], 150);
        assert_eq!(result["returned_files"], DEFAULT_LIMIT);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn explicit_limit_respected() {
        let (ctx, pid) = make_ctx_with_files(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let tool = GetModuleSummaryTool;
        let result = tool
            .call(
                json!({"project_id": pid, "module_path": "src", "limit": 2}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["file_count"], 3);
        assert_eq!(result["returned_files"], 2);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn empty_module_returns_zero() {
        let (ctx, pid) = make_ctx_with_files(&["src/a.rs"]);
        let tool = GetModuleSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "module_path": "nope"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["file_count"], 0);
        assert_eq!(result["returned_files"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn prefix_does_not_match_partial_dirname() {
        // "src" should not match "src_other"
        let (ctx, pid) = make_ctx_with_files(&["src/a.rs", "src_other/b.rs"]);
        let tool = GetModuleSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "module_path": "src"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["file_count"], 1);
    }

    #[tokio::test]
    async fn limit_capped_at_max() {
        let (ctx, pid) = make_ctx_with_files(&["src/a.rs"]);
        let tool = GetModuleSummaryTool;
        // Request limit beyond MAX_LIMIT — should be capped silently
        let result = tool
            .call(
                json!({"project_id": pid, "module_path": "src", "limit": 9999}),
                &ctx,
            )
            .await;
        assert!(result.is_ok());
    }
}
