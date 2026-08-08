# PRD-002: SQLite Schema + Project Isolation

## Overview

Add SQLite database with schema for projects, file summaries, decisions, and session rollups. Implement per-project memory isolation. All data access goes through a repository layer.

## Depends On

- PRD-001: Daemon skeleton, config, core crate

## Scope

- New crate: `familiar-db`
- SQLite via `rusqlite` (not an ORM)
- Migration system (embedded SQL files)
- Repository trait + SQLite implementation
- Project CRUD operations
- Per-project data isolation enforced at query level
- Database path in config
- Integration tests against real SQLite
