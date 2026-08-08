# Familiar — Product Requirements Document

## 1. Overview

Familiar is a lightweight local companion daemon for Claude Code and similar developer tooling.

It runs quietly in the background, maintains per-project memory, summarizes large codebases and discussions, reduces repeated token spend, and exposes useful project context through MCP.

The goal is not to replace Claude Code.

The goal is to make Claude Code cheaper, faster, and less forgetful.

Familiar should:
- remember important project information locally
- compress large discussions and code context into reusable summaries
- maintain isolated memory per project/repository
- optionally use a small local LLM for preprocessing
- expose project memory and context packing through MCP
- minimize token waste when using Claude Code or other external models

Familiar should not:
- become a full IDE
- become a general-purpose vector database platform
- become a giant Electron application
- become a replacement for Git, Jira, docs, or issue tracking
- silently rewrite Claude responses in misleading ways

---

# 2. Core Goals

1. Reduce API token spend
2. Reduce repeated prompt bloat
3. Improve Claude response quality by providing better context
4. Preserve useful project knowledge between sessions
5. Keep the local footprint lightweight and unobtrusive
6. Support both macOS and Linux
7. Remain simple enough that it does not become a second full-time project

---

# 3. User Personas

## Persona A — Solo Developer

Works on several projects simultaneously.

Needs:
- project-specific memory
- automatic summaries
- lightweight local search
- reduced context repetition

## Persona B — Power User / Architect

Maintains large codebases, PRDs, infrastructure docs, and architectural discussions.

Needs:
- large-context summarization
- subsystem summaries
- decision logging
- branch-aware context
- architecture recall

## Persona C — Cost-Conscious Claude User

Uses Claude Code heavily and wants to reduce token spend.

Needs:
- token budgeting
- context compaction
- local preprocessing
- file filtering
- session rollups

---

# 4. Key Features

## 4.1 Background Daemon

Familiar runs as a lightweight background process.

Responsibilities:
- monitor repositories
- maintain local database
- update summaries
- expose MCP server
- manage local model lifecycle
- track token savings and cache statistics

The daemon must be able to run without the local model enabled.

## 4.2 System Tray Integration

Minimal systray UI.

Menu items:
- Enable Local LLM
- Disable Local LLM
- Pause Heavy Background Tasks
- Open Dashboard
- Recent Projects
- Settings
- About
- Quit

Status display:
- number of active projects
- current focused project
- local model state
- MCP state
- token savings estimate

## 4.3 Per-Project Memory Isolation

Every project maintains separate memory.

Per-project data includes:
- summaries
- decisions
- recent task history
- coding conventions
- architecture notes
- ignored paths
- token usage
- recent sessions

Cross-project leakage should be avoided unless explicitly requested.

## 4.4 File and Module Summaries

Familiar maintains summaries of:
- files
- modules
- directories
- services
- subsystems

Example summary:

```text
src/auth/token.rs
- owns JWT creation and validation
- depends on refresh token store
- used by middleware.rs and login.rs
- preserve mobile token payload shape
```

## 4.5 Decision Log

Familiar stores important decisions.

Examples:
- refresh tokens remain Redis-backed
- auth remains stateless
- PostgreSQL is source of truth
- keep local embeddings optional

Decision records should include:
- title
- summary
- timestamp
- project
- related files
- confidence/source

## 4.6 Session Rollups

Long Claude conversations should be compacted into reusable summaries.

Example rollup:

```text
Session Summary
- implemented auth token rotation
- discovered Redis race condition
- deferred audit log changes
- next task: add integration tests
```

## 4.7 MCP Integration

Familiar exposes an MCP server.

Initial tools:
- context.search
- context.pack_for_task
- context.get_file_summary
- context.get_module_summary
- context.get_recent_decisions
- context.get_recent_changes
- context.get_project_status
- context.remember_result

Example:

```json
{
  "task": "continue auth refactor",
  "relevant_files": [
    "src/auth/token.rs",
    "src/auth/middleware.rs"
  ],
  "recent_decisions": [
    "JWT remains stateless",
    "Refresh tokens remain Redis-backed"
  ],
  "constraints": [
    "Preserve mobile payload shape"
  ]
}
```

## 4.8 Local LLM Support

Familiar optionally runs a small local model.

Use cases:
- summarization
- file classification
- relevant-file selection
- task packing
- embeddings generation
- diff summarization
- conversation compression

The local model should be unloadable from memory via systray toggle.

Recommended model sizes:
- 1B–3B for fast mode
- 7B for high quality mode

## 4.9 Token Budgeting

Familiar should explicitly track and limit token usage.

Per-request budgets may include:
- task description
- file context
- decision context
- architecture context
- history summary

Example:

```text
Task: 300 tokens
Relevant files: 1200 tokens
Decisions: 300 tokens
History: 500 tokens
Architecture: 500 tokens
Total Budget: 2800 tokens
```

## 4.10 Project Detection

Familiar should infer active project from:
- terminal cwd
- active IDE window
- current git repository
- recent file edits
- Claude Code session context

---

# 5. Non-Goals

Familiar should not initially include:
- cloud sync
- team collaboration
- cross-device sync
- giant dashboard UI
- browser-based IDE features
- issue tracking
- automated coding agents
- full vector database stack
- complicated permissions model
- transparent MITM response rewriting

---

# 6. Architecture

## 6.1 Core Components

```text
Familiar
├── daemon
├── MCP server
├── local database
├── file watcher
├── project detector
├── summary engine
├── decision logger
├── local LLM manager
├── token budget manager
├── systray UI
└── optional dashboard
```

## 6.2 Suggested Tech Stack

- Rust
- SQLite
- axum or actix-web
- notify for filesystem watching
- serde
- tokio
- mistral.rs for local model support
- tauri only if a richer settings window becomes necessary

---

# 7. Data Model

## Project

```text
Project
- id
- name
- repo_root
- active
- last_used
- ignored_paths
- token_budget
```

## File Summary

```text
FileSummary
- project_id
- path
- summary
- tags
- last_updated
```

## Decision

```text
Decision
- project_id
- title
- summary
- related_files
- timestamp
- source_session
```

## Session Rollup

```text
SessionRollup
- project_id
- timestamp
- summary
- related_files
- next_steps
```

---

# 8. Modes

## Minimal
- indexing only
- no local model
- basic summaries only

## Balanced
- local summarization
- session rollups
- decision logging
- moderate token savings

## Aggressive Savings
- local model handles most preprocessing
- hard context budgets
- strong history compaction
- file summaries preferred over raw files

## Maximum Accuracy
- larger context packs
- less aggressive compression
- optional 7B local model
- higher token spend allowed

---

# 9. MVP Scope

Initial release should include only:
- daemon
- SQLite
- systray menu
- project isolation
- file summaries
- decision log
- session rollups
- local model toggle
- MCP tools
- task packer

Everything else is optional later.

---

# 10. Future Possibilities

Potential future additions:
- branch-aware memory
- semantic search
- embeddings index
- project graphs
- cost analytics dashboard
- team-shared context packs
- issue tracker integration
- PR review summaries
- CI/CD summaries
- local voice commands

These should remain secondary to the core mission:

Keep Claude fast.
Keep Claude cheap.
Keep Claude focused.

