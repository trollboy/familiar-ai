# Familiar Architectural Roadmap

## Purpose

This document orders the approved architectural gaps into milestones that move Familiar from its current local project-memory architecture toward the target persistent engineering steward.

It is an architectural roadmap, not a collection of PRDs or an implementation plan. It defines dependency order, architectural outcomes, and human gates. It does not prescribe tickets, code changes, release dates, or internal implementation mechanics.

## Roadmap Principles

The roadmap follows these constraints from the governing architecture documents:

- The repository remains authoritative.
- Deterministic mechanisms precede model inference.
- Humans own architecture, policy, and risk acceptance.
- Work remains small, bounded, measurable, and stoppable.
- Existing working systems are extended or refactored rather than replaced without evidence.
- SQLite, the Rust daemon, repository watcher, deterministic extractor, MCP transport, token controls, inference backend boundary, dashboard, and tray remain foundations.
- Canonical mutable authority converges in the daemon without removing useful client capabilities.
- Each milestone preserves a working repository and a coherent supported operating mode.
- New authority is not exercised before policy, approval, verification, audit, and rollback foundations exist.

## Milestone Dependency Overview

```text
M0 Architectural Authority Decisions
 │
 ├── M1 Repository Identity and Reconciliation
 │    └── M5 Content-Addressed Repository Intelligence
 │         └── M6 Reproducible Context Compilation and Agent Contracts
 │              └── M9 Independent Adversarial Review
 │
 └── M2 Central Core Authority and Shared Interfaces
      ├── M3 Responsive Control Plane and Host Trust Baseline
      │    └── M7 Deterministic Verification
      │         └── M8 Bounded Isolated Execution
      │              └── M9 Independent Adversarial Review
      │
      └── M4 Canonical Stewardship State, Policy, Audit, and Memory
           ├── M6 Reproducible Context Compilation and Agent Contracts
           ├── M7 Deterministic Verification
           └── M8 Bounded Isolated Execution

Required target-state foundation complete after M9

Optional enhancements:
  M10 Optional Semantic Intelligence Acceleration ← M5, M6
  M11 Expanded Agent and Client Ecosystem          ← M6, M9
```

M1 and M2 may proceed independently after M0 because repository identity correction does not require centralized IPC, while central authority does not require richer repository intelligence. Their dependent milestones converge before context compilation and supervised execution.

## Foundational Milestones

### M0 — Architectural Authority Decisions

**Goal**

Resolve the human-owned architectural questions that determine canonical semantics and trust: state/event model, approval identity, warrant attribution, minimum host isolation, publication authority, decision governance, verification-policy ownership, audit retention, remote-provider disclosure, context equivalence, cross-project use, waivable failures, rollback claims, and daemon-unavailable behavior.

This milestone records decisions; it does not introduce runtime capability.

**Gaps addressed**

- Enables resolution of GAP-001, GAP-002, GAP-008, GAP-010, GAP-011, GAP-012, GAP-014, GAP-017, and GAP-018.
- Resolves no implementation gap by itself.

**Dependencies**

- `docs/philosophy.md`.
- `docs/architecture/target-state.md`.
- The unresolved human questions in `docs/architecture/gap-analysis.md`.

**Expected architectural outcome**

The authority, trust, and canonical-state choices needed by later milestones are explicit and durable. Later work does not invent policy through implementation detail.

The repository remains fully working because this milestone changes architectural authority records only.

**Human approval gate before proceeding**

A human architect must explicitly approve the decisions that affect the next milestone's scope. No canonical-state, policy, approval, warrant, isolation, or audit mechanism proceeds on implicit assumptions.

### M1 — Repository Identity and Reconciliation

**Goal**

Establish one project-scoped, repository-relative durable file identity and make repository intelligence accurately reflect changed, removed, renamed, and scanned files.

**Gaps addressed**

