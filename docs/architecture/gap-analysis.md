# Familiar Architecture Gap Analysis

## Executive Summary

Familiar's current architecture is a credible local project-memory foundation, but it does not yet implement the persistent engineering-steward role defined by the philosophy and target-state architecture. The existing system provides useful building blocks: a Rust daemon, project-scoped SQLite storage, repository discovery and watching, deterministic metadata extraction, MCP tools, bounded channels, token ceilings, an optional dashboard and tray, and a provider-neutral inference backend boundary. Those capabilities should generally be preserved and extended.

The largest architectural gap is authority fragmentation. The daemon and each MCP process independently own status, configuration snapshots, inference routers, and direct database access. The target requires one central daemon to own canonical mutable state and expose shared core services through thin MCP, CLI, local-socket, and loopback-HTTP adapters. This is a structural defect relative to the target, not merely an absent feature.

Three current implementation defects also deserve explicit separation from broader product gaps:

- File summaries use inconsistent absolute and repository-relative identities, compromising cache reuse and module queries.
- Removed and renamed files are observed but not reconciled in persistent state.
- Runtime health surfaces report inferred or hard-coded capability state rather than observed state.

Most remaining gaps are missing product capabilities rather than failures of existing code: canonical task/finding/test/handoff state, content-hashed intelligence, reproducible context manifests, policy and approval gates, isolated worktrees, execution warrants, deterministic verification evidence, independent adversarial review, audit history, rollback records, neutral agent adapters, and explicit host capability enforcement.

This analysis recommends no wholesale replacement of a working subsystem. The dominant classifications are **Extend** and **Refactor**. The only removal recommendation concerns misleading or duplicate authority paths, not a functional user capability. No implementation sequence is implied.

## Gap Matrix

| Gap ID | Title | Classification | Complexity | Risk | Dependencies |
|---|---|---:|---:|---:|---|
| GAP-001 | Canonical core authority is fragmented | Refactor | High | Critical | Existing daemon, storage, MCP transport, local IPC |
| GAP-002 | Canonical engineering domain state is incomplete | Extend | High | High | GAP-001, canonical store |
| GAP-003 | File identity is inconsistent | Refactor | Medium | High | Project identity, storage queries, summary pipeline |
| GAP-004 | Repository intelligence lacks content identity and provenance | Extend | High | High | GAP-003, watcher, summary engine, storage |
| GAP-005 | File lifecycle reconciliation is incomplete | Extend | Medium | Medium | GAP-003, watcher events, storage |
| GAP-006 | Context packing is not a reproducible context compiler | Extend | High | High | GAP-002, GAP-004, token accounting |
| GAP-007 | Deterministic verification is not a core service | Extend | High | Critical | GAP-002, host capabilities, audit evidence |
| GAP-008 | Bounded isolated execution and warrants are absent | Extend | Very High | Critical | GAP-001, GAP-002, GAP-007, host capabilities, approvals |
| GAP-009 | Independent adversarial review is absent | Extend | High | High | GAP-002, GAP-006, GAP-007, agent routing |
| GAP-010 | Interfaces are not thin adapters over shared services | Refactor | High | High | GAP-001, local socket, shared command/query API |
| GAP-011 | Human approval and deterministic policy are absent | Extend | High | Critical | GAP-002, audit, execution supervisor |
| GAP-012 | Auditability, evidence lineage, and rollback state are absent | Extend | High | Critical | GAP-002, GAP-007, canonical events |
| GAP-013 | Agent neutrality stops at model inference | Extend | High | Medium | GAP-002, GAP-006, workflow policy |
| GAP-014 | Host capabilities are implicit rather than governed | Extend | High | High | Central daemon, platform adapters, execution policy |
| GAP-015 | Blocking work is performed inside asynchronous services | Refactor | Medium | Medium | Daemon worker model, SQLite access boundary |
| GAP-016 | Runtime status is not authoritative | Refactor | Medium | High | GAP-001, observed subsystem lifecycle |
| GAP-017 | Trust boundaries and secret handling are incomplete | Extend | High | High | GAP-001, interfaces, host credential capability |
| GAP-018 | Durable memory lacks provenance and approval semantics | Extend | High | High | GAP-002, repository revisions, approval state |
| GAP-019 | Derived intelligence is not explicitly subordinate to source | Extend | Medium | High | GAP-004, context compiler, provenance |
| GAP-020 | Duplicate mutable authority outside the daemon must disappear | Remove | Medium | High | GAP-001, GAP-010 |

## Detailed Gaps

### GAP-001 — Canonical Core Authority Is Fragmented

**Gap type:** Architectural defect  
**Recommendation classification:** Refactor  
**Estimated complexity:** High  
**Risk:** Critical

**Current evidence**

The daemon assembles its own database, status, and inference router in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs#L34), including an `Arc<Mutex<Database>>` and process-local `AppStatus`. The MCP binary independently opens the same database, constructs a fresh status object, and constructs and enables another inference router in [`crates/familiar-mcp/src/bin/familiar-mcp.rs`](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L42). The current-state document identifies the resulting status and inference fragmentation in [`current-state.md`](current-state.md#runtime-state-is-fragmented).

