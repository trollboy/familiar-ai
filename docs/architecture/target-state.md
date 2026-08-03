# Familiar: Target-State Architecture

## Status and Scope

This document defines the desired end-state architecture for Familiar. It is normative architectural direction, not a migration plan, implementation plan, or product requirements document.

The target state is grounded in `docs/philosophy.md`, the current-state baseline, the repository, and the existing product requirements. Working systems remain valid unless evidence shows that they violate an invariant or cannot satisfy the target responsibilities.

## Executive Summary

Familiar is the persistent, local, agent-neutral engineering steward for a software project. It preserves project intelligence, workflow state, engineering memory, execution policy, verification evidence, and architectural continuity while delegating implementation to interchangeable coding agents.

A single central daemon owns canonical Familiar state and exposes shared core services. Claude Code, Codex, Cursor, OpenCode, and future agents connect through thin clients. MCP, CLI, a local operating-system socket, and loopback HTTP are protocol adapters over the same application services; they do not implement independent business logic or maintain competing runtime state.

The repository and its version-control history remain authoritative. Familiar derives content-hashed repository intelligence, summaries, indexes, and context artifacts from source, but every derived artifact carries provenance and can be invalidated or rebuilt. Context is compiled for a specific task under an explicit token budget rather than accumulated opportunistically.

Familiar supervises bounded work through isolated worktrees and explicit execution warrants. It verifies results with deterministic tests, logs, diffs, policy checks, and invariant checks, then coordinates independent, adversarial review by models that did not perform the implementation. Human approval remains mandatory at architectural, policy, privilege, publication, and other defined risk gates. All material actions and conclusions are auditable, and every accepted change retains a clear rollback path.

## Architectural Role

Familiar owns stewardship, not implementation.

It is responsible for:

- Maintaining canonical project and workflow state.
- Preserving durable engineering memory and explicit decisions.
- Observing repositories and compiling trustworthy project intelligence.
- Constructing minimal, task-specific context for agents.
- Selecting and coordinating replaceable implementation and review agents.
- Enforcing execution policy and human approval gates.
- Running deterministic verification and preserving its evidence.
- Maintaining an auditable history from authorization through handoff.
- Protecting architectural continuity across agents, editors, models, and sessions.

Coding agents remain external workers. They receive bounded objectives, context, constraints, capabilities, and verification requirements. They return proposed changes, findings, and evidence. They do not become the source of truth for project state and cannot declare their own work accepted.

## Target Architecture

```text
Claude Code   Codex   Cursor   OpenCode   Future Agents   Human Interfaces
     │          │       │         │             │                 │
     └──────────┴───────┴─────────┴─────────────┴─────────────────┘
                                  │
                 Thin protocol and client adapters
                    MCP │ CLI │ local socket │ HTTP
                                  │
                 ┌────────────────────────────────┐
                 │     Familiar Central Daemon    │
                 │                                │
                 │  Application/Core Services     │
                 │  Workflow and Policy Engine    │
                 │  Context Compiler              │
                 │  Repository Intelligence       │
                 │  Verification and Review       │
                 │  Execution Supervisor          │
                 │  Audit and Handoff              │
                 └───────────────┬────────────────┘
                                 │
                Canonical state, derived indexes, evidence
                                 │
                    Repository and host capabilities
                    Git │ worktrees │ tests │ tools
```

The central daemon is the sole authority for mutable Familiar workflow state. All interfaces invoke the same core commands and queries. Clients may cache disposable presentation data, but they do not own canonical state, independently route policy, or silently mutate project memory.

## Canonical Domain State

Familiar maintains a coherent, durable domain model. Every record belongs to a project and carries stable identity, timestamps, provenance, and audit metadata.

### Project

A project represents a repository under Familiar stewardship. Its canonical state includes:

- Stable project identity and canonical repository root.
- Repository identity and version-control metadata.
- Active branches and approved worktrees.
- Project policies, invariants, commands, and approval rules.
- Ignore rules and intelligence configuration.
- Context and execution budgets.
- Current health and indexing state.

Repository paths are stored in a canonical repository-relative form. Host-specific absolute paths are runtime locations, not durable content identity.

### Task

A task is the unit of bounded work. It records:

