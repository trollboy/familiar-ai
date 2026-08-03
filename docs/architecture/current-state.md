# Familiar: Current-State Architecture

**Analysis date:** August 2, 2026  
**Repository commit analyzed:** Unavailable — the supplied workspace does not contain Git metadata and is not recognized as a Git worktree.

## Executive Summary

Familiar is a local, Rust-based project-memory system intended to make coding assistants cheaper, faster, and less forgetful. Its current implementation is organized around a background daemon that discovers and watches repositories, a separate MCP process that exposes stored context to clients, and a shared SQLite database that acts as the persistence and coordination layer between them.

The system has a clear modular structure, sensible local-first storage, bounded event processing, project-scoped records, a functional MCP tool surface, and a well-separated inference abstraction. However, it remains closer to a functional foundation than to the complete product described in the product vision. The most important architectural issue is inconsistent file-path representation: daemon-generated summaries use absolute paths while MCP operations generally assume repository-relative paths. Runtime state is also fragmented because daemon and MCP processes independently construct status and inference objects without IPC.

Other notable gaps include incomplete removal and rename handling, shallow heuristic summaries and search, unused embedding infrastructure, blocking filesystem and SQLite work inside asynchronous tasks, and several status or UI surfaces that describe capabilities more optimistically than the implementation warrants. The architecture also contains more crate and abstraction boundaries than the current feature set requires, particularly around inference and prospective storage backends.