The target requires a single daemon to be the sole authority for mutable Familiar state and requires all interfaces to invoke shared core commands and queries in [`target-state.md`](target-state.md#target-architecture).

**Target capability**

One long-lived central daemon owns canonical state transitions, workflow coordination, policy, subsystem lifecycle, and mutable runtime state. Other processes are clients of that authority.

**Why the gap exists**

The current system grew as independent daemon and MCP deliverables coordinated through SQLite. That was sufficient for project memory retrieval, but the target introduces authoritative workflow, policy, approvals, execution, and live status that cannot safely be reconstructed independently in each client process.

**Dependencies**

- Existing daemon composition and storage repositories.
- A shared command/query boundary.
- Local IPC suitable for MCP and other thin clients.
- Transaction and concurrency ownership rules.

**Recommendation**

Refactor authority into the existing daemon and reusable core services. Preserve SQLite, MCP protocol handling, and working domain repositories; change ownership and invocation boundaries rather than replacing those systems.

### GAP-002 — Canonical Engineering Domain State Is Incomplete

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

The schema defines only projects, file summaries, decisions, and session rollups in [`crates/familiar-storage/migrations/001_init.sql`](../../crates/familiar-storage/migrations/001_init.sql#L1). The shared models mirror those concepts in [`crates/familiar-core/src/models.rs`](../../crates/familiar-core/src/models.rs). There is no canonical task, finding, verification result, handoff, warrant, approval, or audit-event model.

The target explicitly defines canonical project, task, decision, finding, test, and handoff state in [`target-state.md`](target-state.md#canonical-domain-state), and adds warrants, approvals, and audit events to the canonical store in [`target-state.md`](target-state.md#canonical-state-store).

**Target capability**

A coherent, project-scoped domain model records bounded work, independent findings, reproducible verification, durable handoffs, authority grants, approvals, and causal audit history.

**Why the gap exists**

The existing repository implements the earlier memory-oriented product scope. The stewardship domain was defined later and is genuinely absent rather than incorrectly implemented.

**Dependencies**

- GAP-001 canonical authority.
- Existing SQLite migration and repository patterns.
- Stable project and revision identity.
- Human ownership and provenance rules.

**Recommendation**

Extend the existing canonical storage and core model patterns. Do not reinterpret conversations or rollups as substitutes for explicit task, finding, test, approval, or handoff records.

### GAP-003 — File Identity Is Inconsistent

**Gap type:** Implementation bug with architectural consequences  
**Recommendation classification:** Refactor  
**Estimated complexity:** Medium  
**Risk:** High

**Current evidence**

The background summary worker persists `path.to_string_lossy()` from filesystem events, which is an absolute host path, in [`crates/familiar-daemon/src/summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L174) and [`summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L198). By contrast, `context.get_file_summary` resolves a caller path under `repo_root` and stores the caller-provided path in [`crates/familiar-mcp/src/tools/get_file_summary.rs`](../../crates/familiar-mcp/src/tools/get_file_summary.rs#L55). Module summary queries treat paths as repository-relative prefixes in [`crates/familiar-mcp/src/tools/get_module_summary.rs`](../../crates/familiar-mcp/src/tools/get_module_summary.rs#L56).

The target invariant requires project-scoped repository-relative durable paths and hash-based content identity in [`target-state.md`](target-state.md#architectural-invariants).

**Target capability**

Every durable file record has one canonical project-relative identity. Host absolute paths are runtime locations only, and content hashes identify immutable content.

**Why the gap exists**

The watcher/daemon pipeline and MCP lazy-summary path were implemented through different call paths without a single file-identity abstraction or storage invariant.

**Dependencies**

- Project root resolution.
- Path normalization and containment rules.
- Storage uniqueness and query semantics.
- Repository intelligence provenance.

**Recommendation**

Refactor the existing summary and storage boundary around one canonical file identity. Preserve the watcher, summaries, and containment protection; eliminate dual absolute/relative representations.

### GAP-004 — Repository Intelligence Lacks Content Identity and Provenance

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

File summary freshness relies on modification time and age in [`crates/familiar-daemon/src/summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L165). The code explicitly notes that content hashes are not stored in [`summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L169). Migration 002 adds mtime and size but no content hash, generator identity, source revision, dependency lineage, or provenance in [`crates/familiar-storage/migrations/002_summaries_decisions.sql`](../../crates/familiar-storage/migrations/002_summaries_decisions.sql#L1). The deterministic summary generator returns text, tags, symbols, and line count only in [`crates/familiar-summary/src/generator.rs`](../../crates/familiar-summary/src/generator.rs#L6).

The target requires content-hashed inventory, provenance-bearing derived artifacts, dependency invalidation, branch/worktree awareness, and rebuildable optional indexes in [`target-state.md`](target-state.md#repository-intelligence-engine).

**Target capability**

Repository intelligence is keyed to content and generating configuration, has explicit provenance, reconciles Git identity and dependencies, and safely reuses unchanged artifacts.

**Why the gap exists**

The current summary pipeline optimized for simple local freshness checks. The stronger identity and provenance requirements were not part of that implementation scope.

**Dependencies**

- GAP-003 canonical file identity.
- Existing watcher and deterministic extractor.
- Git revision and worktree identity.
- Derived-artifact storage and invalidation rules.

**Recommendation**

Extend the existing watcher, extractor, summary, and storage subsystems. Content hashing and provenance should strengthen the working pipeline rather than replace it with an LLM or vector database.

### GAP-005 — File Lifecycle Reconciliation Is Incomplete

**Gap type:** Implementation bug  
**Recommendation classification:** Extend  
**Estimated complexity:** Medium  
**Risk:** Medium

**Current evidence**

The watcher has explicit `FileRemoved` and `FileRenamed` event types in [`crates/familiar-watcher/src/events.rs`](../../crates/familiar-watcher/src/events.rs#L12). The daemon handler only logs these events and performs no storage reconciliation in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs#L532). The initial scan stops when its bounded channel cannot accept another item in [`crates/familiar-daemon/src/summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L251), which can leave large projects partially indexed.

The target repository intelligence flow requires change, rename, and deletion reconciliation in [`target-state.md`](target-state.md#repository-observation-and-intelligence).

**Target capability**

Persistent intelligence accurately reflects deletions, renames, and complete scans, with explicit indexing health when reconciliation is incomplete.

**Why the gap exists**

Event detection was implemented before durable lifecycle semantics and resumption state. This is not a watcher replacement problem; it is missing downstream handling.

**Dependencies**

- GAP-003 canonical paths.
- Existing watcher event stream.
- File-summary deletion or tombstone semantics.
- Durable scan status and backpressure behavior.

**Recommendation**

Extend event handling and scan state around the working watcher. Preserve bounded queues, ignore behavior, and repository discovery.

### GAP-006 — Context Packing Is Not a Reproducible Context Compiler

**Gap type:** Missing product capability plus current limitations  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

`context.pack_for_task` already defines profiles, category percentages, and a hard ceiling in [`crates/familiar-mcp/src/tools/pack_for_task.rs`](../../crates/familiar-mcp/src/tools/pack_for_task.rs#L12). However, it selects from at most 100 path-ordered summaries before scoring in [`pack_for_task.rs`](../../crates/familiar-mcp/src/tools/pack_for_task.rs#L134), uses heuristic character-based token estimation from [`crates/familiar-tokens/src/lib.rs`](../../crates/familiar-tokens/src/lib.rs#L1), and returns assembled text without an immutable input manifest or source provenance.

The target context compiler requires task/revision-specific inputs, applicable policies and findings, content-hashed evidence, explicit omissions, agent-aware budgets, authoritative-source fallback, and reproducible manifests in [`target-state.md`](target-state.md#context-compiler).

**Target capability**

Context is an immutable, attributable artifact compiled for one bounded task and receiving-agent profile, with visible budgets, provenance, truncation, and omissions.

**Why the gap exists**

The current packer is a useful memory packing tool built before canonical tasks, content-hashed intelligence, policy state, or agent capability profiles existed.

**Dependencies**

- GAP-002 task and finding state.
- GAP-004 provenance-bearing intelligence.
- Agent capability metadata.
- Reliable token accounting and immutable artifact identity.

**Recommendation**

Extend the existing packer and token-budget concepts into a shared context compiler. Preserve its profiles, warnings, ceilings, and structured result pattern where they remain valid.

### GAP-007 — Deterministic Verification Is Not a Core Service

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** Critical

**Current evidence**

The repository contains tests for Familiar itself, but no production subsystem discovers project verification commands, executes builds/tests/linters, captures logs, checks diffs and invariants, or stores verification results. The daemon composition in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs#L136) starts watchers, summary work, heartbeat, commands, and dashboard only. The canonical schema has no verification-result records in [`001_init.sql`](../../crates/familiar-storage/migrations/001_init.sql#L1).

The philosophy makes deterministic verification non-negotiable in [`docs/philosophy.md`](../philosophy.md#engineering-invariants), and the target defines a deterministic verification engine and structured evidence in [`target-state.md`](target-state.md#deterministic-verification-engine).

**Target capability**

Familiar executes appropriate deterministic checks, records exact commands, tools, environments, revisions, logs, and outcomes, and distinguishes failure, absence, skip, and success.

**Why the gap exists**

Current production scope is repository memory and context retrieval, not supervised engineering execution.

**Dependencies**

- Canonical task and test-result state.
- Host process/tool capabilities.
- Audit artifact storage.
- Project policy and required-check definitions.

**Recommendation**

Extend the daemon with a core verification boundary that invokes existing project tools. Familiar should orchestrate and preserve evidence, not replace compilers, linters, test suites, Docker, or CI.

### GAP-008 — Bounded Isolated Execution and Warrants Are Absent

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** Very High  
**Risk:** Critical

**Current evidence**

No production module manages Git worktrees, agent processes, command allowlists, resource limits, warrants, checkpoints, or unattended execution. Existing daemon commands only toggle inference, pause summary work, and quit in [`crates/familiar-daemon/src/command.rs`](../../crates/familiar-daemon/src/command.rs#L13).

The target requires isolated worktrees and attributable, bounded, revocable execution warrants with explicit scope and stop conditions in [`target-state.md`](target-state.md#worktree-and-execution-supervisor).

**Target capability**

Unattended work occurs only in an identified isolated worktree under explicit authority governing paths, commands, network access, resources, external effects, verification, expiration, and stopping conditions.

**Why the gap exists**

This is a new stewardship responsibility beyond the current passive companion daemon. It is missing capability, not a defect in the existing summary worker.

**Dependencies**

- GAP-001 central authority.
- GAP-002 tasks, warrants, and approvals.
- GAP-007 verification evidence.
- GAP-011 policy gates.
- GAP-014 enforceable host capabilities.

**Recommendation**

Extend Familiar with a bounded execution subsystem alongside existing background services. Do not generalize it into open-ended autonomous-agent execution.

### GAP-009 — Independent Adversarial Review Is Absent

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

The inference abstraction exposes summarize, classify, and embed operations in [`crates/familiar-llm/src/backend.rs`](../../crates/familiar-llm/src/backend.rs#L8), but there is no implementation/reviewer role separation, review assignment, durable finding model, or evidence-based disposition workflow. The existing decision and rollup tools record memory but not independent review findings in [`crates/familiar-mcp/src/tools/mod.rs`](../../crates/familiar-mcp/src/tools/mod.rs#L1).

The philosophy states that no single model should both perform and declare work correct in [`docs/philosophy.md`](../philosophy.md#multi-agent-philosophy). The target requires independent adversarial model review and durable findings in [`target-state.md`](target-state.md#multi-agent-review-coordinator).

**Target capability**

Distinct implementer and reviewer roles, potentially across models, produce separately attributable evidence and findings that require explicit disposition.

**Why the gap exists**

The current LLM subsystem routes inference operations, not engineering roles or workflows.

**Dependencies**

- GAP-002 tasks and findings.
- GAP-006 context packages.
- GAP-007 verification evidence.
- GAP-013 neutral agent capability routing.

**Recommendation**

Extend orchestration around the provider-neutral backend concepts. Do not treat model agreement as proof or reuse the implementation agent as the sole reviewer.

### GAP-010 — Interfaces Are Not Thin Adapters over Shared Services

**Gap type:** Architectural defect  
**Recommendation classification:** Refactor  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

MCP tools directly invoke an async storage trait in [`crates/familiar-mcp/src/tool.rs`](../../crates/familiar-mcp/src/tool.rs#L37) and [`crates/familiar-mcp/src/storage.rs`](../../crates/familiar-mcp/src/storage.rs#L31). The MCP binary opens the database itself in [`familiar-mcp.rs`](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L42). The dashboard calls repositories directly through shared database mutexes in [`crates/familiar-daemon/src/dashboard.rs`](../../crates/familiar-daemon/src/dashboard.rs#L20). A platform socket path exists in [`crates/familiar-core/src/paths.rs`](../../crates/familiar-core/src/paths.rs#L12), but no local socket service uses it.

The target requires MCP, CLI, local socket, and loopback HTTP to translate into the same core commands and queries in [`target-state.md`](target-state.md#interface-and-client-adapters).

**Target capability**

Protocol adapters share authorization, validation, policy, and state semantics and do not independently implement business rules or database mutation.

**Why the gap exists**

MCP and dashboard were added as direct views over early storage because no central application-service boundary existed.

**Dependencies**

- GAP-001 canonical core.
- Shared command/query contracts.
- Local IPC and event semantics.
- Interface authentication and capability mapping.

**Recommendation**

Refactor working MCP and HTTP handlers into thin adapters while preserving their protocol, schemas where compatible, and presentation behavior. Extend with CLI and local-socket adapters rather than creating separate cores.

### GAP-011 — Human Approval and Deterministic Policy Are Absent

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** Critical

**Current evidence**

Current configuration controls operational settings but defines no project execution policy or approval state in [`crates/familiar-core/src/config.rs`](../../crates/familiar-core/src/config.rs). The schema has no approval or warrant records in [`001_init.sql`](../../crates/familiar-storage/migrations/001_init.sql#L1). Tray commands take effect immediately and are operational toggles, not attributable human gates, in [`crates/familiar-daemon/src/command.rs`](../../crates/familiar-daemon/src/command.rs#L63).

Human ownership of architecture is mandatory in [`docs/philosophy.md`](../philosophy.md#4-humans-own-architecture). The target requires deterministic policy and explicit approval records for warrants, architecture, privilege, external effects, destructive actions, risk acceptance, and policy changes in [`target-state.md`](target-state.md#human-approval-gates).

**Target capability**

Inspectable policy determines required gates, and approval records bind a human to exact scope, revision, action, and expiration.

**Why the gap exists**

The current daemon performs no coding-agent execution, publication, or architectural workflow, so authority state was not required by its original scope.

**Dependencies**

- GAP-002 domain records.
- GAP-012 attributable audit history.
- Execution and interface identities.
- Host and external-effect capability descriptions.

**Recommendation**

Extend core domain and policy services. Approval must be canonical state, never inferred from chat language or delegated to an LLM.

### GAP-012 — Auditability, Evidence Lineage, and Rollback State Are Absent

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** Critical

**Current evidence**

The current system has tracing logs through [`crates/familiar-logging/src/lib.rs`](../../crates/familiar-logging/src/lib.rs#L1), timestamps on memory records, and Git remains external. It has no append-oriented audit domain, causal event relationships, retained command evidence, approval lineage, external-effect records, or explicit rollback status in the storage schema.

The philosophy requires project history and rollback capability in [`docs/philosophy.md`](../philosophy.md#engineering-invariants). The target specifies attributable audit events, evidence reconstruction, tamper-evidence, and explicit rollback truth in [`target-state.md`](target-state.md#audit-evidence-and-rollback-service).

**Target capability**

Humans can reconstruct authorization, context, actors, commands, revisions, changes, checks, reviews, acceptance, external effects, and known rollback mechanisms for every material task.

**Why the gap exists**

Operational logs were sufficient for daemon diagnostics but are not durable engineering evidence or canonical workflow history.

**Dependencies**

- GAP-002 canonical identities.
- GAP-007 structured verification.
- GAP-011 approvals.
- Artifact retention, redaction, and integrity policy.

**Recommendation**

Extend existing logging and persistence with a separate audit/evidence responsibility. Preserve tracing for diagnostics; do not misclassify ordinary logs as complete audit records.

### GAP-013 — Agent Neutrality Stops at Model Inference

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** Medium

**Current evidence**

`LlmBackend` provides a useful vendor-neutral inference interface, and `InferenceRouter` supports primary/fallback configuration in [`crates/familiar-llm/src/router.rs`](../../crates/familiar-llm/src/router.rs#L82). Its operations are limited to summarize, classify, and embed; there are no Claude Code, Codex, Cursor, OpenCode, or generic coding-agent adapters, capability advertisements, task dispatch contracts, or neutral result/handoff contracts.

The philosophy requires coding agents to be interchangeable implementation details in [`docs/philosophy.md`](../philosophy.md#8-agents-are-replaceable). The target defines provider-neutral capability routing and thin clients in [`target-state.md`](target-state.md#model-and-agent-capability-router).

**Target capability**

Neutral project/task/context/warrant/finding/handoff contracts are translated by replaceable agent adapters selected by capability, privacy, cost, latency, health, and role.

**Why the gap exists**

The existing abstraction targets individual inference requests rather than supervised coding-agent sessions.

**Dependencies**

- GAP-002 task and handoff contracts.
- GAP-006 context manifests.
- Workflow and policy decisions.
- Agent authentication and capability discovery.

**Recommendation**

Extend the existing provider abstraction principles to coding-agent adapters. Preserve the OpenAI-compatible inference backend; do not force coding-agent orchestration through summarize/classify/embed APIs.

### GAP-014 — Host Capabilities Are Implicit Rather than Governed

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

`AppPaths` provides macOS and Linux filesystem locations and socket paths in [`crates/familiar-core/src/paths.rs`](../../crates/familiar-core/src/paths.rs#L5). The tray has platform-specific GTK behavior in [`crates/familiar-tray/src/tray.rs`](../../crates/familiar-tray/src/tray.rs#L39), and shutdown handles platform signals in [`crates/familiar-daemon/src/shutdown.rs`](../../crates/familiar-daemon/src/shutdown.rs). There is no unified capability model for worktrees, process isolation, sandboxing, resource enforcement, secure credentials, service lifecycle, containers, or toolchains.

The target requires explicit macOS/Linux capability facts and refusal when required guarantees cannot be enforced in [`target-state.md`](target-state.md#macos-and-linux-host-model).

**Target capability**

Portable core services request host capabilities through explicit adapters and make policy decisions based on observed enforcement, not platform assumptions.

**Why the gap exists**

Current platform support covers daemon location, signals, and UI, not supervised execution.

**Dependencies**

- Central daemon lifecycle.
- Execution and verification requirements.
- Platform-specific enforcement mechanisms.
- Credential and service integration.

**Recommendation**

Extend existing platform-aware code into a coherent host capability boundary. Preserve established paths and native tray integration where they satisfy the capability contract.

### GAP-015 — Blocking Work Is Performed inside Asynchronous Services

**Gap type:** Architectural defect  
**Recommendation classification:** Refactor  
**Estimated complexity:** Medium  
**Risk:** Medium

**Current evidence**

The daemon shares `rusqlite::Connection` behind `std::sync::Mutex` in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs#L38). The summary worker synchronously performs metadata access, file reads, extraction, and database operations from its Tokio select loop in [`crates/familiar-daemon/src/summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L68). MCP exposes an async storage trait while synchronously locking SQLite in [`crates/familiar-mcp/src/storage.rs`](../../crates/familiar-mcp/src/storage.rs#L102).

The target central daemon must coordinate verification, execution, indexing, interfaces, and recovery with reliable backpressure and shutdown in [`target-state.md`](target-state.md#central-daemon-and-core-services).

**Target capability**

Blocking filesystem, parsing, Git, process, and SQLite work cannot stall asynchronous control-plane responsiveness or hold broad shared locks across unrelated operations.

**Why the gap exists**

The current workload is small and serial enough that simple synchronous ownership was pragmatic. The target substantially increases concurrent control-plane responsibilities.

**Dependencies**

- Central daemon concurrency model.
- Database ownership and transaction semantics.
- Worker backpressure and cancellation.
- Metrics for queue health and latency.

**Recommendation**

Refactor execution boundaries while preserving `rusqlite`, bounded channels, and deterministic extraction unless measured evidence justifies replacement. The gap does not justify adopting remote storage or a different database by itself.

### GAP-016 — Runtime Status Is Not Authoritative

**Gap type:** Implementation bug and architectural defect  
**Recommendation classification:** Refactor  
**Estimated complexity:** Medium  
**Risk:** High

**Current evidence**

Dashboard health reports `watcher_running: true` unconditionally in [`crates/familiar-daemon/src/dashboard.rs`](../../crates/familiar-daemon/src/dashboard.rs#L88). The daemon sets `mcp_enabled` to mean compiled capability rather than an active MCP session in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs#L110). The MCP process initializes a fresh `AppStatus` in [`familiar-mcp.rs`](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L52), so its project and inference status cannot describe daemon state. The tray pause state is not part of `AppStatus` in [`crates/familiar-core/src/status.rs`](../../crates/familiar-core/src/status.rs#L4).

The target makes the daemon the authoritative coordinator and requires failures and missing capabilities to remain visible in [`target-state.md`](target-state.md#architectural-invariants).

**Target capability**

Health and status are observed subsystem lifecycle state with clear distinctions among configured, available, running, degraded, connected, paused, and failed.

**Why the gap exists**

The current status model is a small presentation snapshot assembled independently by processes, not a canonical subsystem registry.

**Dependencies**

- GAP-001 central authority.
- Subsystem lifecycle and heartbeat state.
- Interface connection tracking.
- Structured error and degradation reporting.

**Recommendation**

Refactor status into daemon-owned observed state. Preserve the dashboard and tray as consumers, but remove inferred constants and process-local substitutes.

### GAP-017 — Trust Boundaries and Secret Handling Are Incomplete

**Gap type:** Missing product capability with current exposure risk  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

Remote API keys are plain optional configuration strings in [`crates/familiar-core/src/config.rs`](../../crates/familiar-core/src/config.rs#L111). The dashboard bind address is configurable in [`config/default.toml`](../../config/default.toml#L51), while dashboard routes have no authentication or authorization layer in [`crates/familiar-daemon/src/dashboard.rs`](../../crates/familiar-daemon/src/dashboard.rs#L35). MCP accepts client requests and directly reaches mutable storage after protocol initialization in [`crates/familiar-mcp/src/server.rs`](../../crates/familiar-mcp/src/server.rs#L48). There is no execution sandbox or untrusted-repository-content policy because agent execution is not implemented.

The target defines human, client, agent/model, repository-content, execution, provider/network, derived-intelligence, and verification-tool trust boundaries in [`target-state.md`](target-state.md#trust-boundaries).

**Target capability**

Least-privilege local clients, host-backed credentials, explicit disclosure policy, untrusted content handling, authenticated IPC where appropriate, and warrant-enforced execution boundaries.

**Why the gap exists**

The current local companion has a much smaller threat surface than the target unattended execution and multi-agent system.

**Dependencies**

- GAP-001 authority boundary.
- GAP-010 unified interfaces.
- GAP-011 policy and approvals.
- GAP-014 credential and isolation capabilities.

**Recommendation**

Extend security boundaries in proportion to new authority. Preserve loopback-first operation and current path traversal protection; do not add a generalized permissions platform beyond the target's local, project-scoped needs.

### GAP-018 — Durable Memory Lacks Provenance and Approval Semantics

**Gap type:** Missing product capability  
**Recommendation classification:** Extend  
**Estimated complexity:** High  
**Risk:** High

**Current evidence**

Decisions currently store title, summary, related files, optional source session, confidence, and timestamps in [`crates/familiar-storage/migrations/001_init.sql`](../../crates/familiar-storage/migrations/001_init.sql#L26) and [`002_summaries_decisions.sql`](../../crates/familiar-storage/migrations/002_summaries_decisions.sql#L1). `context.create_decision` permits a client to create a decision directly after field validation in [`crates/familiar-mcp/src/tools/create_decision.rs`](../../crates/familiar-mcp/src/tools/create_decision.rs#L31). Rollups store summary, files, and next steps but no source revision or approval status in [`crates/familiar-mcp/src/tools/remember_result.rs`](../../crates/familiar-mcp/src/tools/remember_result.rs#L31).

The target distinguishes proposed from human-approved knowledge, relates decisions to revisions and evidence, and preserves supersession history in [`target-state.md`](target-state.md#engineering-memory-and-decision-service).

**Target capability**

Typed durable memory is attributable, revision-aware, approval-aware, project-scoped, and never silently overwritten or promoted from an agent assertion to architectural truth.

**Why the gap exists**

The current memory model optimizes simple recall and assumes cooperative clients. It predates human-owned architectural decision semantics.

**Dependencies**

- GAP-002 domain model.
- GAP-011 approvals.
- Revision and artifact provenance.
- Identity for human, agent, and deterministic actors.

**Recommendation**

Extend existing decisions and rollups rather than replacing them. Preserve their useful content and project isolation while making status, provenance, relationships, and supersession explicit.

### GAP-019 — Derived Intelligence Is Not Explicitly Subordinate to Source

**Gap type:** Architectural safeguard gap  
**Recommendation classification:** Extend  
**Estimated complexity:** Medium  
**Risk:** High

**Current evidence**

The current summary records contain generated prose and symbols but no source hash or explicit authoritative/derived marker in [`crates/familiar-core/src/models.rs`](../../crates/familiar-core/src/models.rs). Search and task packing return summaries as context without provenance or source-validation requirements in [`crates/familiar-mcp/src/tools/search.rs`](../../crates/familiar-mcp/src/tools/search.rs#L27) and [`pack_for_task.rs`](../../crates/familiar-mcp/src/tools/pack_for_task.rs#L49). `get_file_summary` does appropriately re-read source when cached metadata is stale in [`crates/familiar-mcp/src/tools/get_file_summary.rs`](../../crates/familiar-mcp/src/tools/get_file_summary.rs#L65), which is a foundation to preserve.

The philosophy states that the repository is canonical and derived artifacts must never replace authoritative source in [`docs/philosophy.md`](../philosophy.md#1-the-repository-is-truth). The target requires provenance and authoritative-source fallback in [`target-state.md`](target-state.md#repository-intelligence-engine).

**Target capability**

Every consumer can distinguish source facts from derived claims, evaluate freshness and provenance, and inspect source whenever correctness or uncertainty requires it.

**Why the gap exists**

The current summaries are simple local cache records; formal epistemic status was unnecessary for the initial MCP tools but becomes essential when Familiar supervises engineering decisions.

**Dependencies**

- GAP-004 content hashes and provenance.
- GAP-006 context manifests.
- Confidence and invalidation semantics.

**Recommendation**

Extend summary and context metadata with explicit derivation and source references. Preserve lazy source regeneration and deterministic extraction.

### GAP-020 — Duplicate Mutable Authority outside the Daemon Must Disappear

**Gap type:** Architectural defect  
**Recommendation classification:** Remove  
**Estimated complexity:** Medium  
**Risk:** High

**Current evidence**

The MCP binary directly opens the database read/write because `remember_result` and decision tools mutate it in [`crates/familiar-mcp/src/bin/familiar-mcp.rs`](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L42). It also constructs and enables its own router in [`familiar-mcp.rs`](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L59). The current-state analysis documents that tray inference changes do not propagate to MCP and that process status diverges in [`current-state.md`](current-state.md#runtime-state-is-fragmented).

The target explicitly prohibits direct client database access and competing core state in [`target-state.md`](target-state.md#canonical-state-store), while requiring protocol adapters to share core semantics.

**Target capability**

MCP and other clients retain their useful protocol behavior but have no independent write authority, policy implementation, or inference lifecycle that competes with the daemon.

**Why the gap exists**

Direct access was a pragmatic way to make the per-session MCP process functional before local IPC and canonical application services existed.

**Dependencies**

- GAP-001 central authority.
- GAP-010 shared service adapters.
- Reliable local IPC, error propagation, and daemon availability semantics.

**Recommendation**

Remove only duplicate mutable authority after equivalent shared daemon services exist. Do **not** remove MCP, SQLite, decision tools, rollup tools, or inference capability; retain them behind the canonical core boundary.

## Capabilities That Should Be Preserved Unchanged

The following current capabilities already align with the target and have no evidence-backed replacement case:

- **Local-first Rust daemon foundation.** The daemon is already the natural host for long-lived coordination in [`crates/familiar-daemon/src/main.rs`](../../crates/familiar-daemon/src/main.rs).
- **SQLite as the local canonical persistence engine.** WAL mode, foreign keys, busy timeout, and transactional migrations are appropriate in [`crates/familiar-storage/src/db.rs`](../../crates/familiar-storage/src/db.rs#L21) and [`crates/familiar-storage/src/migrate.rs`](../../crates/familiar-storage/src/migrate.rs#L17). The target does not require a different database.
- **Project-scoped relational storage.** Foreign keys and project-qualified uniqueness provide a sound isolation baseline in [`001_init.sql`](../../crates/familiar-storage/migrations/001_init.sql#L14).
- **Repository discovery, filesystem watching, debounce, and ignore support.** These are valid inputs to richer reconciliation in [`crates/familiar-watcher/src/watcher.rs`](../../crates/familiar-watcher/src/watcher.rs) and [`crates/familiar-watcher/src/discovery.rs`](../../crates/familiar-watcher/src/discovery.rs).
- **Deterministic language and symbol extraction.** It implements the philosophy's determinism-first rule and should remain available even if optional semantic summarization is added in [`crates/familiar-summary/src/extractor.rs`](../../crates/familiar-summary/src/extractor.rs).
- **Bounded summary queues, file-size limits, quiet-period deduplication, and staleness controls.** These are sound resource-management concepts in [`summary_worker.rs`](../../crates/familiar-daemon/src/summary_worker.rs#L68).
- **MCP protocol and stdio transport.** MCP remains a required target interface; its transport and registry are reusable in [`crates/familiar-mcp/src/server.rs`](../../crates/familiar-mcp/src/server.rs) and [`crates/familiar-mcp/src/transport.rs`](../../crates/familiar-mcp/src/transport.rs).
- **Explicit token ceilings, truncation warnings, and context profiles.** Their implementation needs extension, but the controls themselves are aligned in [`pack_for_task.rs`](../../crates/familiar-mcp/src/tools/pack_for_task.rs#L12) and [`crates/familiar-tokens/src/lib.rs`](../../crates/familiar-tokens/src/lib.rs).
- **Provider-neutral inference backend boundary and disabled mode.** The target explicitly requires model independence and usefulness without inference; both exist in [`crates/familiar-llm/src/backend.rs`](../../crates/familiar-llm/src/backend.rs) and [`crates/familiar-llm/src/router.rs`](../../crates/familiar-llm/src/router.rs).
- **Loopback-first lightweight dashboard and native platform paths.** These match the target host and HTTP interface direction in [`dashboard.rs`](../../crates/familiar-daemon/src/dashboard.rs) and [`paths.rs`](../../crates/familiar-core/src/paths.rs).

"Preserved unchanged" means the capability and architectural intent should remain. Localized defects identified above still require correction.

## Capabilities That Require Extension

- The existing project/decision/rollup schema into canonical task, finding, test, handoff, warrant, approval, and audit state.
- Repository watching into content-hashed, revision-aware intelligence with provenance and complete lifecycle reconciliation.
- Deterministic summaries into layered file, module, subsystem, dependency, and diff intelligence without displacing source.
- Task packing into reproducible, task-specific, agent-aware context compilation.
- Existing test and tool awareness into a production verification evidence service.
- Daemon background coordination into bounded worktree execution under warrants.
- Provider-neutral inference into capability-based coding-agent and reviewer adapters.
- Decision and rollup memory into typed, attributable, approval-aware engineering memory.
- Platform-specific paths, signals, and tray support into explicit macOS/Linux host capability reporting and enforcement.
- Logging into separately modeled audit and evidence lineage.
- Local dashboard and MCP access into appropriately authenticated, least-privilege client boundaries.

These are missing target capabilities. Their absence should not be described as failure of code that was never intended to provide them.

## Capabilities That Require Structural Refactoring

- **Authority ownership:** mutable state, policy, and runtime lifecycle must converge in the central daemon.
- **Interface boundaries:** MCP and HTTP must invoke shared core services rather than own storage and business logic.
- **File identity:** absolute host paths and repository-relative durable paths must become one canonical identity model.
- **Concurrency boundary:** synchronous filesystem and SQLite work must not block the daemon's asynchronous control plane.
- **Runtime status:** inferred process-local flags must become observed daemon-owned lifecycle state.

These refactors should retain working storage, protocol, watcher, summary, and UI behavior wherever compatible.

## Proposed Replacements, If Any

No working subsystem should be replaced wholesale based on current evidence.

The analysis finds no technical justification to replace:

- Rust or Tokio.
- SQLite or `rusqlite`.
- The existing watcher libraries.
- MCP stdio transport.
- The deterministic summary extractor.
- The OpenAI-compatible inference backend.
- The lightweight Axum dashboard.
- Native tray support.

The only removal recommendation is GAP-020: eliminate direct mutable database and inference-lifecycle authority from MCP clients once those operations are available through the central daemon. This replaces an authority path, not the MCP capability or its user-facing tools.

## Unresolved Questions Requiring Human Architectural Decisions

1. **Canonical event model:** Should workflow state use an append-only event model with projections, conventional transactional tables plus audit events, or a defined hybrid? The target requires auditability but does not prescribe storage mechanics.
2. **Approval identity:** What establishes a trusted human identity locally, and which interfaces may capture approval for each risk class?
3. **Warrant representation:** What signature, attribution, revocation, and expiration mechanism is sufficient for local unattended execution?
4. **Host isolation baseline:** Which sandbox guarantees are mandatory on macOS and Linux, and which capability gaps require refusal versus explicit human acceptance?
5. **Merge and publication authority:** Is Familiar permitted to merge, push, open pull requests, release, or deploy under warrants, or must some actions always remain interactive human operations?
6. **Decision governance:** Which decision categories require explicit human approval, and how are proposed, accepted, superseded, rejected, and expired decisions represented?
7. **Verification policy ownership:** Are required build, test, lint, invariant, and comparison commands declared in repository-owned policy, Familiar-local policy, or both, and which source prevails?
8. **Audit retention and tamper evidence:** What retention period, artifact size policy, redaction policy, and integrity mechanism are required for a local-first system?
9. **Agent privacy policy:** Under what conditions may source, diffs, findings, or memory be sent to remote models, and is approval per project, task, provider, or disclosure event?
10. **Context equivalence:** How much variation between vendor-specific context renderings is acceptable while preserving equivalent evidence and constraints?
11. **Cross-project knowledge:** The target defaults to isolation but permits explicit authorization. What records may ever cross boundaries, and how is that authority represented and revoked?
12. **Failure authority:** Which failed checks or unresolved finding severities may a human explicitly accept, and which invariants are non-waivable?
13. **Rollback claims:** Which external actions may Familiar call reversible, and what deterministic proof of rollback readiness is required before execution?
14. **Local daemon availability:** Should thin clients fail closed when the daemon is unavailable, or may any read-only degraded mode operate from a snapshot without creating competing authority?

These questions affect authority, trust, and canonical semantics. They require explicit human architectural decisions rather than implementation-level assumptions.