- Explicit objective and measurable completion criteria.
- Scope, allowed paths, and prohibited changes.
- Owning project, base revision, branch, and worktree.
- Assigned implementation and review agents.
- Required context, policies, decisions, and invariants.
- Verification plan and required evidence.
- Execution warrant and approval state.
- Lifecycle state, attempts, outcomes, and stopping point.

Task state is explicit and monotonic where possible. A conversation is not task state.

### Decision

A decision records a human-owned architectural or engineering choice:

- Title, rationale, status, and scope.
- Author and explicit approver.
- Related tasks, findings, files, and earlier decisions.
- Alternatives considered and consequences.
- Confidence and evidence.
- Effective and superseded revisions.

Agents may propose or challenge decisions. They cannot silently establish architectural decisions.

### Finding

A finding is an independently reviewable observation produced by a human, agent, or deterministic system. It includes:

- Severity, category, confidence, and status.
- Precise repository location or artifact reference.
- Claim, supporting evidence, and reproduction instructions.
- Originating reviewer or verification system.
- Resolution, disposition, or explicit acceptance of risk.

Findings are distinct from decisions and tasks so review evidence remains durable even when work is deferred.

### Test and Verification Result

A test result records deterministic evidence:

- Exact command or verification operation.
- Tool and environment identity.
- Base and tested revisions.
- Exit status, duration, structured result, and retained logs.
- Relevant hashes and artifact references.
- Whether the result satisfied a declared task requirement.

A passing status without reproducible evidence is not a valid verification result.

### Handoff

A handoff is the durable boundary between workers, stages, or human review. It contains:

- Task and revision identity.
- Work completed and work remaining.
- Decisions made or requested.
- Changed files and summarized diff.
- Verification performed and evidence references.
- Open findings, risks, assumptions, and blockers.
- Recommended next action and required approval.

Handoffs are structured project records, not ephemeral chat summaries.

## Major Subsystems and Responsibilities

### 1. Central Daemon and Core Services

The central daemon is the long-lived authority and process coordinator. It owns:

- Canonical state transitions.
- Transaction boundaries and concurrency control.
- Project lifecycle and active-project resolution.
- Workflow scheduling and task lifecycle.
- Policy evaluation and approval enforcement.
- Repository observation and invalidation.
- Agent, verification, and host-capability coordination.
- Health, backpressure, recovery, and shutdown.

The daemon remains functional without any LLM. Model availability may improve derived intelligence or review quality, but it cannot be required for basic state access, policy enforcement, source inspection, or deterministic verification.

### 2. Interface and Client Adapters

Thin adapters serve Claude Code, Codex, Cursor, OpenCode, future agents, automation, and humans.

Supported interfaces are:

- **MCP:** Agent-facing tools and resources for context, task, memory, findings, verification, and handoff operations.
- **CLI:** Scriptable human and automation access, diagnostics, approvals, task control, and evidence inspection.
- **Local socket:** The primary authenticated local IPC channel for low-latency commands, events, and state synchronization.
- **Loopback HTTP:** Dashboard and local integrations requiring HTTP, streaming, or browser access.

All adapters translate protocol-specific requests into shared core commands and queries. They enforce protocol concerns such as framing, schema validation, authentication, and presentation, but contain no independent workflow rules. Identical operations have identical authorization and state semantics across interfaces.

An editor or agent integration is replaceable. No core record uses vendor-specific session state as its sole identity or source of truth.

### 3. Canonical State Store

The state store persists projects, tasks, decisions, findings, tests, handoffs, warrants, approvals, audit events, and references to derived artifacts.

Its responsibilities are:

- Transactional domain updates.
- Project isolation.
- Referential integrity.
- Schema evolution and recovery.
- Durable event and audit history.
- Query support for core services.
- Retention of provenance and revision identity.

Only core services mutate canonical state. Direct client database access is outside the boundary. Derived indexes may use specialized storage, but they cannot become the exclusive copy of canonical engineering knowledge.

### 4. Repository Intelligence Engine

The repository intelligence engine observes and analyzes source using deterministic mechanisms first. It maintains:

- Repository-relative file inventory.
- Content hashes and Git object identities.
- Language, symbol, module, dependency, and ownership metadata.
- Build, test, lint, and configuration discovery.
- Change, rename, and deletion reconciliation.
- Branch and worktree awareness.
- Cached file, module, subsystem, diff, and session summaries.
- Optional lexical and semantic indexes.

