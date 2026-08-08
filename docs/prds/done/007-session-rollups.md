# PRD-007: Session Rollups

## Overview

Compact long Claude conversations into reusable summaries. Store and retrieve via MCP.

## Depends On

- PRD-002: SQLite schema
- PRD-005: MCP server

## Scope

- Session rollup data model (summary, related files, next steps)
- Rollup creation via MCP tool (context.remember_result)
- Rollup retrieval via context.get_recent_changes
- Token-aware truncation