- Resolves GAP-003: File identity is inconsistent.
- Resolves GAP-005: File lifecycle reconciliation is incomplete.

**Dependencies**

- M0 decisions concerning repository identity and rollback of stored derived state.
- Existing project identity, watcher events, summary pipeline, storage repositories, and path-containment behavior.

**Expected architectural outcome**

Daemon-generated and client-requested summaries refer to the same durable file identity. Module queries, cache lookups, and project portability no longer depend on host absolute paths. Delete and rename events reconcile persistent derived state, and scan completeness is observable rather than silently partial.

The existing watcher, deterministic summaries, MCP tools, and SQLite records remain operational throughout the transition. Compatibility handling prevents existing indexed projects from becoming unreadable.

**Human approval gate before proceeding**

A human architect must approve the canonical path semantics, treatment of existing absolute-path records, rename identity rules, and whether deletion uses tombstones or removal before content-addressed intelligence depends on them.

### M2 — Central Core Authority and Shared Interfaces

**Goal**

Make the existing daemon the sole authority for mutable Familiar state and runtime lifecycle while retaining MCP, dashboard, tray, and storage capabilities through shared application services.

**Gaps addressed**

- Resolves GAP-001: Canonical core authority is fragmented.
- Resolves GAP-010: Interfaces are not thin adapters over shared services.
- Resolves GAP-016: Runtime status is not authoritative.
- Resolves GAP-020: Duplicate mutable authority outside the daemon must disappear.

**Dependencies**

- M0 decisions concerning daemon-unavailable behavior, local client identity, and canonical command/query semantics.
- Existing daemon composition, SQLite repositories, MCP server and transport, dashboard, tray, and platform socket paths.

**Expected architectural outcome**

One daemon-owned core provides canonical commands, queries, runtime health, and state transitions. MCP, local socket, loopback HTTP, dashboard, tray, and subsequent CLI access share those semantics. MCP no longer owns a competing writable database connection, status snapshot, policy path, or inference lifecycle.

Current MCP tools and local UI remain usable through compatibility-preserving adapters. The repository remains functional in a clearly defined daemon-present mode; any approved degraded read-only behavior cannot create competing authority.

**Human approval gate before proceeding**

A human architect must approve the core command/query boundary, local IPC trust model, daemon availability contract, status semantics, and the point at which direct MCP mutation authority is removed.

### M3 — Responsive Control Plane and Host Trust Baseline

**Goal**

Ensure the central daemon can remain responsive under indexing, storage, Git, filesystem, verification, and future execution workloads, while exposing explicit macOS and Linux host capabilities and enforcing least-privilege interface and secret boundaries.

**Gaps addressed**

- Resolves GAP-015: Blocking work is performed inside asynchronous services.
- Resolves the host-capability foundation of GAP-014: Host capabilities are implicit rather than governed.
- Resolves the core/client/credential foundation of GAP-017: Trust boundaries and secret handling are incomplete.

**Dependencies**

- M0 decisions concerning minimum isolation, credential handling, and client trust.
- M2 central authority and shared interface boundary.
- Existing bounded queues, platform paths, signal handling, and local-first networking.

**Expected architectural outcome**

Blocking work has explicit execution boundaries, backpressure, cancellation, and health visibility without requiring replacement of SQLite or the current deterministic extractor. The daemon reports enforceable host capabilities rather than assuming macOS/Linux equivalence. Local interfaces use consistent trust semantics, remote credentials live behind the approved host credential boundary, and loopback services cannot silently broaden exposure.

Existing indexing, MCP, dashboard, and tray operation remains available. A host lacking a capability reports degradation but does not falsely claim enforcement.

**Human approval gate before proceeding**

A human architect must approve the concurrency ownership model, minimum responsive-control-plane guarantees, macOS/Linux capability vocabulary, credential boundary, and fail-closed behavior for unavailable security capabilities.

### M4 — Canonical Stewardship State, Policy, Audit, and Memory