Every derived artifact is keyed by the content and configuration that produced it. A summary identifies its source hashes, generator, model or deterministic algorithm, prompt or ruleset version, timestamp, and scope. Unchanged content reuses valid artifacts. Changed content invalidates dependent artifacts. Missing or uncertain intelligence causes source inspection, not fabrication.

Semantic indexes and embeddings are optional acceleration structures. They are project-isolated, reconstructible, and never authoritative.

### 5. Engineering Memory and Decision Service

This subsystem preserves durable knowledge across conversations and agents:

- Architectural decisions and rationale.
- Project invariants and conventions.
- Accepted and unresolved findings.
- Task outcomes and implementation history.
- Handoffs and session rollups.
- Verification history and known failure modes.

Memory entries are typed, scoped, attributable, and connected to source revisions. Proposed knowledge remains distinguishable from human-approved knowledge. Superseded knowledge remains auditable rather than being silently overwritten.

### 6. Context Compiler

The context compiler produces a purpose-built context package for one task, one project state, and one receiving agent capability profile.

It:

- Resolves the task objective, scope, base revision, and acceptance criteria.
- Includes applicable human decisions, invariants, policies, findings, and handoffs.
- Selects repository evidence using hashes, diffs, symbols, dependencies, and relevance.
- Prefers validated summaries and structured metadata over unchanged raw source.
- Includes raw source when precision, novelty, or uncertainty requires it.
- Applies explicit section and total token budgets.
- Records omissions, truncation, provenance, and estimated or exact token cost.
- Produces a reproducible context manifest.

Context packages are immutable artifacts identified by their inputs. Agent-specific rendering may differ, but the underlying evidence and constraints remain equivalent.

### 7. Workflow and Execution Policy Engine

The workflow engine advances tasks through explicit states and applies project policy. It determines:

- Whether a task is sufficiently specified.
- Which approvals are required.
- Which agent capabilities are appropriate.
- Which execution warrant may be issued.
- Which verification and review stages are mandatory.
- Whether findings block acceptance.
- When work must stop and return to a human.

Policy is deterministic and inspectable. An LLM may advise policy evaluation but cannot grant itself authority, expand a warrant, waive a gate, or redefine acceptance criteria.

### 8. Worktree and Execution Supervisor

Implementation runs in isolated Git worktrees or equivalently isolated repository environments. The supervisor owns:

- Worktree creation, identity, base revision, and lifecycle.
- Process execution within explicit filesystem and command boundaries.
- Resource, time, concurrency, and network limits.
- Environment and dependency provenance.
- Checkpointing and intermediate evidence.
- Cancellation, timeout, and recovery.
- Diff containment and scope enforcement.

Unattended execution requires a signed or otherwise attributable execution warrant. A warrant defines:

- Objective and task identity.
- Base revision and authorized worktree.
- Allowed paths and operations.
- Permitted commands, tools, network access, and external side effects.
- Resource and time ceilings.
- Required checkpoints and verification.
- Explicit prohibitions and stop conditions.
- Expiration and revocation state.
- Human approver and approval evidence.

A warrant is bounded authority, not general autonomy. Agents cannot broaden it. When authority is insufficient or a stop condition occurs, execution halts safely and produces a handoff.

### 9. Deterministic Verification Engine

Verification treats agent claims as hypotheses and gathers reproducible evidence. It performs, as applicable:

- Test discovery and execution.
- Build, type-check, lint, format, and static-analysis checks.
- Exact diff and changed-path inspection.
- Test and application log parsing.
- Repository cleanliness and generated-artifact checks.
- Policy and architectural invariant checks.
- Dependency, schema, API, and migration compatibility checks.
- Reproduction of reported failures and fixes.
- Comparison against base-revision behavior where necessary.

Verification inputs, environment, commands, outputs, and hashes are retained as structured evidence. Logs are preserved without allowing untrusted output to alter policy or instructions. Passing tests are necessary where required but do not replace diff inspection, reasoning, or invariant validation.

### 10. Multi-Agent Review Coordinator

Review is independent and adversarial by design. The coordinator:

