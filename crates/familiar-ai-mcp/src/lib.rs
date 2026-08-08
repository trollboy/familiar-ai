//! MCP (Model Context Protocol) server for Familiar.
//!
//! Implements a JSON-RPC 2.0 server over Content-Length-framed stdio,
//! targeting MCP protocol version 2024-11-05.

pub mod handshake;
pub mod meta;
pub mod protocol;
pub mod server;
pub mod storage;
pub mod tool;
pub mod tools;
pub mod transport;

pub use server::McpServer;
pub use storage::{SqliteStorage, Storage, StorageError};
pub use tool::{Tool, ToolContext, ToolError, ToolRegistry};
pub use transport::{StdioTransport, Transport, TransportEvent};
