use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use familiar_ai_core::config::Config;
use familiar_ai_core::AppStatus;
use familiar_ai_llm::InferenceRouter;

use crate::storage::Storage;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

pub struct ToolContext {
    pub storage: Arc<dyn Storage>,
    pub status: Arc<Mutex<AppStatus>>,
    pub config: Arc<Config>,
    /// LLM manager for health reporting. `None` when the MCP binary is
    /// running without an LLM configured (or when failed to construct).
    ///
    /// Known limitation: each process owns its own manager. The MCP binary
    /// spawned later by Claude Code does not share runtime state with the
    /// daemon. See InferenceRouter docs for rationale.
    pub router: Option<Arc<InferenceRouter>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn list(&self) -> Vec<ToolDescriptor> {
        let mut out: Vec<ToolDescriptor> = self
            .tools
            .values()
            .map(|t| ToolDescriptor {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub async fn call(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext,
    ) -> Result<Value, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::InvalidParams(format!("unknown tool: {name}")))?
            .clone();
        tool.call(args, ctx).await
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for stub tools to return a structured "not implemented" response.
pub fn not_implemented_value(message: &str) -> Value {
    json!({
        "implemented": false,
        "message": message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "echoes input"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn call(&self, args: Value, _ctx: &ToolContext) -> Result<Value, ToolError> {
            Ok(args)
        }
    }

    fn make_ctx() -> ToolContext {
        // Construct a minimal ToolContext using a mock storage
        struct DummyStorage;
        #[async_trait]
        impl Storage for DummyStorage {
            async fn list_decisions_by_project(
                &self,
                _: i64,
                _: usize,
            ) -> Result<Vec<familiar_ai_core::models::Decision>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn get_project_by_id(
                &self,
                _: i64,
            ) -> Result<Option<familiar_ai_core::models::Project>, crate::storage::StorageError>
            {
                Ok(None)
            }
            async fn get_project_by_repo_root(
                &self,
                _: &str,
            ) -> Result<Option<familiar_ai_core::models::Project>, crate::storage::StorageError>
            {
                Ok(None)
            }
            async fn list_active_projects(
                &self,
            ) -> Result<Vec<familiar_ai_core::models::Project>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn create_session_rollup(
                &self,
                _: &familiar_ai_core::models::NewSessionRollup,
            ) -> Result<familiar_ai_core::models::SessionRollup, crate::storage::StorageError>
            {
                Err(crate::storage::StorageError::Other("dummy".into()))
            }
            async fn list_session_rollups_by_project(
                &self,
                _: i64,
                _: usize,
            ) -> Result<Vec<familiar_ai_core::models::SessionRollup>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn create_decision(
                &self,
                _: &familiar_ai_core::models::NewDecision,
            ) -> Result<familiar_ai_core::models::Decision, crate::storage::StorageError>
            {
                Err(crate::storage::StorageError::Other("dummy".into()))
            }
            async fn get_file_summary(
                &self,
                _: i64,
                _: &str,
            ) -> Result<Option<familiar_ai_core::models::FileSummary>, crate::storage::StorageError>
            {
                Ok(None)
            }
            async fn list_file_summaries_under(
                &self,
                _: i64,
                _: &str,
                _: usize,
            ) -> Result<Vec<familiar_ai_core::models::FileSummary>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn count_file_summaries_under(
                &self,
                _: i64,
                _: &str,
            ) -> Result<usize, crate::storage::StorageError> {
                Ok(0)
            }
            async fn search_file_summaries(
                &self,
                _: i64,
                _: &str,
                _: usize,
            ) -> Result<Vec<familiar_ai_core::models::FileSummary>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn search_decisions(
                &self,
                _: i64,
                _: &str,
                _: usize,
            ) -> Result<Vec<familiar_ai_core::models::Decision>, crate::storage::StorageError>
            {
                Ok(vec![])
            }
            async fn create_or_update_file_summary(
                &self,
                _: &familiar_ai_core::models::NewFileSummary,
            ) -> Result<familiar_ai_core::models::FileSummary, crate::storage::StorageError>
            {
                Err(crate::storage::StorageError::Other("dummy".into()))
            }
        }

        ToolContext {
            storage: Arc::new(DummyStorage),
            status: Arc::new(Mutex::new(AppStatus::new())),
            config: Arc::new(Config::default()),
            router: None,
        }
    }

    #[tokio::test]
    async fn register_and_call() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let ctx = make_ctx();
        let result = reg
            .call("echo", json!({"hello": "world"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result, json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let reg = ToolRegistry::new();
        let ctx = make_ctx();
        let result = reg.call("nope", json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[test]
    fn list_returns_descriptors() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "echo");
    }

    #[test]
    fn registry_len_and_empty() {
        let mut reg = ToolRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(EchoTool));
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
    }

    #[test]
    fn not_implemented_value_shape() {
        let v = not_implemented_value("test");
        assert_eq!(v["implemented"], false);
        assert_eq!(v["message"], "test");
    }
}
