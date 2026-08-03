use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::meta::{MCP_PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION};
use crate::protocol::{JsonRpcError, INVALID_PARAMS};

#[derive(Debug, Clone, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default, rename = "clientInfo")]
    pub client_info: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "serverInfo")]
    pub server_info: Value,
    pub capabilities: Value,
}

pub fn handle_initialize(params: Value) -> Result<InitializeResult, JsonRpcError> {
    let parsed: InitializeParams = serde_json::from_value(params).map_err(|e| {
        JsonRpcError::new(INVALID_PARAMS, format!("invalid initialize params: {e}"))
    })?;

    if parsed.protocol_version != MCP_PROTOCOL_VERSION {
        return Err(JsonRpcError::with_data(
            INVALID_PARAMS,
            format!("unsupported protocol version: {}", parsed.protocol_version),
            json!({
                "supportedProtocolVersion": MCP_PROTOCOL_VERSION,
            }),
        ));
    }

    Ok(InitializeResult {
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        server_info: json!({
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        }),
        capabilities: json!({
            "tools": {},
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_success() {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        });
        let result = handle_initialize(params).unwrap();
        assert_eq!(result.protocol_version, MCP_PROTOCOL_VERSION);
        assert_eq!(result.server_info["name"], SERVER_NAME);
        assert_eq!(result.server_info["version"], SERVER_VERSION);
        assert!(result.capabilities["tools"].is_object());
    }

    #[test]
    fn initialize_version_mismatch_returns_structured_error() {
        let params = json!({
            "protocolVersion": "1999-01-01",
            "capabilities": {},
        });
        let err = handle_initialize(params).unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.data.is_some());
        let data = err.data.unwrap();
        assert_eq!(data["supportedProtocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn initialize_missing_version_errors() {
        let params = json!({"capabilities": {}});
        let err = handle_initialize(params).unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }
}
