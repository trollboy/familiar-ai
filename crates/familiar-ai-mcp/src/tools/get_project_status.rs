use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolContext, ToolError};

pub struct GetProjectStatusTool;

#[async_trait]
impl Tool for GetProjectStatusTool {
    fn name(&self) -> &'static str {
        "context.get_project_status"
    }

    fn description(&self) -> &'static str {
        "Returns the current Familiar daemon status including active project count and feature state."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let status = ctx.status.lock().unwrap().clone();
        let uptime_secs = (chrono::Utc::now() - status.startup_time).num_seconds();

        let inference = if let Some(ref router) = ctx.router {
            let h = router.health().await;
            json!({
                "text_mode": h.text_mode,
                "text_primary": h.text_primary,
                "text_fallback": h.text_fallback,
                "embedding_primary": h.embedding_primary,
                "embedding_fallback": h.embedding_fallback,
            })
        } else {
            json!({
                "text_mode": "disabled",
                "text_primary": null,
                "text_fallback": null,
                "embedding_primary": null,
                "embedding_fallback": null,
            })
        };

        let mut result = json!({
            "active_projects": status.active_projects,
            "local_llm_enabled": status.local_llm_enabled,
            "mcp_enabled": status.mcp_enabled,
            "startup_time": status.startup_time.to_rfc3339(),
            "uptime_secs": uptime_secs,
        });

        // Merge inference health fields into the response
        if let (Some(obj), Some(llm_obj)) = (result.as_object_mut(), inference.as_object()) {
            for (k, v) in llm_obj {
                obj.insert(k.clone(), v.clone());
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_helpers::DummyStorage;
    use familiar_ai_core::config::Config;
    use familiar_ai_core::AppStatus;
    use std::sync::{Arc, Mutex};

    fn ctx() -> ToolContext {
        let mut s = AppStatus::new();
        s.active_projects = 3;
        s.local_llm_enabled = true;
        s.mcp_enabled = true;
        ToolContext {
            storage: Arc::new(DummyStorage),
            status: Arc::new(Mutex::new(s)),
            config: Arc::new(Config::default()),
            router: None,
        }
    }

    #[tokio::test]
    async fn returns_status() {
        let tool = GetProjectStatusTool;
        let result = tool.call(json!({}), &ctx()).await.unwrap();
        assert_eq!(result["active_projects"], 3);
        assert_eq!(result["local_llm_enabled"], true);
        assert_eq!(result["mcp_enabled"], true);
        assert!(result["uptime_secs"].is_number());
    }
}