**Goal**

Extend the existing project, decision, and rollup model into the canonical engineering state required for stewardship, with deterministic policy, explicit human approvals, durable evidence lineage, and approval-aware memory.

**Gaps addressed**

- Resolves GAP-002: Canonical engineering domain state is incomplete.
- Resolves GAP-011: Human approval and deterministic policy are absent.
- Resolves GAP-012: Auditability, evidence lineage, and rollback state are absent.
- Resolves GAP-018: Durable memory lacks provenance and approval semantics.

**Dependencies**

- M0 decisions concerning state/event mechanics, human identity, decision governance, audit retention, rollback claims, and waivable failures.
- M2 central authority and shared command/query semantics.
- Stable project identity.

**Expected architectural outcome**

Projects, tasks, decisions, findings, test results, handoffs, warrants, approvals, and audit events have canonical project-scoped identities and explicit state semantics. Existing decisions and rollups remain useful records but gain clear provenance, status, relationships, and supersession rather than being reinterpreted as tasks or approvals.

Policy can determine when approval is required but grants no execution capability yet. The system remains a working memory and workflow-state service before any unattended execution is introduced.

**Human approval gate before proceeding**

A human architect must approve the domain boundaries, state transitions, non-waivable invariants, approval semantics, audit truth model, retention policy, and rollback representation before context, verification, or execution relies on them.

### M5 — Content-Addressed Repository Intelligence

**Goal**

Extend the corrected repository pipeline into content-hashed, revision-aware, provenance-bearing intelligence whose derived artifacts remain explicitly subordinate to authoritative source.

**Gaps addressed**

- Resolves GAP-004: Repository intelligence lacks content identity and provenance.
- Resolves GAP-019: Derived intelligence is not explicitly subordinate to source.

**Dependencies**

- M1 canonical file identity and lifecycle reconciliation.
- Stable repository, revision, branch, and worktree identity from approved domain semantics.
- Existing watcher, deterministic language/symbol extraction, summary generator, and SQLite storage.

**Expected architectural outcome**

File inventory and derived summaries are keyed to content and generating configuration. Provenance identifies source hashes, revisions, generator or model identity, and scope. Valid unchanged artifacts are reusable; changed or deleted inputs invalidate dependents. Uncertain or missing intelligence triggers authoritative source inspection.

The deterministic extractor remains the baseline, and Familiar remains fully useful with model inference disabled. Optional semantic indexing is not required for completion of this milestone.

**Human approval gate before proceeding**

A human architect must approve content and artifact identity, provenance requirements, invalidation rules, branch/worktree semantics, and the conditions that force source inspection.

### M6 — Reproducible Context Compilation and Agent Contracts

**Goal**

Evolve task packing into an immutable, task-specific context compiler and define vendor-neutral contracts for implementation and review agents.

**Gaps addressed**

- Resolves GAP-006: Context packing is not a reproducible context compiler.
- Resolves GAP-013: Agent neutrality stops at model inference.

**Dependencies**

- M4 canonical task, decision, finding, handoff, policy, and approval state.
- M5 content-addressed intelligence and authoritative-source fallback.
- Existing token ceilings, truncation warnings, packer profiles, and provider-neutral inference concepts.
- M0 decisions concerning remote disclosure and cross-provider context equivalence.

**Expected architectural outcome**

Every agent receives a reproducible context artifact tied to a task, project revision, constraints, decisions, findings, handoffs, source evidence, token allocation, omissions, and provenance. Vendor-specific renderings preserve equivalent authority and evidence through neutral task, context, result, finding, and handoff contracts.

Existing `context.pack_for_task` behavior remains available as a compatible surface over the compiler. No agent receives execution authority from context alone.

**Human approval gate before proceeding**

A human architect must approve the neutral agent contracts, context manifest, budget semantics, required source-fallback rules, remote disclosure policy, and acceptable equivalence among Claude Code, Codex, Cursor, OpenCode, and future adapters.

