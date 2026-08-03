# PRD-004: System Tray

## Overview

Minimal system tray integration using `tray-icon` + `muda`. Provides status display and quick toggles.

## Depends On

- PRD-001: Daemon skeleton
- PRD-002: SQLite (project list)

## Scope

- System tray icon with menu
- Menu items: Enable/Disable LLM, Pause Tasks, Recent Projects, Settings (opens config file), About, Quit
- Status: active project count, LLM state, MCP state
- Runs on main thread (tray-icon requirement), daemon on tokio runtime
- Cross-platform: Linux + macOS
