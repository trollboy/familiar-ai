use std::sync::Arc;

use serde_json::{json, Value};

use familiar_core::FamiliarError;

use crate::handshake::handle_initialize;
use crate::protocol::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, INVALID_PARAMS, INVALID_REQUEST,
};
use crate::tool::{not_implemented_value, ToolContext, ToolError, ToolRegistry};
use crate::transport::Transport;

pub struct McpServer {
    transport: Box<dyn Transport>,
    registry: Arc<ToolRegistry>,
    context: Arc<ToolContext>,
    initialized: bool,
}

impl McpServer {
    pub fn new(
        transport: Box<dyn Transport>,
        registry: Arc<ToolRegistry>,
        context: Arc<ToolContext>,
    ) -> Self {
        Self {
            transport,
            registry,
            context,
            initialized: false,
        }
    }

    pub async fn run(mut self) -> Result<(), FamiliarError> {
        loop {
            let msg = match self.transport.read_message().await? {
                Some(m) => m,
                None => {
                    // Clean shutdown — client closed stdin
                    tracing::info!("transport closed, exiting cleanly");
                    return Ok(());
                }
            };

            // Parse JSON-RPC envelope
            let request: JsonRpcRequest = match serde_json::from_str(&msg) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse JSON-RPC request");
                    let resp = JsonRpcResponse::parse_error();
                    self.write_response(&resp).await?;
                    continue;
                }
            };

            let id = request.id.clone();
            let is_notification = request.is_notification();

            // Dispatch
            let result = self.dispatch(request).await;

            // Notifications get no response
            if is_notification {
                continue;
            }

            let response = match result {
                Ok(value) => JsonRpcResponse::success(id.unwrap_or(Value::Null), value),
                Err(err) => JsonRpcResponse::error(id.unwrap_or(Value::Null), err),
            };

            self.write_response(&response).await?;
        }
    }

    async fn dispatch(&mut self, request: JsonRpcRequest) -> Result<Value, JsonRpcError> {
        match request.method.as_str() {
            "initialize" => {
                let params = request.params.unwrap_or(Value::Null);
                let result = handle_initialize(params)?;
                self.initialized = true;
                serde_json::to_value(result).map_err(|e| {
                    JsonRpcError::internal_error(format!(
                        "failed to serialize initialize result: {e}"
                    ))
                })
            }
            "initialized" | "notifications/initialized" => {
                // Notification — just mark as initialized.
                self.initialized = true;
                Ok(Value::Null)
            }
            method => {
                if !self.initialized && method != "initialize" {
                    return Err(JsonRpcError::new(INVALID_REQUEST, "server not initialized"));
                }
                match method {
                    "tools/list" => Ok(json!({ "tools": self.registry.list() })),
                    "tools/call" => self.call_tool(request.params).await,
                    other => Err(JsonRpcError::method_not_found(other)),
                }
            }
        }
    }

    async fn call_tool(&self, params: Option<Value>) -> Result<Value, JsonRpcError> {
        let params = params
            .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "missing params for tools/call"))?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "missing tool name"))?
            .to_string();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        match self.registry.call(&name, arguments, &self.context).await {
            Ok(value) => Ok(json!({
                "content": [
                    {"type": "text", "text": value.to_string()}
                ],
                "isError": false,
                "structuredContent": value,
            })),
            Err(ToolError::NotImplemented(msg)) => {
                let value = not_implemented_value(&msg);
                Ok(json!({
                    "content": [
                        {"type": "text", "text": value.to_string()}
                    ],
                    "isError": false,
                    "structuredContent": value,
                }))
            }
            Err(ToolError::InvalidParams(msg)) => Err(JsonRpcError::new(INVALID_PARAMS, msg)),
            Err(ToolError::Internal(msg)) => Err(JsonRpcError::internal_error(msg)),
        }
    }

    async fn write_response(&mut self, response: &JsonRpcResponse) -> Result<(), FamiliarError> {
        let body = serde_json::to_string(response)
            .map_err(|e| FamiliarError::Mcp(format!("failed to serialize response: {e}")))?;
        self.transport.write_message(&body).await
    }
}