- Assigns review to an agent or model distinct from the implementer whenever practical.
- Supplies the objective, constraints, diff, evidence, and relevant source without exposing unnecessary implementation-chain reasoning.
- Requests falsification, edge-case discovery, invariant checking, and evidence-backed findings.
- Supports multiple reviewers with different models or review specialties for higher-risk work.
- Deduplicates findings without erasing disagreement.
- Requires each finding to cite evidence and disposition.
- Prevents the implementing agent from unilaterally resolving or accepting its own findings.

Model agreement is not proof. Deterministic verification and human approval remain independent signals.

### 11. Model and Agent Capability Router

The router describes agents and models by capabilities rather than vendor identity. It considers:

- Task and review role.
- Context-window and protocol constraints.
- Tool and modality support.
- Privacy, locality, cost, and latency policy.
- Observed reliability for the relevant work class.
- Availability and health.

Provider adapters translate the neutral task, context, and handoff contracts into vendor protocols. Provider-specific features may be used as optimizations but cannot become required for canonical state or workflow correctness.

### 12. Host Capability Layer

The host layer provides a stable abstraction over macOS and Linux capabilities:

- Filesystem watching and path normalization.
- Process and signal management.
- Local sockets and loopback networking.
- Git and worktree operations.
- Sandboxing and resource controls available on the host.
- Secure credential storage and retrieval.
- Service installation and lifecycle integration.
- Notifications, tray integration, and opening local resources.
- Container and toolchain discovery.

Platform differences are explicit capability facts. Core policy requests capabilities and handles their absence; it does not silently assume equivalent enforcement across operating systems.

### 13. Audit, Evidence, and Rollback Service

Every material operation produces an attributable audit event. The service records:

- Actor, interface, task, warrant, and project.
- Command or state transition.
- Inputs, outputs, revision hashes, and artifact references.
- Approval and policy decisions.
- Verification and review outcomes.
- External side effects.
- Timestamps and causal relationships.

Audit records are append-oriented and tamper-evident to the extent supported locally. Sensitive values are redacted while retaining useful provenance.

Rollback capability is preserved through Git history, isolated worktrees, checkpoints, reversible state transitions, database backups or journals, and explicit records of external effects. Familiar never represents an operation as safely reversible unless its rollback mechanism is known and verified.

## Subsystem Boundaries

The following boundaries are architectural, regardless of internal crate layout:

- **Interfaces versus core:** MCP, CLI, socket, and HTTP adapters validate and translate; core services decide and mutate.
- **Canonical state versus derived intelligence:** Domain records are durable authority; summaries, embeddings, caches, and indexes are disposable projections with provenance.
- **Steward versus worker:** Familiar authorizes and evaluates; coding agents implement within granted scope.
- **Policy versus reasoning:** Deterministic policy grants authority; model reasoning may recommend but cannot authorize.
- **Implementation versus review:** Implementer output and independent review remain separate evidence streams.
- **Workflow versus execution:** Workflow defines desired transitions; the supervisor performs bounded host actions.
- **Verification versus acceptance:** Verification establishes evidence; policy and humans determine whether evidence is sufficient for acceptance.
- **Project isolation:** State, intelligence, worktrees, credentials, and context do not cross project boundaries without explicit authorization.
- **Host capability versus portable core:** Platform adapters expose facts and mechanisms; portable core semantics remain stable.

## Core Data Flows

### Repository Observation and Intelligence

```text
Filesystem and Git events
  → canonical project and worktree resolution
  → deterministic inventory and content hashing
  → change/rename/deletion reconciliation
  → parser and repository analysis
  → dependency invalidation
  → cached derived artifacts and indexes
  → auditable intelligence state
```

Source content is read when hashes change, when a required artifact is absent, or when confidence is insufficient. A valid content-hashed result is reused across sessions and agents.

### Task Definition and Authorization

```text
Human or authorized client proposes task
  → task objective, scope, criteria, and base revision recorded
  → applicable policy, decisions, invariants, and risk resolved
  → required human approval gates evaluated
  → bounded execution warrant issued
  → isolated worktree assigned
```

No unattended implementation begins without a valid task, base revision, stopping point, and warrant.

### Context Compilation and Agent Dispatch

