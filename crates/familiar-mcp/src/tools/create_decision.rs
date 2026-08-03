use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use familiar_core::models::NewDecision;

use crate::tool::{Tool, ToolContext, ToolError};

pub const MAX_TITLE_LEN: usize = 256;
pub const MAX_SUMMARY_LEN: usize = 16 * 1024;
pub const MAX_RELATED_FILES: usize = 256;
pub const MAX_PATH_LEN: usize = 1024;

pub const ALLOWED_CONFIDENCE: &[&str] = &["low", "medium", "high", "unknown"];

#[derive(Debug, Deserialize)]
struct Args {
    project_id: i64,
    title: String,
    summary: String,
    #[serde(default)]
    related_files: Vec<String>,
    #[serde(default)]
    source_session: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
}

pub struct CreateDecisionTool;

#[async_trait]
impl Tool for CreateDecisionTool {
    fn name(&self) -> &'static str {
        "context.create_decision"
    }

    fn description(&self) -> &'static str {
        "Records a decision (title + summary + related files + optional confidence) for a project."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "title": {"type": "string"},
                "summary": {"type": "string"},
                "related_files": {"type": "array", "items": {"type": "string"}},
                "source_session": {"type": "string"},
                "confidence": {"type": "string", "enum": ["low", "medium", "high", "unknown"]},
            },
            "required": ["project_id", "title", "summary"],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        if parsed.title.len() > MAX_TITLE_LEN {
            return Err(ToolError::InvalidParams(format!(
                "title too long: {} > {MAX_TITLE_LEN}",
                parsed.title.len()
            )));
        }
        if parsed.summary.len() > MAX_SUMMARY_LEN {
            return Err(ToolError::InvalidParams(format!(
                "summary too long: {} > {MAX_SUMMARY_LEN}",
                parsed.summary.len()
            )));
        }
        if parsed.related_files.len() > MAX_RELATED_FILES {
            return Err(ToolError::InvalidParams(format!(
                "too many related_files: {} > {MAX_RELATED_FILES}",
                parsed.related_files.len()
            )));
        }
        for f in &parsed.related_files {
            if f.len() > MAX_PATH_LEN {
                return Err(ToolError::InvalidParams(format!(
                    "related_file too long: {} > {MAX_PATH_LEN}",
                    f.len()
                )));
            }
        }

        // Normalize and validate confidence
        let normalized_confidence = match parsed.confidence {
            Some(c) => {
                let lower = c.to_lowercase();
                if !ALLOWED_CONFIDENCE.contains(&lower.as_str()) {
                    return Err(ToolError::InvalidParams(format!(
                        "invalid confidence value '{c}': must be one of {ALLOWED_CONFIDENCE:?}"
                    )));
                }
                Some(lower)
            }
            None => None,
        };

        let new_decision = NewDecision {
            project_id: parsed.project_id,
            title: parsed.title,
            summary: parsed.summary,
            related_files: parsed.related_files,
            source_session: parsed.source_session,
            confidence: normalized_confidence,
        };

        let decision = ctx
            .storage
            .create_decision(&new_decision)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        Ok(json!({ "decision": decision }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_core::config::Config;
    use familiar_core::models::NewProject;
    use familiar_core::AppStatus;
    use familiar_storage::{Database, ProjectRepository};
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_project() -> (ToolContext, i64) {
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
    async fn creates_decision_with_confidence() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "Use SQLite",
                    "summary": "Simple and reliable",
                    "confidence": "high",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["decision"]["title"], "Use SQLite");
        assert_eq!(result["decision"]["confidence"], "high");
    }

    #[tokio::test]
    async fn confidence_normalized_to_lowercase() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "Test",
                    "summary": "x",
                    "confidence": "HIGH",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["decision"]["confidence"], "high");

        let result2 = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "Test 2",
                    "summary": "x",
                    "confidence": "Medium",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result2["decision"]["confidence"], "medium");
    }

    #[tokio::test]
    async fn rejects_unknown_confidence() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "x",
                    "summary": "x",
                    "confidence": "extremely-confident",
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn confidence_optional() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "x",
                    "summary": "x",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result["decision"]["confidence"].is_null());
    }

    #[tokio::test]
    async fn rejects_oversized_title() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let big_title = "x".repeat(MAX_TITLE_LEN + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": big_title,
                    "summary": "x",
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_oversized_summary() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let big_summary = "x".repeat(MAX_SUMMARY_LEN + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "x",
                    "summary": big_summary,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_too_many_related_files() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let many: Vec<String> = (0..MAX_RELATED_FILES + 1)
            .map(|i| format!("f{i}"))
            .collect();
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "x",
                    "summary": "x",
                    "related_files": many,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_long_related_file_path() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = CreateDecisionTool;
        let long_path = "x".repeat(MAX_PATH_LEN + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "title": "x",
                    "summary": "x",
                    "related_files": [long_path],
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }
}
