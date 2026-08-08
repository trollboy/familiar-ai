# PRD-003: File Watcher + Repo Detection

## Overview

Watch filesystem for changes, detect Git repositories, and auto-register projects. Feed file change events into the daemon event loop.

## Depends On

- PRD-001: Daemon skeleton
- PRD-002: SQLite + project storage

## Scope

- File watcher using `notify` crate
- Git repository detection (find `.git` directories)
- Project auto-detection from cwd / watched paths
- Debounced change events
- Configurable ignore patterns (respect `.gitignore` + custom)
- Event channel integration with daemon loop