```text
Authorized task
  → resolve agent capability profile and token ceiling
  → gather canonical constraints and durable memory
  → select content-hashed repository intelligence
  → inspect authoritative source where required
  → allocate explicit section budgets
  → emit immutable context package and manifest
  → dispatch to replaceable implementation agent
```

The resulting package makes exclusions and truncation visible. It never presents derived summaries as authoritative source.

### Bounded Implementation

```text
Agent receives task, context, warrant, and worktree
  → executes permitted operations
  → supervisor enforces scope and resource limits
  → checkpoints, logs, and diff are captured
  → stop condition, completion claim, or blocked handoff
```

Changes outside the warrant are rejected or cause execution to stop. External side effects require specific authority.

### Verification, Review, and Acceptance

```text
Implementation result
  → deterministic build/test/lint/log/diff/invariant verification
  → structured evidence bundle
  → independent adversarial model review
  → findings triage and resolution
  → required human approval
  → accept, return for revision, reject, or roll back
```

The implementing model cannot be the sole reviewer or acceptance authority. Failed or inconclusive checks remain visible; they are not converted into success.

### Handoff and Durable Memory

```text
Task stage or session ends
  → structured handoff generated from canonical state and evidence
  → decisions and findings explicitly proposed or recorded
  → task state and next required action committed
  → future context compilation consumes durable records
```

Conversation history may inform a handoff, but only committed structured state survives as project memory.

## Architectural Invariants

1. The repository and its version-control history are authoritative for source and change state.
2. Canonical Familiar workflow state has one owning core, regardless of access protocol.
3. All durable file identities are project-scoped and repository-relative; content identity is hash-based.
4. Derived summaries, embeddings, indexes, and caches carry provenance and are reconstructible.
5. Deterministic mechanisms are used before model inference whenever they can answer correctly.
6. Every model invocation has an explicit purpose, bounded input, selected provider policy, and observable result.
7. Every task has an objective, scope, completion criteria, owner, base revision, and stopping point.
8. Unattended execution occurs only in isolation under a valid, bounded, revocable warrant.
9. Agents cannot expand their own authority or waive approval and verification requirements.
10. Architectural changes require explicit human approval and durable decision records.
11. No implementation agent is the sole authority declaring its work correct.
12. Acceptance uses both engineering reasoning and deterministic evidence appropriate to risk.
13. Project data and context remain isolated unless a human explicitly authorizes cross-project use.
14. Every material state transition and host action is attributable and auditable.
15. Failures, skipped checks, uncertainty, and disagreement remain visible.
16. Accepted changes retain a defined rollback path and the evidence needed to understand it.
17. Core semantics do not depend on a specific editor, coding model, model provider, or proprietary protocol.
18. Familiar remains useful with model inference disabled.

## Trust Boundaries

### Human Authority Boundary

Humans are the final authority for architecture, policy, risk acceptance, privilege expansion, publication, and other configured approval gates. Familiar may organize evidence and recommend action but cannot impersonate human approval.

### Client Boundary

MCP clients, editors, CLIs, dashboards, and third-party integrations are untrusted callers. They authenticate locally as appropriate, receive least privilege, and cannot bypass core validation by selecting a different interface.

### Agent and Model Boundary

Agent output is untrusted proposed work. Prompts, generated code, summaries, reviews, tool requests, and completion claims require validation. Models do not receive secrets or unrelated project context unless explicitly required and authorized.

### Repository Content Boundary

Repository files, issue text, logs, test output, generated artifacts, and dependency metadata may contain malicious or misleading instructions. They are treated as data, not authority. Content cannot modify policy or warrants merely by containing imperative text.

### Execution Boundary

The worktree and supervised process environment separate agent execution from the host and canonical repository. Filesystem, process, network, credential, and external-service access are denied unless granted by the warrant and supported by host enforcement.

### Provider and Network Boundary

Remote model providers and APIs are external systems. Data disclosure follows project privacy policy, credentials remain in the host credential boundary, and remote responses are treated as untrusted. Local-only operation remains possible.

### Derived Intelligence Boundary

Cached summaries, embeddings, rankings, and model-produced memory can be stale or wrong. Consumers receive provenance and confidence and fall back to authoritative source whenever correctness depends on it.

### Verification Tool Boundary

