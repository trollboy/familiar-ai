# PRD-006: Summaries + Decision Log

## Overview

File/module/directory summary generation and decision logging. Wire up MCP tools for retrieval.

## Depends On

- PRD-002: SQLite schema
- PRD-003: File watcher (triggers)
- PRD-005: MCP server (tool implementations)

## Scope

- Summary generation (initially from file metadata + structure, LLM-backed later)
- File, module, and directory level summaries
- Decision CRUD with structured records
- Wire up: context.get_file_summary, context.get_module_summary, context.get_recent_decisions
- Configurable summary staleness threshold
