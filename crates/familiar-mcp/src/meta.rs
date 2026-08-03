/// MCP protocol version we implement.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name reported in the initialize handshake.
pub const SERVER_NAME: &str = "familiar-mcp";

/// Server version reported in the initialize handshake.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