### M7 — Deterministic Verification and Evidence

**Goal**

Make deterministic verification a central service that can discover or apply approved project checks and retain reproducible evidence without granting Familiar implementation authority.

**Gaps addressed**

- Resolves GAP-007: Deterministic verification is not a core service.
- Completes the verification-tool portion of GAP-017 trust boundaries.
- Uses the audit and test-result capabilities introduced for GAP-012 and GAP-002.

**Dependencies**

- M3 responsive worker and host capability boundaries.
- M4 canonical tasks, test results, policy, approvals, audit, and evidence references.
- M0 decisions concerning verification-policy ownership, failure acceptance, audit retention, and rollback claims.
- Existing project compilers, tests, linters, Docker environments, Git, and logs remain external authoritative tools.

**Expected architectural outcome**

Familiar can record exact commands, tools, environments, base and tested revisions, results, logs, hashes, skips, and failures. It can inspect diffs and declared invariants and can determine whether required evidence exists, without treating tests alone or an LLM review as proof.

Verification is initially usable as a human-invoked or workflow-invoked service. The repository remains working even when a project has no discovered checks; absence is reported explicitly rather than converted into success.

**Human approval gate before proceeding**

A human architect must approve verification-policy precedence, allowed verification commands, environment trust, required evidence classes, treatment of flaky or absent checks, log retention, and non-waivable failures before verification can gate execution or acceptance.

### M8 — Bounded Isolated Execution

**Goal**

Introduce supervised coding-agent execution only within isolated worktrees or equivalent isolation and only under explicit, bounded, attributable, revocable warrants.

**Gaps addressed**

- Resolves GAP-008: Bounded isolated execution and warrants are absent.
- Completes execution-boundary portions of GAP-014 and GAP-017.

**Dependencies**

- M3 explicit host capabilities, secure credentials, and responsive control plane.
- M4 canonical tasks, warrants, approvals, policy, handoffs, audit, and rollback state.
- M6 neutral agent and context contracts.
- M7 deterministic verification and evidence.
- M0 decisions concerning warrant representation, minimum isolation, publication authority, external effects, and rollback claims.

**Expected architectural outcome**

An implementation agent can work only against an identified base revision in an approved isolated environment, under explicit path, command, network, resource, time, checkpoint, and external-effect limits. Agents cannot expand authority. Stop conditions produce a durable handoff. Verification and evidence are available before any result can be accepted.

Unattended execution is additive and opt-in. Existing memory, indexing, context, MCP, CLI, dashboard, and human-driven workflows continue to operate when execution is disabled or unsupported by the host.

**Human approval gate before proceeding**

A human architect must approve the warrant schema, approver identity, revocation and expiration semantics, mandatory isolation baseline, allowed effect classes, stop conditions, checkpoint requirements, rollback guarantees, and initial scope of unattended authority.

### M9 — Independent Adversarial Review and Acceptance

**Goal**

Complete the target stewardship loop by separating implementation, deterministic verification, independent adversarial review, finding disposition, human approval, and acceptance.

**Gaps addressed**

- Resolves GAP-009: Independent adversarial review is absent.
- Completes the target use of canonical findings, verification evidence, agent neutrality, approvals, audit, and handoffs introduced for GAP-002, GAP-007, GAP-011, GAP-012, GAP-013, and GAP-018.

**Dependencies**

- M4 canonical findings, decisions, approvals, audit, and handoffs.
- M6 neutral reviewer contracts and reproducible context.
- M7 deterministic verification evidence.
- M8 bounded implementation results and worktree identity.
- M0 decisions concerning finding severity, waivable failures, acceptance authority, and reviewer independence.

**Expected architectural outcome**

Implementation claims, deterministic evidence, and independent review are distinct attributable streams. A reviewer different from the implementer attempts to falsify correctness and records evidence-backed findings. Disagreement and failed checks remain visible. Findings require explicit disposition, and configured human gates control acceptance and risk.