Compilers, tests, linters, containers, and scripts provide evidence but may themselves be incomplete, compromised, flaky, or misconfigured. Familiar records exact tools and environments, distinguishes absence from success, and applies multiple independent checks where risk warrants it.

## Human Approval Gates

Approval gates are explicit domain state, not conversational implication. At minimum, Familiar requires configurable human approval for:

- Creation or material expansion of an unattended execution warrant.
- Architectural decisions and changes to declared invariants.
- Operations outside the repository or approved worktree.
- Credential use, network publication, deployment, release, merge, or other external side effects.
- Destructive or difficult-to-reverse actions.
- Acceptance of unresolved high-risk findings or failed required verification.
- Changes to Familiar's own execution and approval policy.

Approvals identify the human, exact scope, revision or artifact, timestamp, and expiration where applicable. A later or broader operation requires a new approval.

## Editor and Model Independence

Familiar's domain model and core APIs use neutral concepts: project, task, context package, warrant, decision, finding, verification result, review, and handoff. They do not encode Claude-, Codex-, Cursor-, or OpenCode-specific assumptions.

Agent adapters advertise capabilities and translate neutral contracts. If a vendor disappears, only its adapter and capability metadata disappear. Canonical state, repository intelligence, workflows, evidence, and audit history remain intact.

MCP is an important interoperability interface, not the internal architecture. CLI, socket, and HTTP clients exercise the same core services, preventing any one protocol from becoming a second source of behavior.

## macOS and Linux Host Model

Familiar provides equivalent core semantics on macOS and Linux while acknowledging different enforcement mechanisms.

On both platforms it supports:

- Native background service lifecycle.
- Platform-correct configuration, data, state, runtime, and log locations.
- Local IPC with per-user access control.
- Filesystem watching and repository discovery.
- Git worktree isolation.
- Process supervision and cancellation.
- Secure credential integration where available.
- Loopback dashboard and notifications.
- Capability reporting for sandboxing, containers, toolchains, and resource limits.

When a host cannot enforce a requested isolation or resource guarantee, Familiar reports the missing capability and refuses warrants that require it unless a human explicitly chooses an acceptable alternative. It does not claim cross-platform equivalence that the host cannot provide.

## Auditability and Rollback

For any task, a human can reconstruct:

- Who or what requested and approved it.
- Which source revision and worktree were used.
- Which context, decisions, policies, and warrant governed execution.
- Which agent and model performed each role.
- Which commands ran and which external effects occurred.
- What changed and why.
- Which tests and checks ran, with exact evidence.
- Which reviewers examined the result and what they found.
- Who accepted the result and which risks remained.

Rollback is designed into the workflow. Proposed changes remain isolated until accepted. Canonical records retain supersession history. External operations declare whether and how they can be reversed. Familiar favors recoverable actions and stops before destructive actions whose target, authority, or rollback is unclear.

## What Familiar Must Never Become

Familiar must never become:

- The canonical source of truth for source code in place of the repository.
- A system that allows summaries, embeddings, or model memory to override source.
- A vendor-specific wrapper for Claude, Codex, Cursor, OpenCode, or any future agent.
- An editor or IDE whose core value depends on owning the user interface.
- A chatbot with durable state hidden inside conversation history.
- A general autonomous-agent framework pursuing open-ended goals without bounded warrants.
- A mechanism for agents to grant themselves permissions or approve their own work.
- A system in which the same model both implements and solely certifies correctness.
- A prompt library substituting prose for deterministic policy and verification.
- A LangGraph-style orchestration abstraction whose workflow complexity obscures engineering state.
- A general-purpose vector database or knowledge platform detached from repository stewardship.
- A replacement for Git, compilers, test suites, issue trackers, CI systems, or human architectural judgment.
- A silent response-rewriting or man-in-the-middle layer.
- A cloud dependency required for local project continuity.
- A source of hidden assumptions, unverifiable success, or silent architectural drift.
- An automation engine optimized for code volume rather than correctness, safety, repeatability, velocity, and observability.

## Final Direction

Familiar remains the durable steward while coding agents, models, editors, and protocols change around it. It prepares bounded work, supplies intentional context, enforces policy, gathers deterministic evidence, coordinates independent review, and presents humans with explicit engineering decisions.

The engineer remains responsible. The repository remains truth. The coding agents remain replaceable.
