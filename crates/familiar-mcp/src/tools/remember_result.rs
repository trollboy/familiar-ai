use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use familiar_core::models::NewSessionRollup;
use familiar_tokens::{estimate_tokens, truncate_to_tokens};

use crate::tool::{Tool, ToolContext, ToolError};

// TODO: Support `repo_root` as an alternative to `project_id`. Many callers
// will know the repo path rather than the DB ID. Mirror the dual-arg pattern
// from get_recent_decisions when implemented.

pub const MAX_RELATED_FILES: usize = 256;
pub const MAX_NEXT_STEPS: usize = 64;
pub const MAX_PATH_LEN: usize = 1024;
pub const MAX_STEP_LEN: usize = 1024;

#[derive(Debug, Deserialize)]
struct Args {
    project_id: i64,
    summary: String,
    #[serde(default)]
    related_files: Vec<String>,
    #[serde(default)]
    next_steps: Vec<String>,
}

pub struct RememberResultTool;

#[async_trait]
impl Tool for RememberResultTool {
    fn name(&self) -> &'static str {
        "context.remember_result"
    }

    fn description(&self) -> &'static str {
        "Stores a session rollup (summary + related files + next steps) for a project."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "summary": {"type": "string"},
                "related_files": {"type": "array", "items": {"type": "string"}},
                "next_steps": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["project_id", "summary"],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        let max_chars = ctx.config.rollup.max_rollup_chars;
        let max_tokens = ctx.config.rollup.max_rollup_tokens;

        // Hard absolute ceiling — reject summaries beyond this
        if parsed.summary.chars().count() > max_chars {
            return Err(ToolError::InvalidParams(format!(
                "summary exceeds hard ceiling: {} > {max_chars}",
                parsed.summary.chars().count()
            )));
        }

        // Per-list/per-string limits remain
        if parsed.related_files.len() > MAX_RELATED_FILES {
            return Err(ToolError::InvalidParams(format!(
                "too many related_files: {} > {MAX_RELATED_FILES}",
                parsed.related_files.len()
            )));
        }
        if parsed.next_steps.len() > MAX_NEXT_STEPS {
            return Err(ToolError::InvalidParams(format!(
                "too many next_steps: {} > {MAX_NEXT_STEPS}",
                parsed.next_steps.len()
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
        for s in &parsed.next_steps {
            if s.len() > MAX_STEP_LEN {
                return Err(ToolError::InvalidParams(format!(
                    "next_step too long: {} > {MAX_STEP_LEN}",
                    s.len()
                )));
            }
        }

        // Token-aware truncation: combine all text with newline separators so
        // the estimate isn't artificially low. Truncate the summary if needed.
        let combined = format!(
            "{}\n{}\n{}",
            parsed.summary,
            parsed.related_files.join("\n"),
            parsed.next_steps.join("\n"),
        );
        let total_tokens = estimate_tokens(&combined);

        let final_summary = if total_tokens > max_tokens {
            // Reserve token budget for related_files and next_steps; truncate
            // summary down to fit the rest.
            let lists_text = format!(
                "{}\n{}",
                parsed.related_files.join("\n"),
                parsed.next_steps.join("\n"),
            );
            let lists_tokens = estimate_tokens(&lists_text);
            let summary_budget = max_tokens.saturating_sub(lists_tokens).max(1);
            let (truncated, was_truncated) = truncate_to_tokens(&parsed.summary, summary_budget);
            if was_truncated {
                tracing::info!(
                    original_tokens = total_tokens,
                    max_tokens = max_tokens,
                    summary_budget = summary_budget,
                    "truncated rollup summary to fit token budget"
                );
            }
            truncated
        } else {
            parsed.summary
        };

        let new_rollup = NewSessionRollup {
            project_id: parsed.project_id,
            summary: final_summary,
            related_files: parsed.related_files,
            next_steps: parsed.next_steps,
        };

        let rollup = ctx
            .storage
            .create_session_rollup(&new_rollup)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        Ok(json!({ "rollup": rollup }))
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
    async fn create_rollup() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "did stuff",
                    "related_files": ["src/a.rs"],
                    "next_steps": ["write tests"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["rollup"]["summary"], "did stuff");
    }

    #[tokio::test]
    async fn rejects_summary_beyond_hard_ceiling() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let max = ctx.config.rollup.max_rollup_chars;
        let big = "x".repeat(max + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": big,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn truncates_summary_when_over_token_budget() {
        let (mut ctx_inner, pid) = make_ctx_with_project();
        // Set a small token budget to force truncation
        let mut config = Config::default();
        config.rollup.max_rollup_tokens = 20;
        config.rollup.max_rollup_chars = 50_000;
        ctx_inner.config = Arc::new(config);

        let tool = RememberResultTool;
        // Create a summary that's well over 20 tokens (~80 chars)
        let big_summary =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega"
                .to_string();
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": big_summary.clone(),
                }),
                &ctx_inner,
            )
            .await
            .unwrap();

        let stored_summary = result["rollup"]["summary"].as_str().unwrap();
        assert!(stored_summary.contains("[truncated]"));
        assert!(stored_summary.len() < big_summary.len());
    }

    #[tokio::test]
    async fn fitting_rollup_stored_verbatim() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "small enough to fit",
                    "related_files": ["a.rs"],
                    "next_steps": ["test it"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["rollup"]["summary"], "small enough to fit");
        assert!(!result["rollup"]["summary"]
            .as_str()
            .unwrap()
            .contains("[truncated]"));
    }

    #[tokio::test]
    async fn rejects_too_many_related_files() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let files: Vec<String> = (0..MAX_RELATED_FILES + 1)
            .map(|i| format!("f{i}"))
            .collect();
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "x",
                    "related_files": files,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_too_many_next_steps() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let steps: Vec<String> = (0..MAX_NEXT_STEPS + 1).map(|i| format!("s{i}")).collect();
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "x",
                    "next_steps": steps,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_long_path() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let long_path = "x".repeat(MAX_PATH_LEN + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "x",
                    "related_files": [long_path],
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_long_step() {
        let (ctx, pid) = make_ctx_with_project();
        let tool = RememberResultTool;
        let long_step = "x".repeat(MAX_STEP_LEN + 1);
        let result = tool
            .call(
                json!({
                    "project_id": pid,
                    "summary": "x",
                    "next_steps": [long_step],
                }),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn missing_required_fields_errors() {
        let (ctx, _) = make_ctx_with_project();
        let tool = RememberResultTool;
        let result = tool.call(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }
}
