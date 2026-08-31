use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use familiar_ai_core::config::Config;
use familiar_ai_core::models::NewProject;
use familiar_ai_core::AppStatus;
use familiar_ai_mcp::storage::{SqliteStorage, Storage};
use familiar_ai_mcp::tool::{ToolContext, ToolRegistry};
use familiar_ai_mcp::tools::register_default_tools;
use familiar_ai_mcp::transport::MockTransport;
use familiar_ai_mcp::McpServer;
use familiar_ai_storage::{Database, ProjectRepository};

fn build_server(
    inbound: Vec<String>,
) -> (
    McpServer,
    std::sync::Arc<std::sync::Mutex<Vec<familiar_ai_mcp::TransportEvent>>>,
    i64,
) {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();

    let pid = db
        .create_project(&NewProject {
            name: "test".into(),
            repo_root: "/test/repo".into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap()
        .id;

    let arc_db = Arc::new(Mutex::new(db));
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(arc_db));

    let mut status = AppStatus::new();
    status.active_projects = 1;
    status.mcp_enabled = true;

    let context = Arc::new(ToolContext {
        storage,
        status: Arc::new(Mutex::new(status)),
        config: Arc::new(Config::default()),
        router: None,
    });

    let mut registry = ToolRegistry::new();
    register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    let transport = MockTransport::new(inbound);
    let events = transport.events_handle();
    let server = McpServer::new(Box::new(transport), registry, context);
    (server, events, pid)
}

fn parse_response(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[tokio::test]
async fn full_session_handshake_list_call() {
    let inbound = vec![
        // initialize
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1"}
            }
        })
        .to_string(),
        // initialized notification
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        })
        .to_string(),
        // tools/list
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        })
        .to_string(),
        // tools/call get_project_status
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "context.get_project_status",
                "arguments": {}
            }
        })
        .to_string(),
    ];

    let (server, events, _pid) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();

    // Should have 3 responses (initialize, tools/list, tools/call) — notification gets none
    assert_eq!(
        writes.len(),
        3,
        "expected 3 responses, got {}: {writes:?}",
        writes.len()
    );

    let init_resp = parse_response(writes[0]);
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "familiar-ai-mcp");

    let list_resp = parse_response(writes[1]);
    assert_eq!(list_resp["id"], 2);
    let tools = list_resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 25);
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "stewardship.usage_series"));

    let call_resp = parse_response(writes[2]);
    assert_eq!(call_resp["id"], 3);
    assert_eq!(call_resp["result"]["isError"], false);
    let structured = &call_resp["result"]["structuredContent"];
    assert_eq!(structured["active_projects"], 1);
    assert_eq!(structured["mcp_enabled"], true);
}

#[tokio::test]
async fn remember_result_then_get_recent_decisions_via_repo_root() {
    let inbound = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {}
            }
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "context.remember_result",
                "arguments": {
                    "project_id": 1,
                    "summary": "did stuff",
                    "related_files": ["src/a.rs"],
                    "next_steps": ["write tests"]
                }
            }
        })
        .to_string(),
        // Get recent decisions by repo_root (will be empty, but should resolve)
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "context.get_recent_decisions",
                "arguments": {
                    "repo_root": "/test/repo"
                }
            }
        })
        .to_string(),
    ];

    let (server, events, _pid) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(writes.len(), 3);

    let remember_resp = parse_response(&writes[1]);
    assert_eq!(remember_resp["id"], 2);
    assert_eq!(remember_resp["result"]["isError"], false);
    assert_eq!(
        remember_resp["result"]["structuredContent"]["rollup"]["summary"],
        "did stuff"
    );

    let decisions_resp = parse_response(&writes[2]);
    assert_eq!(decisions_resp["id"], 3);
    assert_eq!(decisions_resp["result"]["isError"], false);
    let arr = decisions_resp["result"]["structuredContent"]["decisions"]
        .as_array()
        .unwrap();
    assert_eq!(arr.len(), 0);
}

#[tokio::test]
async fn search_returns_results() {
    let inbound = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "context.search",
                "arguments": {"project_id": 1, "query": "test"}
            }
        })
        .to_string(),
    ];

    let (server, events, _) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    let resp = parse_response(&writes[1]);
    assert_eq!(resp["result"]["isError"], false);
    // Verify structured content has search result shape
    let sc = &resp["result"]["structuredContent"];
    assert!(sc["query"].is_string());
    assert!(sc["results"].is_array());
}

#[tokio::test]
async fn version_mismatch_returns_structured_error() {
    let inbound = vec![json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "1999-01-01", "capabilities": {}}
    })
    .to_string()];

    let (server, events, _) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    let resp = parse_response(&writes[0]);
    assert_eq!(resp["id"], 1);
    assert!(resp["error"].is_object());
    assert_eq!(
        resp["error"]["data"]["supportedProtocolVersion"],
        "2024-11-05"
    );
}

#[tokio::test]
async fn parse_error_response() {
    let inbound = vec!["not json at all".to_string()];

    let (server, events, _) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    let resp = parse_response(&writes[0]);
    assert_eq!(resp["error"]["code"], -32700);
}

#[tokio::test]
async fn method_before_initialize_errors() {
    let inbound = vec![json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    })
    .to_string()];

    let (server, events, _) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    let resp = parse_response(&writes[0]);
    assert!(resp["error"].is_object());
    assert_eq!(resp["error"]["code"], -32600);
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let inbound = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "nonexistent/method"
        })
        .to_string(),
    ];

    let (server, events, _) = build_server(inbound);
    server.run().await.unwrap();

    let events = events.lock().unwrap();
    let writes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            familiar_ai_mcp::TransportEvent::Write(s) => Some(s.clone()),
            _ => None,
        })
        .collect();

    let resp = parse_response(&writes[1]);
    assert_eq!(resp["error"]["code"], -32601);
}
