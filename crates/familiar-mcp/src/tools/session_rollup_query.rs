//! Shared logic for the session-rollup retrieval tools.
//! Used by both `context.get_session_rollups` (canonical) and
//! `context.get_recent_changes` (alias for now).

use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{ToolContext, ToolError};

pub const HARD_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
pub struct Args {
    #[serde(default)]
    pub project_id: Option<i64>,
    #[serde(default)]
    pub repo_root: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn fetch_session_rollups(args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
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

    let default_limit = ctx.config.rollup.default_limit;
    let limit = parsed.limit.unwrap_or(default_limit).clamp(1, HARD_LIMIT);

    let rollups = ctx
        .storage
        .list_session_rollups_by_project(project_id, limit)
        .await
        .map_err(|e| ToolError::Internal(e.to_string()))?;

    Ok(json!({
        "project_id": project_id,
        "returned_rollups": rollups.len(),
        "rollups": rollups,
    }))
}

pub fn input_schema_value() -> Value {
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
