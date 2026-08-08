# PRD-009: Context Packer (pack_for_task)

## Overview

Assemble optimized context packs for Claude tasks within token budgets.

## Depends On

- PRD-002: SQLite (data)
- PRD-005: MCP server
- PRD-006: Summaries + decisions

## Scope

- Token counting (tiktoken-rs or simple estimator)
- Budget allocation across: task, files, decisions, history, architecture
- context.pack_for_task MCP tool implementation
- context.search MCP tool implementation
- context.get_project_status MCP tool implementation
- Configurable budget profiles (minimal, balanced, aggressive, max accuracy)