The repository remains working after every review outcome: accepted work remains isolated until approved, rejected work can be discarded, revisions can return for bounded correction, and blocked work produces a durable handoff.

**Human approval gate before proceeding**

A human architect must approve reviewer-independence requirements, severity and disposition semantics, acceptance policy, model-versus-deterministic evidence weighting, mandatory human gates, and the conditions under which a task may be closed or rolled back.

## Optional Enhancement Milestones

Optional milestones may improve efficiency, reach, or review depth. They are not prerequisites for the target stewardship foundation and must not weaken deterministic operation, source authority, project isolation, or human gates.

### M10 — Optional Semantic Intelligence Acceleration

**Goal**

Add optional project-isolated semantic retrieval or model-assisted derived summaries where measured evidence shows that deterministic and lexical intelligence is insufficient.

**Gaps addressed**

- Enhances the capability established for GAP-004.
- Enhances the context quality established for GAP-006.
- Does not resolve a new mandatory architectural gap.

**Dependencies**

- M5 content-addressed provenance, invalidation, and source-authority rules.
- M6 context manifests, budgets, and omission reporting.
- Approved privacy, provider, cost, and local-model policy.

**Expected architectural outcome**

Embeddings, semantic indexes, and model-derived summaries act only as reconstructible acceleration structures. Their inputs, model or algorithm, configuration, hashes, and confidence are recorded. The deterministic and inference-disabled operating mode remains fully supported.

The existing lexical search, deterministic extraction, and source fallback remain available, allowing this enhancement to be disabled or removed without loss of canonical knowledge.

**Human approval gate before proceeding**

A human architect must approve evidence that semantic acceleration materially improves retrieval, along with privacy, cost, provenance, invalidation, project-isolation, and source-fallback guarantees.

### M11 — Expanded Agent and Client Ecosystem

**Goal**

Broaden thin integrations and review specialization after neutral contracts and the complete stewardship loop are stable.

**Gaps addressed**

- Enhances the shared-interface capability established for GAP-010.
- Enhances agent neutrality established for GAP-013.
- Enhances multi-model review established for GAP-009.
- Does not resolve a new mandatory architectural gap.

**Dependencies**

- M2 shared core services and thin adapter boundary.
- M6 neutral agent contracts and context equivalence.
- M9 independent review and acceptance semantics.
- Measured demand for each additional adapter or specialty reviewer.

**Expected architectural outcome**

Claude Code, Codex, Cursor, OpenCode, and future agents can be added or removed through thin adapters without changing canonical state or workflow semantics. Additional specialist reviewers may participate as independent evidence sources without turning model consensus into proof.

The system remains operational when any one editor, model, provider, or adapter is unavailable. Existing MCP, CLI, local-socket, and HTTP interfaces retain equivalent authorization behavior.

**Human approval gate before proceeding**

A human architect must approve evidence of need, capability and privacy mappings, context-equivalence behavior, vendor-specific exceptions, maintenance cost, and confirmation that the adapter introduces no new authority path.

## Target-State Completion Boundary

M0 through M9 establish the required architectural foundation described in the target-state document:

- One canonical daemon and core.
- Thin shared interfaces.
- Canonical engineering state and durable memory.
- Repository-relative, content-addressed intelligence.
- Reproducible context compilation.
- Deterministic policy and human approval.
- Verification evidence and auditability.
- Explicit host and trust boundaries.
- Isolated, warranted execution.
- Independent adversarial review and human acceptance.
- Rollback-aware handoffs and project continuity.

M10 and M11 are optional enhancements. Target-state conformance does not depend on semantic indexing, embeddings, larger model usage, numerous vendor adapters, or multiple specialist reviewers. Those additions are justified only by evidence and must remain subordinate to the repository, deterministic verification, explicit policy, and human judgment.