This document is the baseline description of the system as analyzed on the date above.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Major Subsystems](#major-subsystems)
  - [Configuration and Platform Integration](#configuration-and-platform-integration)
  - [Daemon Orchestration](#daemon-orchestration)
  - [Repository Discovery and Watching](#repository-discovery-and-watching)
  - [Summary Pipeline](#summary-pipeline)
  - [Storage](#storage)
  - [MCP Server](#mcp-server)
  - [Inference Subsystem](#inference-subsystem)
  - [Dashboard and Tray](#dashboard-and-tray)
- [Data Flow](#data-flow)
  - [Repository Ingestion](#repository-ingestion)
  - [MCP Retrieval](#mcp-retrieval)
  - [Explicit Memory Writes](#explicit-memory-writes)
  - [Inference](#inference)
- [Current Strengths](#current-strengths)
- [Current Weaknesses](#current-weaknesses)
- [Areas That Appear Incomplete](#areas-that-appear-incomplete)
- [Areas That Appear Overengineered](#areas-that-appear-overengineered)

## Architecture Overview

Familiar is a local, Rust-based project-memory system built around three principal runtime components:

1. `familiar-daemon` watches repositories, registers projects, generates heuristic file summaries, maintains runtime status, and optionally hosts a tray icon and dashboard.
2. `familiar-mcp` is a separate stdio process launched by an MCP client. It reads and writes the same SQLite database and exposes project context tools.
3. SQLite is the shared persistence and coordination layer between daemon and MCP processes.

The workspace is divided into narrowly scoped crates:

- `familiar-core`: shared configuration, models, paths, errors, status, and version metadata.
- `familiar-storage`: SQLite connection, migrations, SQL, and repositories.
- `familiar-watcher`: repository discovery and filesystem events.
- `familiar-summary`: language detection, regex extraction, and deterministic summaries.
- `familiar-llm`: inference lifecycle, routing, fallback, health checks, and OpenAI-compatible HTTP support.
- `familiar-mcp`: JSON-RPC/MCP transport and context tools.
- `familiar-daemon`: orchestration, workers, dashboard, shutdown, and tray integration.
- `familiar-tray`: native tray UI, excluded from the formal workspace but optionally used by the daemon.
- `familiar-tokens`: approximate token counting and truncation.
- `familiar-logging` and `familiar-testutil`: infrastructure support.

The implementation follows the completed PRDs closely, but it remains closer to a functional foundation than to the full product described in `docs/prds/vision.md`.

## Major Subsystems

### Configuration and Platform Integration

Configuration is layered from defaults, an optional TOML file, and `FAMILIAR_` environment variables. Platform-specific directories are computed for macOS and XDG-style Linux systems.

Configuration already anticipates more functionality than is fully implemented: daemon sockets, summary scheduling, rollups, packer profiles, dashboard settings, text inference, and embedding inference.

### Daemon Orchestration

`crates/familiar-daemon/src/main.rs` is the composition root. Startup performs:

- Directory creation.
- Configuration loading.
- Logging setup.
- Database opening and migration.
- PID-file management.
- Status initialization.
- Inference-router construction.
- Watcher, summary worker, heartbeat, command handler, and optional dashboard startup.
- Tray startup on the main thread when enabled.

Communication between internal tasks uses bounded Tokio channels and a watch channel for shutdown.

### Repository Discovery and Watching

The watcher:

- Watches configured root paths.
- Searches for Git repositories.
- Debounces filesystem notifications.
- Associates file events with repository roots.
- Applies configured ignore rules and optionally respects `.gitignore`.
- Sends typed repository and file events to the daemon.

Repository discovery creates project records automatically. An initial walk then queues files for summary generation.

### Summary Pipeline

The summary worker maintains a per-file pending map keyed by project and path. It waits for a configurable quiet period, checks file size and staleness, reads the file, generates a summary, and upserts it into SQLite.

Current summaries are deterministic metadata summaries, not semantic summaries. They contain approximately:

- Language.
- Path.
- Line count.
- Extracted top-level symbols.
- First doc block.
- Heuristic tags such as `test`, `schema`, `migration`, or `api`.

Extraction is language-aware but regex-based. The configured LLM router is not connected to this background summary pipeline.

### Storage

SQLite uses WAL mode, foreign keys, and a busy timeout. The schema contains:

- `projects`
- `file_summaries`
- `decisions`
- `session_rollups`
- `schema_migrations`

Project IDs scope almost all content queries, which provides basic project isolation. File summaries are unique by project and path.

The repository layer is conventional and straightforward. JSON columns store lists such as tags, symbols, related files, and next steps.

### MCP Server

The MCP binary implements JSON-RPC 2.0 over newline-delimited stdin/stdout. It performs the MCP initialization handshake and exposes ten tools:

- `context.get_project_status`
- `context.get_recent_decisions`
- `context.remember_result`
- `context.create_decision`
- `context.search`
- `context.pack_for_task`
- `context.get_file_summary`
- `context.get_module_summary`
- `context.get_recent_changes`
- `context.get_session_rollups`

The MCP process constructs its own database connection, status object, and inference router. It does not communicate directly with the daemon.

Search is case-insensitive SQL `LIKE` candidate retrieval followed by in-process keyword scoring. Context packing combines file summaries, decisions, and rollups under profile-based approximate token budgets.

### Inference Subsystem

The inference layer has a clean separation among:

- `LlmBackend`: abstract summarize, classify, and embed operations.
- `LlmManager`: backend loading, unloading, and health state.
- `InferenceRouter`: primary and fallback routing.
- `OpenAiHttpBackend`: OpenAI-compatible HTTP endpoints.
- `StubBackend`: deterministic test and development behavior.
- Heuristics for trivial-input bypass, importance, and packer-profile selection.

It supports local-only, remote-only, hybrid, and disabled text modes, plus separate embedding configuration.

### Dashboard and Tray

The optional Axum dashboard exposes health, statistics, projects, recent records, and inference-status endpoints. Its UI is embedded static HTML and JavaScript.

The tray provides LLM toggling, heavy-task pause and resume, recent project links, settings access, status text, and shutdown.

## Data Flow

### Repository Ingestion

```text
Configured watch roots
  → repository discovery
  → RepoDiscovered event
  → project lookup/creation in SQLite
  → initial repository scan
  → bounded SummaryRequest channel
  → per-file quiet-period deduplication
  → metadata/read/staleness checks
  → regex-based summary generation
  → file_summaries upsert
```

Subsequent file changes enter the same summary queue.

### MCP Retrieval

```text
MCP client request
  → stdio JSON-RPC transport
  → tool registry
  → tool argument validation
  → project resolution by ID or repo root
  → SQLite queries
  → optional keyword scoring/token packing
  → structured JSON plus MCP text content
```

### Explicit Memory Writes

```text
context.create_decision
  → validate fields
  → decisions insert

context.remember_result
  → enforce character/list/token limits
  → optionally truncate summary
  → session_rollups insert
```

### Inference

```text
Daemon or MCP startup
  → construct independent InferenceRouter
  → construct primary/fallback managers
  → enable configured HTTP backends
  → health/summarize/classify/embed routing
```

Currently, routine file summary generation and task packing do not use model inference.

## Current Strengths

- Clear separation of concerns. Crate responsibilities are easy to identify.
- SQLite is an appropriate choice for the local-first, single-user product.
- Project-scoped schema and queries provide a sound baseline against cross-project leakage.
- WAL, foreign keys, transactions for migrations, and busy timeouts show good persistence hygiene.
- Bounded channels and summary deduplication protect against unbounded event growth.
- File size limits, rollup ceilings, list limits, and packer ceilings provide useful resource controls.
- `get_file_summary` includes canonical-path containment checks against traversal and symlink escape.
- The system degrades cleanly when inference is disabled or unavailable.
- MCP keeps stdout reserved for protocol traffic and sends logging to stderr.
- The inference backend boundary is clean and supports common local servers through an OpenAI-compatible interface.
- Context packing returns budgets, counts, warnings, and estimated usage rather than hiding truncation.
- The code has substantial unit and integration-test coverage across storage, protocol, tools, extraction, routing, and daemon behavior.
- The product consciously avoids heavyweight UI and vector-database infrastructure.

## Current Weaknesses

### Path Representation Is Inconsistent

The daemon summary worker stores filesystem paths as absolute paths. `context.get_file_summary` is designed around caller-supplied repository-relative paths and writes those relative paths to the same table.

Consequences include:

- Duplicate records for one physical file.
- Cached lookups missing daemon-generated summaries.
- Module-prefix queries such as `src/` failing to match absolute daemon paths.
- Search and packed context exposing machine-specific absolute paths.
- Less portable project memory if a repository moves.

This is the most important architectural inconsistency.

### File Lifecycle Handling Is Incomplete

Changed files are re-summarized, but removal and rename events are only logged. Old rows remain indefinitely. Renames can produce an obsolete record plus a new record.

Initial scans also stop as soon as their bounded channel is full, with no retry or continuation mechanism. Large repositories can therefore be only partially indexed.

### Async Execution Contains Blocking Work

Tokio tasks directly perform:

- Synchronous filesystem walks and reads.
- Regex extraction.
- SQLite operations.
- `std::sync::Mutex` locking.

The summary worker processes ready files serially inside its timer branch. A large flush can block event intake and shutdown responsiveness. MCP's asynchronous storage abstraction similarly wraps synchronous locked SQLite calls without `spawn_blocking`.

### Runtime State Is Fragmented

The daemon and every MCP process create independent:

- `AppStatus`
- `InferenceRouter`
- Backend lifecycle.
- Configuration snapshot.

Therefore:

- MCP project status does not describe the daemon.
- MCP `active_projects` remains zero.
- MCP `local_llm_enabled` is not synchronized with the tray toggle.
- Enabling or disabling inference in the tray does not affect existing or future MCP sessions unless configuration is changed.
- Multiple processes may independently load the same local model-facing backend.

The configured socket path suggests intended IPC, but no socket server or shared-state protocol exists.

### Status Reporting Is Sometimes Aspirational

Examples:

- Dashboard health reports `watcher_running: true` unconditionally.
- `mcp_enabled` means the binary is believed to exist, not that an MCP connection is running.
- The tray pause state is not stored in `AppStatus`, so the menu is always initially rendered as unpaused.
- The tray's polling thread logs status but does not rebuild the menu or tooltip.
- Dashboard recent-record queries silently convert database errors into empty lists.

### Search and Packing Are Shallow

Search is substring matching plus hand-tuned keyword scoring. It lacks:

- Full-text indexing.
- Semantic search.
- Embedding persistence.
- Recency-aware ranking.
- Dependency-aware relevance.
- Branch awareness.
- Robust tokenization.

The context packer fetches at most 100 path-sorted file summaries before scoring, meaning relevant files outside that first slice are invisible. When scores are zero, the claimed recency fallback is actually path order.

Category budgets are also soft: the first item in a category can exceed that category's allocation. The final assembled context adds headings and formatting after allocation, so the reported result can exceed the intended total budget.

### Summary Quality Is Limited

The summary generator largely restates file metadata and top-level names. It does not determine:

- Responsibilities.
- Dependencies.
- Invariants.
- Call relationships.
- Architectural role.
- Risks or constraints.
- Meaningful change summaries.

This falls substantially short of the examples in the product vision.

### Configuration and Deployment Gaps

- `daemon.socket_path` is unused.
- Per-project ignored paths and token budgets exist in the database but are barely integrated into runtime behavior.
- Docker mounts `/data` and `/config`, but default platform paths are not redirected to those mount points.
- Dashboard inference settings are status and test only despite wording suggesting model selection.
- API keys are plain configuration strings with no secret-store integration.
- No authentication is appropriate for the default localhost dashboard, but configurable bind addresses could expose it beyond localhost without protection.

## Areas That Appear Incomplete

- True filesystem recent-change history; `context.get_recent_changes` is currently an alias for session rollups.
- Semantic or LLM-generated file and module summaries.
- Connecting `InferenceRouter` to background summarization and context selection.
- Embedding storage, indexing, and semantic retrieval.
- Deleted-file and renamed-file reconciliation.
- Consistent repository-relative path normalization.
- Reliable completion of initial scans for large repositories.
- Shared daemon and MCP state or IPC.
- Active-project detection from terminal cwd, IDE window, MCP client context, or Git activity.
- Branch-aware memory and branch metadata.
- Automatic session ingestion or rollup generation; rollups currently depend on explicit MCP calls.
- Automatic decision extraction.
- Token-savings and cache-hit metrics promised by the vision.
- Dependency graphs and module or subsystem summaries.
- Per-project configuration enforcement.
- Tray refresh behavior and persistent inference toggling.
- Dashboard configuration editing.
- Local model lifecycle management beyond connecting to an already-running HTTP service.
- Pruning, retention, reconciliation, backup, or database repair procedures.
- Packaging, installation, and service definitions for normal macOS and Linux background operation.

## Areas That Appear Overengineered

- Ten workspace crates plus a separately excluded tray crate are a lot of package boundaries for the present code size and maturity. `familiar-tokens`, `familiar-logging`, and `familiar-testutil` could plausibly be modules until they gain independent consumers.
- The async `Storage` trait anticipates remote storage, caches, vector services, and PostgreSQL even though the product explicitly aims to remain a lightweight local companion. It adds indirection without solving the current blocking-SQLite problem.
- The inference subsystem has managers, factories, health types, router policies, four manager slots, fallback topology, classify and embed operations, heuristics, connection testing, and a settings page, while the main summary pipeline does not use inference at all.
- The stub backend and heuristic routing create several successful inference-shaped paths that do not produce meaningful inference.
- Status is modeled independently in daemon, MCP, dashboard, and tray-facing behavior without a shared source of truth. The abstraction count is higher than the actual consistency achieved.
- Centralizing every SQL statement in one file separates queries from the repository code that explains them. At the current schema size, this adds navigation overhead and has already encouraged broad `SELECT *` coupling.
- The tray has its own manifest and lockfile while being excluded from the workspace but referenced as an optional daemon dependency. That is an awkward structural compromise and makes workspace-wide build and test semantics less obvious.
- The PRD and configuration surface are ahead of the working product. Several polished control surfaces surround capabilities that are still placeholders or disconnected.
