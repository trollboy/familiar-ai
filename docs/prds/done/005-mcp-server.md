# PRD-005: MCP Server Skeleton

## Overview

JSON-RPC 2.0 MCP server over stdin/stdout transport. Small internal abstraction layer. Initial tool stubs.

## Depends On

- PRD-001: Daemon skeleton
- PRD-002: SQLite (data access)

## Scope

- JSON-RPC 2.0 implementation (serde_json)
- stdin/stdout transport
- MCP protocol handshake (initialize, initialized)
- Tool registration system
- Stub implementations for all 8 MCP tools
- Internal trait for swapping transport later
- Error responses per MCP spec
