# Familiar Delivery Backlog

## Status

This document is a proposed delivery hierarchy derived from the approved philosophy, current-state architecture, target-state architecture, gap analysis, and architectural roadmap.

It separates unresolved human architectural decisions from delivery work:

1. Architecture Decision Records establish authority and constraints.
2. Milestones preserve the dependency order in `roadmap.md`.
3. Epics group bounded architectural capabilities.
4. Provisional implementation PRDs describe independently mergeable work.

All `PRD-TBD-*` identifiers are provisional. Canonical PRD numbers are assigned only after this hierarchy is approved.

## 1. Proposed Architecture Decision Records

These ADRs are required decisions, not implementation PRDs. None is decided by this backlog.

### ADR-001 — Canonical State and Event Semantics

- **Decision required:** Select the authoritative persistence and state-transition model for canonical workflow state and audit history.
- **Options that must be evaluated:** Conventional transactional tables plus audit events; append-only event sourcing with projections; a defined hybrid separating canonical state from append-oriented audit evidence.
- **Governing constraints:** One daemon-owned mutable authority; SQLite remains the default absent evidence for replacement; deterministic recovery; project isolation; referential integrity; auditable causality; no competing client state.
- **Downstream milestones blocked:** M2, M4, M8, and all later work that relies on canonical transitions or audit semantics.
- **Approval requirement:** Explicit human architectural approval after documenting consistency, recovery, operational complexity, and rollback consequences.

### ADR-002 — Human Identity and Approval Semantics

- **Decision required:** Define trusted local human identity and how approval is bound to exact scope, revision, action, time, and expiration.
- **Options that must be evaluated:** Operating-system user identity; locally managed signing identity; interface-specific authenticated identity mapped to a canonical human; a constrained hybrid.
- **Governing constraints:** Conversational assent is not approval; agents cannot approve themselves; approvals are attributable, scoped, revocable where applicable, and cannot silently broaden.
- **Downstream milestones blocked:** M2 client authorization, M4 approvals and policy, M8 warrants, and M9 acceptance.
- **Approval requirement:** Explicit human architectural and security approval.

### ADR-003 — Execution Warrant and External-Effect Authority

- **Decision required:** Define the warrant authority model and which merge, push, publication, release, deployment, credential, network, and other external effects Familiar may perform.
- **Options that must be evaluated:** Local attributable records; cryptographically signed warrants; scoped approval tokens; combinations with effect-specific mandatory interactive gates.
- **Governing constraints:** Bounded, revocable, expiring authority; explicit worktree and base revision; least privilege; no self-expansion; clear stop conditions; truthful rollback claims.
- **Downstream milestones blocked:** M4 warrant state, M8 isolated execution, and M9 acceptance of externally effective work.
- **Approval requirement:** Explicit human architectural and security approval for each authority class.

### ADR-004 — Host Isolation and Capability Policy

- **Decision required:** Define required macOS and Linux isolation guarantees, capability vocabulary, and fail-closed behavior when enforcement is unavailable.
- **Options that must be evaluated:** Native operating-system sandboxing; container isolation; restricted subprocess/worktree isolation; capability-tiered combinations with explicit refusal or human exception.
- **Governing constraints:** Never claim enforcement the host cannot provide; worktree isolation alone is not equivalent to process isolation; platform differences remain visible; safe disabled mode is required.
- **Downstream milestones blocked:** M3 host capability boundary, M7 verification execution, and M8 agent execution.
- **Approval requirement:** Explicit human architectural and security approval for minimum guarantees and permitted exceptions.

### ADR-005 — Verification, Audit, and Rollback Governance

- **Decision required:** Define verification-policy precedence, required evidence, non-waivable failures, audit retention/redaction, and when rollback may be claimed.
- **Options that must be evaluated:** Repository-owned verification policy; Familiar-local policy; an explicit precedence model combining both; append-oriented audit retention policies; Git-only versus broader effect-specific rollback evidence.
- **Governing constraints:** Passing tests alone is insufficient; absence and skip are not success; failures remain visible; project history and rollback capability are invariants; audit data must not leak secrets.
- **Downstream milestones blocked:** M4 audit and policy, M7 deterministic verification, M8 execution, and M9 acceptance.
- **Approval requirement:** Explicit human architectural approval, including non-waivable checks and retention policy.

### ADR-006 — Privacy, Context Equivalence, and Project Isolation Policy

- **Decision required:** Define remote disclosure, cross-provider context equivalence, cross-project knowledge authorization, and revocation.
- **Options that must be evaluated:** Local-only defaults with per-task approval; project/provider allowlists; classification-based disclosure; prohibited cross-project use versus explicitly authorized typed sharing.
- **Governing constraints:** Project isolation by default; secrets excluded; clients and providers are untrusted boundaries; vendor rendering cannot alter authority or constraints; source remains authoritative.
- **Downstream milestones blocked:** M3 credential boundaries, M6 context and agent contracts, M9 review routing, M10 semantic/model enhancements, and M11 vendor adapters.
- **Approval requirement:** Explicit human privacy and architectural approval.

## 2. Milestone-to-Epic Hierarchy

| Milestone | Epic | Architectural capability |
|---|---|---|
| M1 | EPIC-M1-01 | Canonical file identity and compatibility |
| M1 | EPIC-M1-02 | Repository lifecycle reconciliation |
| M2 | EPIC-M2-01 | Daemon-owned core and local IPC |
| M2 | EPIC-M2-02 | Thin client and presentation adapters |
| M3 | EPIC-M3-01 | Responsive control-plane execution boundaries |
| M3 | EPIC-M3-02 | Host and local trust capabilities |
| M4 | EPIC-M4-01 | Canonical task and evidence state |
| M4 | EPIC-M4-02 | Audit, approval, warrant, and policy state |
| M4 | EPIC-M4-03 | Durable handoff and decision governance |
| M5 | EPIC-M5-01 | Content-addressed repository inventory |
| M5 | EPIC-M5-02 | Provenance and invalidation |
| M6 | EPIC-M6-01 | Reproducible context compiler |
| M6 | EPIC-M6-02 | Neutral agent contracts |
| M7 | EPIC-M7-01 | Deterministic verification and evidence |
| M8 | EPIC-M8-01 | Isolated worktree and warrant enforcement |
| M8 | EPIC-M8-02 | Supervised execution, handoff, and effects |
| M9 | EPIC-M9-01 | Independent adversarial review |
| M9 | EPIC-M9-02 | Finding disposition and acceptance |
| M10 | EPIC-M10-01 | Optional semantic intelligence |
| M11 | EPIC-M11-01 | Expanded agent and reviewer ecosystem |

## 3. Epics

### EPIC-M1-01 — Canonical File Identity and Compatibility

- **Milestone:** M1 — Repository Identity and Reconciliation
- **Goal:** Establish one project-scoped repository-relative file identity while retaining safe access to existing summary records.
- **Gaps addressed:** GAP-003.
- **Dependencies:** ADR-001; existing project identity, path containment, summary repositories.
- **Constituent PRDs:** PRD-TBD-M1-01, PRD-TBD-M1-02.
- **Completion boundary:** All new file-summary reads and writes use one canonical identity; legacy data is reconciled reversibly; traversal and project isolation remain intact.

### EPIC-M1-02 — Repository Lifecycle Reconciliation

- **Milestone:** M1
- **Goal:** Make persistent intelligence converge with repository removals, renames, and complete scans.
- **Gaps addressed:** GAP-005.
- **Dependencies:** EPIC-M1-01.
- **Constituent PRDs:** PRD-TBD-M1-03.
- **Completion boundary:** Removed and renamed records reconcile correctly, scans are resumable or explicitly incomplete, and bounded resource behavior remains operational.

### EPIC-M2-01 — Daemon-Owned Core and Local IPC

- **Milestone:** M2 — Central Core Authority and Shared Interfaces
- **Goal:** Establish shared command/query contracts and daemon-owned local IPC as the canonical access boundary.
- **Gaps addressed:** GAP-001, GAP-010.
- **Dependencies:** ADR-001, ADR-002.
- **Constituent PRDs:** PRD-TBD-M2-01.
- **Completion boundary:** Core operations have protocol-neutral semantics and can be invoked through authenticated local IPC without removing existing clients.

### EPIC-M2-02 — Thin Client and Presentation Adapters

- **Milestone:** M2
- **Goal:** Move MCP, dashboard, tray, and CLI behavior behind shared core services and authoritative status.
- **Gaps addressed:** GAP-010, GAP-016, GAP-020.
- **Dependencies:** EPIC-M2-01.
- **Constituent PRDs:** PRD-TBD-M2-02, PRD-TBD-M2-03, PRD-TBD-M2-04.
- **Completion boundary:** Clients retain useful behavior but have no competing mutable state, policy, status, or inference lifecycle.

### EPIC-M3-01 — Responsive Control-Plane Execution Boundaries

- **Milestone:** M3 — Responsive Control Plane and Host Trust Baseline
- **Goal:** Isolate blocking database, filesystem, Git, and analysis work from asynchronous control-plane responsiveness.
- **Gaps addressed:** GAP-015.
- **Dependencies:** M2 complete.
- **Constituent PRDs:** PRD-TBD-M3-01.
- **Completion boundary:** IPC, health, watcher intake, cancellation, and shutdown remain responsive under bounded storage and indexing load.

### EPIC-M3-02 — Host and Local Trust Capabilities

- **Milestone:** M3
- **Goal:** Represent enforceable macOS/Linux capabilities and secure local clients, loopback interfaces, and provider credentials.
- **Gaps addressed:** GAP-014 and the foundational portion of GAP-017.
- **Dependencies:** ADR-004, ADR-006, EPIC-M2-01.
- **Constituent PRDs:** PRD-TBD-M3-02, PRD-TBD-M3-03.
- **Completion boundary:** Capability absence is explicit, clients cannot bypass core authorization, secrets remain outside ordinary configuration and generated context, and unsupported enforcement fails according to policy.

### EPIC-M4-01 — Canonical Task and Evidence State

- **Milestone:** M4 — Canonical Stewardship State, Policy, Audit, and Memory
- **Goal:** Add explicit tasks, findings, and verification-result state without treating conversation as workflow state.
- **Gaps addressed:** GAP-002.
- **Dependencies:** ADR-001, M2 complete.
- **Constituent PRDs:** PRD-TBD-M4-01, PRD-TBD-M4-02.
- **Completion boundary:** Tasks and evidence have project-scoped identities, valid transitions, provenance, and referential integrity.

### EPIC-M4-02 — Audit, Approval, Warrant, and Policy State

- **Milestone:** M4
- **Goal:** Add attributable audit history and deterministic authority evaluation without enabling execution.
- **Gaps addressed:** GAP-011, GAP-012.
- **Dependencies:** ADR-001 through ADR-005 as applicable; EPIC-M4-01.
- **Constituent PRDs:** PRD-TBD-M4-03, PRD-TBD-M4-04.
- **Completion boundary:** Material transitions are auditable; approvals and warrants are scoped and attributable; deterministic policy can grant or deny state transitions but cannot execute agents.

### EPIC-M4-03 — Durable Handoff and Decision Governance

- **Milestone:** M4
- **Goal:** Preserve structured handoffs and approval-aware, supersedable engineering decisions.
- **Gaps addressed:** GAP-018 and remaining durable-memory aspects of GAP-002.
- **Dependencies:** EPIC-M4-01, EPIC-M4-02.
- **Constituent PRDs:** PRD-TBD-M4-05, PRD-TBD-M4-06.
- **Completion boundary:** Session boundaries and decisions retain actors, revisions, evidence, status, relationships, and history without silently elevating agent assertions.

### EPIC-M5-01 — Content-Addressed Repository Inventory

- **Milestone:** M5 — Content-Addressed Repository Intelligence
- **Goal:** Identify file content, Git revision, branch, and worktree deterministically.
- **Gaps addressed:** GAP-004.
- **Dependencies:** M1 complete; canonical project/task revision semantics from M4 where applicable.
- **Constituent PRDs:** PRD-TBD-M5-01, PRD-TBD-M5-03.
- **Completion boundary:** Repository intelligence is tied to content and revision identity rather than mtime or host path alone.

### EPIC-M5-02 — Provenance and Invalidation

- **Milestone:** M5
- **Goal:** Make every derived artifact attributable, reconstructible, invalidatable, and subordinate to source.
- **Gaps addressed:** GAP-004, GAP-019.
- **Dependencies:** EPIC-M5-01.
- **Constituent PRDs:** PRD-TBD-M5-02.
- **Completion boundary:** Valid artifacts are reused, invalid artifacts cannot appear fresh, and uncertain intelligence forces source inspection.

### EPIC-M6-01 — Reproducible Context Compiler

- **Milestone:** M6 — Reproducible Context Compilation and Agent Contracts
- **Goal:** Produce immutable task-specific context with provenance, explicit budgets, omissions, and source fallback.
- **Gaps addressed:** GAP-006.
- **Dependencies:** M4 and M5 complete; ADR-006.
- **Constituent PRDs:** PRD-TBD-M6-01, PRD-TBD-M6-02.
- **Completion boundary:** Equivalent inputs produce attributable context manifests within enforced budgets, and derived intelligence never replaces required source.

### EPIC-M6-02 — Neutral Agent Contracts

- **Milestone:** M6
- **Goal:** Define vendor-neutral task, context, result, finding, and handoff contracts.
- **Gaps addressed:** GAP-013.
- **Dependencies:** EPIC-M6-01, ADR-006.
- **Constituent PRDs:** PRD-TBD-M6-03.
- **Completion boundary:** No canonical contract depends on Claude Code, Codex, Cursor, OpenCode, or any provider-specific session identity.

### EPIC-M7-01 — Deterministic Verification and Evidence

- **Milestone:** M7 — Deterministic Verification and Evidence
- **Goal:** Resolve approved checks, execute them under host constraints, and preserve exact test, log, diff, and invariant evidence.
- **Gaps addressed:** GAP-007 and verification-tool aspects of GAP-017.
- **Dependencies:** ADR-004, ADR-005, M3 and M4 complete.
- **Constituent PRDs:** PRD-TBD-M7-01, PRD-TBD-M7-02.
- **Completion boundary:** Familiar distinguishes pass, fail, skip, absent, and inconclusive; exact commands and evidence are reproducible; verification grants no implementation authority.

### EPIC-M8-01 — Isolated Worktree and Warrant Enforcement

- **Milestone:** M8 — Bounded Isolated Execution
- **Goal:** Bind execution to safe worktrees and validated warrants.
- **Gaps addressed:** GAP-008 and execution portions of GAP-014/GAP-017.
- **Dependencies:** ADR-003, ADR-004, M3, M4, M6, and M7 complete.
- **Constituent PRDs:** PRD-TBD-M8-01, PRD-TBD-M8-02.
- **Completion boundary:** No agent process begins without an identified base revision, isolated environment, valid authority, and enforceable stop conditions.

### EPIC-M8-02 — Supervised Execution, Handoff, and Effects

- **Milestone:** M8
- **Goal:** Supervise bounded agent processes, preserve terminal handoffs, and govern external effects and rollback truth.
- **Gaps addressed:** Remaining GAP-008 and execution trust boundaries.
- **Dependencies:** EPIC-M8-01, ADR-005.
- **Constituent PRDs:** PRD-TBD-M8-03, PRD-TBD-M8-04.
- **Completion boundary:** Every attempt terminates with attributable state and evidence; unauthorized effects fail closed; interrupted work remains recoverable.

### EPIC-M9-01 — Independent Adversarial Review

- **Milestone:** M9 — Independent Adversarial Review and Acceptance
- **Goal:** Assign a reviewer distinct from the implementer and compile falsification-oriented review context.
- **Gaps addressed:** GAP-009.
- **Dependencies:** M6, M7, and M8 complete.
- **Constituent PRDs:** PRD-TBD-M9-01.
- **Completion boundary:** Implementer and reviewer are independently attributable, and review consumes reproducible evidence rather than implementer conclusions as truth.

### EPIC-M9-02 — Finding Disposition and Acceptance

- **Milestone:** M9
- **Goal:** Preserve review findings and combine deterministic evidence, disposition, human approval, and concise handoff reporting.
- **Gaps addressed:** GAP-009 and completion use of GAP-002/GAP-011/GAP-012/GAP-018 capabilities.
- **Dependencies:** EPIC-M9-01.
- **Constituent PRDs:** PRD-TBD-M9-02, PRD-TBD-M9-03.
- **Completion boundary:** No finding disappears silently, acceptance remains policy- and human-gated, and every outcome produces a reconstructible report.

### EPIC-M10-01 — Optional Semantic Intelligence

- **Milestone:** M10 — Optional Semantic Intelligence Acceleration
- **Goal:** Add optional project-isolated semantic retrieval and model-assisted summaries only where evidence justifies them.
- **Gaps addressed:** Enhances resolved GAP-004 and GAP-006; no new mandatory gap.
- **Dependencies:** M5 and M6 complete; ADR-006.
- **Constituent PRDs:** PRD-TBD-M10-01, PRD-TBD-M10-02.
- **Completion boundary:** All semantic artifacts are disposable, provenance-bearing accelerators; deterministic and inference-disabled operation remains complete.

### EPIC-M11-01 — Expanded Agent and Reviewer Ecosystem

- **Milestone:** M11 — Expanded Agent and Client Ecosystem
- **Goal:** Add vendor adapters and specialist review routing without adding new authority paths.
- **Gaps addressed:** Enhances resolved GAP-009, GAP-010, and GAP-013.
- **Dependencies:** M6 and M9 complete; ADR-006.
- **Constituent PRDs:** PRD-TBD-M11-01, PRD-TBD-M11-02.
- **Completion boundary:** Agents and reviewers are selected through neutral capabilities; any adapter can be removed without affecting canonical state or workflow correctness.

## 4. Dependency-Ordered Provisional Implementation PRDs

### M1 — Repository Identity and Reconciliation

#### PRD-TBD-M1-01 — Canonical File Identity Boundary

- **Epic:** EPIC-M1-01
- **Dependencies:** ADR-001.
- **Scope:** Canonical repository-relative identity across watcher, summary, storage, lazy lookup, and module queries.
- **Success criteria:** All new reads/writes converge on one identity; containment and project isolation pass.
- **Rollback:** Retain legacy reads and revert new-write selection.
- **Risks:** Collisions, symlink escape, platform path divergence.
- **Tests and deterministic verification:** Unit/integration path fixtures; duplicate-identity and containment checks.
- **Migration:** Schema support may be additive; no legacy data rewrite in this PRD.

#### PRD-TBD-M1-02 — Legacy Summary Identity Reconciliation

- **Epic:** EPIC-M1-01
- **Dependencies:** PRD-TBD-M1-01.
- **Scope:** Reversible reconciliation of absolute-path and duplicate summary records.
- **Success criteria:** Existing records remain readable; conflicts are reported; no silent loss.
- **Rollback:** Preserve original values or reversible mappings until verification completes.
- **Risks:** Incorrect project association and destructive deduplication.
- **Tests and deterministic verification:** Migration fixtures, before/after counts, foreign keys, replay.
- **Migration:** Required reversible data migration.

#### PRD-TBD-M1-03 — Repository Lifecycle and Scan Reconciliation

- **Epic:** EPIC-M1-02
- **Dependencies:** PRD-TBD-M1-01.
- **Scope:** Delete/rename persistence plus observable, resumable scan completion.
- **Success criteria:** Inventory converges with filesystem/Git state; interrupted scans resume; partial state is visible.
- **Rollback:** Restore tombstoned records or replay a deterministic inventory scan.
- **Risks:** Event races, false deletion, duplicate work.
- **Tests and deterministic verification:** Rename/delete/atomic-save/saturation/restart integration fixtures; inventory comparison.
- **Migration:** Lifecycle or scan-state records may require additive schema.

### M2 — Central Core Authority and Shared Interfaces

#### PRD-TBD-M2-01 — Core Contracts and Authenticated Local IPC

- **Epic:** EPIC-M2-01
- **Dependencies:** ADR-001, ADR-002.
- **Scope:** Protocol-neutral commands/queries, errors, authorization context, and daemon local-socket transport.
- **Success criteria:** Existing operations are representable and locally callable with bounded framing and per-user access.
- **Rollback:** Keep existing in-process/direct paths active until parity is proven.
- **Risks:** Protocol concepts leaking into core, unauthorized local access, stale sockets.
- **Tests and deterministic verification:** Contract goldens; malformed-message, permission, restart, and parity tests.
- **Migration:** None.

#### PRD-TBD-M2-02 — MCP Core-Service Cutover

- **Epic:** EPIC-M2-02
- **Dependencies:** PRD-TBD-M2-01.
- **Scope:** Read and mutation MCP tools over core services; removal of direct database and inference-lifecycle authority.
- **Success criteria:** Tool compatibility holds; all writes originate in the daemon; status is authoritative.
- **Rollback:** Adapter-level fallback during cutover without changing stored data.
- **Risks:** Duplicate/lost mutation, daemon availability, response drift.
- **Tests and deterministic verification:** Golden MCP parity; idempotency, reconnect, isolation, and write-origin tracing.
- **Migration:** None.

#### PRD-TBD-M2-03 — Authoritative Status, Dashboard, and Tray Adapters

- **Epic:** EPIC-M2-02
- **Dependencies:** PRD-TBD-M2-01.
- **Scope:** Observed subsystem health and presentation adapters over core queries/commands.
- **Success criteria:** Configured, running, paused, degraded, connected, and failed are distinct; no hard-coded health claims remain.
- **Rollback:** Legacy fields remain derived compatibility views.
- **Risks:** UI regressions and status flapping.
- **Tests and deterministic verification:** Lifecycle failure fixtures, endpoint/menu parity, controlled status snapshots.
- **Migration:** None.

#### PRD-TBD-M2-04 — Core CLI Adapter

- **Epic:** EPIC-M2-02
- **Dependencies:** PRD-TBD-M2-01.
- **Scope:** Scriptable query/command access with the same authorization semantics.
- **Success criteria:** CLI cannot bypass policy and has deterministic output and exit codes.
- **Rollback:** Disable the adapter without affecting core services.
- **Risks:** Alternate-interface policy bypass.
- **Tests and deterministic verification:** Argument/error/auth tests; cross-interface golden comparisons.
- **Migration:** None.

### M3 — Responsive Control Plane and Host Trust Baseline

#### PRD-TBD-M3-01 — Blocking Work Execution Boundary

- **Epic:** EPIC-M3-01
- **Dependencies:** M2 complete.
- **Scope:** Explicit bounded execution for SQLite, filesystem, Git, and analysis work.
- **Success criteria:** Control-plane IPC, watcher intake, health, cancellation, and shutdown remain responsive under load.
- **Rollback:** Restore serial paths while retaining bounded queues.
- **Risks:** Transaction reordering, unbounded workers, cancellation leaks.
- **Tests and deterministic verification:** Concurrency, saturation, shutdown, busy-database, and latency-bound tests.
- **Migration:** None.

#### PRD-TBD-M3-02 — macOS and Linux Host Capability Model

- **Epic:** EPIC-M3-02
- **Dependencies:** ADR-004, M2 complete.
- **Scope:** Observable socket, process, Git/worktree, sandbox, container, credential, and resource capabilities.
- **Success criteria:** Enforcement level is queryable and never inferred from platform identity.
- **Rollback:** Keep reporting informational and disable dependent enforcement.
- **Risks:** False-positive capability claims and platform divergence.
- **Tests and deterministic verification:** Platform fixtures; probes compared with attempted operations.
- **Migration:** None.

#### PRD-TBD-M3-03 — Local Client, Loopback, and Credential Trust Boundary

- **Epic:** EPIC-M3-02
- **Dependencies:** ADR-002, ADR-006, PRD-TBD-M3-02.
- **Scope:** Least-privilege clients, fail-closed loopback exposure, secret references, redaction, and host credential access.
- **Success criteria:** Interfaces cannot bypass authorization; secrets are absent from ordinary config, logs, and context.
- **Rollback:** Disable affected integrations and retain explicitly marked legacy credential support temporarily.
- **Risks:** Credential loss, lockout, accidental network disclosure.
- **Tests and deterministic verification:** Auth/bind/store/redaction tests; listener inspection and secret scanning.
- **Migration:** Optional non-destructive credential import.

### M4 — Canonical Stewardship State, Policy, Audit, and Memory

#### PRD-TBD-M4-01 — Canonical Task State

- **Epic:** EPIC-M4-01
- **Dependencies:** ADR-001, M2 complete.
- **Scope:** Project-scoped objective, scope, criteria, base revision, lifecycle, and stopping point.
- **Success criteria:** Task transitions are explicit and conversation-independent.
- **Rollback:** Leave additive state unused; existing memory tools continue.
- **Risks:** Mutable criteria and invalid transitions.
- **Tests and deterministic verification:** Repository/isolation tests and state-machine matrix.
- **Migration:** Additive task schema.

#### PRD-TBD-M4-02 — Canonical Finding and Verification Evidence State

- **Epic:** EPIC-M4-01
- **Dependencies:** PRD-TBD-M4-01, ADR-005.
- **Scope:** Findings plus test/verification results and artifact references as one evidence-domain boundary.
- **Success criteria:** Findings remain distinct from tasks/decisions; pass/fail/skip/absent/inconclusive remain distinct.
- **Rollback:** Disable new workflows while preserving additive records.
- **Risks:** Unsupported claims, evidence-free success, severity drift.
- **Tests and deterministic verification:** Lifecycle, status, evidence-reference, and schema-integrity matrices.
- **Migration:** Additive finding and verification schemas in one reviewed migration boundary.

#### PRD-TBD-M4-03 — Append-Oriented Audit Events

- **Epic:** EPIC-M4-02
- **Dependencies:** ADR-001, ADR-005, PRD-TBD-M4-01.
- **Scope:** Attributable, causal, redacted audit events for material core transitions.
- **Success criteria:** Actor, interface, project/task, operation, result, and causal identity are reconstructible.
- **Rollback:** Disable emission only before dependent authority is enabled; retain committed events.
- **Risks:** Sensitive leakage and incomplete coverage.
- **Tests and deterministic verification:** Ordering, attribution, redaction, retention, event-chain coverage.
- **Migration:** Additive audit schema.

#### PRD-TBD-M4-04 — Approval, Warrant, and Deterministic Policy State

- **Epic:** EPIC-M4-02
- **Dependencies:** ADR-002, ADR-003, ADR-005, PRD-TBD-M4-03.
- **Scope:** Scoped approval/warrant records and deterministic policy evaluation without execution.
- **Success criteria:** Expired, revoked, replayed, broadened, or mismatched authority fails closed; identical inputs yield explainable decisions.
- **Rollback:** Revoke/disable all warrants and return policy to report-only mode.
- **Risks:** Replay, hidden defaults, policy bypass.
- **Tests and deterministic verification:** Golden authorization/policy corpus; expiration, revocation, and cross-interface tests.
- **Migration:** Additive approval, warrant, and policy metadata.

#### PRD-TBD-M4-05 — Canonical Handoff State

- **Epic:** EPIC-M4-03
- **Dependencies:** PRD-TBD-M4-01, PRD-TBD-M4-02.
- **Scope:** Structured task/revision/change/evidence/risk/next-action handoffs.
- **Success criteria:** Session completion does not depend on transient conversation.
- **Rollback:** Existing rollups remain available as compatibility views.
- **Risks:** Stale next action and unstructured summary drift.
- **Tests and deterministic verification:** Relationship/completeness fixtures and referential-integrity checks.
- **Migration:** Additive handoff schema.

#### PRD-TBD-M4-06 — Decision Provenance and Supersession

- **Epic:** EPIC-M4-03
- **Dependencies:** ADR-002, PRD-TBD-M4-03.
- **Scope:** Decision author, approver, status, rationale, evidence, revision, relationships, and supersession.
- **Success criteria:** Agent proposals cannot silently become approved architecture; legacy history remains visible.
- **Rollback:** Preserve compatibility reads and original fields.
- **Risks:** Incorrect legacy classification and lost rationale.
- **Tests and deterministic verification:** Legacy, approval, supersession, and history-reconciliation tests.
- **Migration:** Additive decision migration with conservative legacy status.

### M5 — Content-Addressed Repository Intelligence

#### PRD-TBD-M5-01 — Content-Hashed File Inventory

- **Epic:** EPIC-M5-01
- **Dependencies:** M1 complete.
- **Scope:** Deterministic content and Git identity for canonical files.
- **Success criteria:** Unchanged content is recognized independently of mtime; lifecycle reconciliation remains correct.
- **Rollback:** Fall back to existing freshness metadata while retaining hashes.
- **Risks:** Hashing cost and inconsistent normalization.
- **Tests and deterministic verification:** Content/mtime/rename/binary fixtures; independent hash comparison.
- **Migration:** Additive inventory/hash schema.

#### PRD-TBD-M5-02 — Derived Artifact Provenance and Invalidation

- **Epic:** EPIC-M5-02
- **Dependencies:** PRD-TBD-M5-01.
- **Scope:** Source/configuration/generator provenance, artifact identity, dependency edges, and invalidation.
- **Success criteria:** Reusable artifacts are demonstrably valid; changed inputs cannot appear fresh; legacy summaries are marked unprovenanced.
- **Rollback:** Conservatively invalidate and regenerate deterministic artifacts.
- **Risks:** Under-invalidation or wasteful over-invalidation.
- **Tests and deterministic verification:** Provenance recomputation and dependency hit/miss fixtures.
- **Migration:** Additive provenance/dependency schema.

#### PRD-TBD-M5-03 — Branch and Worktree Intelligence Identity

- **Epic:** EPIC-M5-01
- **Dependencies:** PRD-TBD-M5-01, PRD-TBD-M4-01.
- **Scope:** Revision, branch, detached-head, and worktree identity without duplicating immutable content.
- **Success criteria:** Intelligence cannot leak across incompatible repository states.
- **Rollback:** Restrict intelligence to primary repository state.
- **Risks:** Cross-branch leakage and worktree ambiguity.
- **Tests and deterministic verification:** Divergent-branch/worktree fixtures compared with Git plumbing.
- **Migration:** Additive revision/worktree metadata.

### M6 — Reproducible Context Compilation and Agent Contracts

#### PRD-TBD-M6-01 — Immutable Context Manifest

- **Epic:** EPIC-M6-01
- **Dependencies:** M4 and M5 complete; ADR-006.
- **Scope:** Task, revision, constraints, evidence, provenance, budgets, omissions, and artifact identity.
- **Success criteria:** Equivalent inputs produce the same manifest identity.
- **Rollback:** Existing task packer remains available.
- **Risks:** Hidden inputs and unstable ordering.
- **Tests and deterministic verification:** Round-trip/determinism/isolation tests; repeated hash equality.
- **Migration:** Optional additive context-artifact storage.

#### PRD-TBD-M6-02 — Budgeted Selection and Authoritative Source Fallback

- **Epic:** EPIC-M6-01
- **Dependencies:** PRD-TBD-M6-01, PRD-TBD-M5-02.
- **Scope:** Section/total budgets, ranking, truncation, omission, and mandatory source fallback.
- **Success criteria:** Output respects ceilings and never presents invalid derived intelligence as source truth.
- **Rollback:** Source-only or legacy packer modes remain available.
- **Risks:** Relevant omission and excessive source loading.
- **Tests and deterministic verification:** Exact budget goldens; stale/missing/conflicting fallback fixtures.
- **Migration:** None.

#### PRD-TBD-M6-03 — Neutral Agent Task and Result Contracts

- **Epic:** EPIC-M6-02
- **Dependencies:** PRD-TBD-M6-01, ADR-006.
- **Scope:** Vendor-neutral task, context, result, finding, and handoff contracts.
- **Success criteria:** Canonical schemas contain no vendor-specific authority or session identity.
- **Rollback:** Keep contracts internal and retain current inference interfaces.
- **Risks:** Lowest-common-denominator contracts and vendor leakage.
- **Tests and deterministic verification:** Conformance fixtures and cross-rendering schema equivalence.
- **Migration:** None.

### M7 — Deterministic Verification and Evidence

#### PRD-TBD-M7-01 — Verification Policy Resolution and Supervised Runner

- **Epic:** EPIC-M7-01
- **Dependencies:** ADR-004, ADR-005, M3 and M4 complete.
- **Scope:** Resolve declared/discovered checks and execute approved commands with bounded process control and structured evidence.
- **Success criteria:** Exact command/environment/revision/result/log data is retained; missing checks never become success.
- **Rollback:** Report-only discovery with execution disabled.
- **Risks:** Untrusted commands, orphan processes, output exhaustion.
- **Tests and deterministic verification:** Discovery precedence plus success/failure/timeout/cancel/output fixture commands.
- **Migration:** Optional verification-policy metadata; evidence schema already established.

#### PRD-TBD-M7-02 — Diff, Log, Repository, and Invariant Verification

- **Epic:** EPIC-M7-01
- **Dependencies:** PRD-TBD-M7-01, PRD-TBD-M5-03, PRD-TBD-M4-06.
- **Scope:** Changed-path scope, exact diff, cleanliness, logs, generated artifacts, and declared invariants.
- **Success criteria:** Violations and suspicious results remain visible and attributable.
- **Rollback:** Run checks in non-gating report mode.
- **Risks:** False positives and unsafe log interpretation.
- **Tests and deterministic verification:** Git plumbing comparisons and golden log/invariant fixtures.
- **Migration:** Optional invariant-policy metadata.

### M8 — Bounded Isolated Execution

#### PRD-TBD-M8-01 — Safe Worktree Lifecycle

- **Epic:** EPIC-M8-01
- **Dependencies:** ADR-004, PRD-TBD-M3-02, PRD-TBD-M5-03.
- **Scope:** Create, identify, inspect, preserve, and retire task-specific worktrees.
- **Success criteria:** Each worktree binds to a task/base revision and uncertain targets are never deleted.
- **Rollback:** Preserve worktree for manual recovery.
- **Risks:** User-data deletion and revision confusion.
- **Tests and deterministic verification:** Collision/dirty/interruption/retirement fixtures; Git worktree comparison.
- **Migration:** Additive worktree lifecycle state.

#### PRD-TBD-M8-02 — Warrant Validation and Execution Admission

- **Epic:** EPIC-M8-01
- **Dependencies:** ADR-003, PRD-TBD-M4-04, PRD-TBD-M8-01.
- **Scope:** Validate task, worktree, paths, commands, network, resources, time, approval, expiration, and revocation before admission.
- **Success criteria:** Invalid or broadened warrants fail closed before a process starts.
- **Rollback:** Revoke all warrants and disable admission.
- **Risks:** Path alias bypass, replay, clock dependence.
- **Tests and deterministic verification:** Golden warrant corpus plus escape/replay/expiration tests.
- **Migration:** None beyond M4 warrant state.

#### PRD-TBD-M8-03 — Bounded Agent Supervisor and Terminal Handoff

- **Epic:** EPIC-M8-02
- **Dependencies:** PRD-TBD-M6-03, PRD-TBD-M7-01, PRD-TBD-M8-02, PRD-TBD-M4-05.
- **Scope:** Supervise one agent process with limits, checkpoints, cancellation, stop conditions, and mandatory terminal handoff.
- **Success criteria:** Every attempt terminates with attributable status, evidence, and recoverable worktree state.
- **Rollback:** Terminate supervision, preserve worktree/logs, and issue blocked handoff.
- **Risks:** Sandbox escape, leaked children, missing terminal state.
- **Tests and deterministic verification:** Adversarial fixture agent; timeout/revoke/crash/restart/completeness tests.
- **Migration:** Additive execution-attempt/checkpoint state.

#### PRD-TBD-M8-04 — External Effects and Rollback Evidence

- **Epic:** EPIC-M8-02
- **Dependencies:** ADR-003, ADR-005, PRD-TBD-M8-03.
- **Scope:** Effect-specific authorization, observation, audit, and rollback evidence.
- **Success criteria:** No external effect occurs without exact authority; irreversible effects fail before execution unless explicitly permitted.
- **Rollback:** Apply approved effect-specific reversal; otherwise no action is admitted.
- **Risks:** Partial external failure and misleading reversibility.
- **Tests and deterministic verification:** Denied/expired/partial/reversible scenarios compared with observed state.
- **Migration:** Additive effect and rollback-evidence records.

### M9 — Independent Adversarial Review and Acceptance

#### PRD-TBD-M9-01 — Independent Reviewer Assignment and Review Context

- **Epic:** EPIC-M9-01
- **Dependencies:** M6, M7, and M8 complete.
- **Scope:** Distinct reviewer assignment plus reproducible falsification-oriented context.
- **Success criteria:** Implementer/reviewer roles are separate and review does not treat implementer claims as truth.
- **Rollback:** Require human-only review using the same evidence manifest.
- **Risks:** Superficial role separation and reviewer anchoring.
- **Tests and deterministic verification:** Same-agent rejection, fallback, context determinism, and role-isolation fixtures.
- **Migration:** Additive review-assignment state.

#### PRD-TBD-M9-02 — Review Finding Ingestion and Disposition

- **Epic:** EPIC-M9-02
- **Dependencies:** PRD-TBD-M9-01, PRD-TBD-M4-02, PRD-TBD-M4-03.
- **Scope:** Evidence-backed findings, disagreement preservation, and explicit resolution/rejection/deferral/risk acceptance.
- **Success criteria:** Findings cannot disappear through deduplication or implementer self-dismissal.
- **Rollback:** Freeze automated disposition and require human handling.
- **Risks:** Noise, unsupported claims, lost disagreement.
- **Tests and deterministic verification:** Conflict/duplicate/disposition fixtures and audit-completeness checks.
- **Migration:** None beyond finding state.

#### PRD-TBD-M9-03 — Acceptance Gate and Engineering Report

- **Epic:** EPIC-M9-02
- **Dependencies:** PRD-TBD-M9-02, PRD-TBD-M7-02, PRD-TBD-M8-04.
- **Scope:** Deterministic acceptance decision, human gate, and concise report derived from canonical records.
- **Success criteria:** Failed required checks and unresolved findings block unless explicitly waivable and approved; reports preserve uncertainty.
- **Rollback:** Disable automated acceptance and expose structured records for manual review.
- **Risks:** Consensus treated as proof and summary implying unearned success.
- **Tests and deterministic verification:** Golden accept/reject/revise/waive/rollback decisions and report completeness.
- **Migration:** Additive acceptance state if not represented by task lifecycle.

### M10 — Optional Semantic Intelligence Acceleration

#### PRD-TBD-M10-01 — Optional Project-Isolated Semantic Index

- **Epic:** EPIC-M10-01
- **Dependencies:** M5 and M6 complete; ADR-006; measured need.
- **Scope:** Reconstructible semantic retrieval with project isolation and provenance.
- **Success criteria:** Index can be disabled or rebuilt without canonical knowledge loss.
- **Rollback:** Drop/disable index and retain deterministic lexical retrieval.
- **Risks:** Leakage, stale vectors, unjustified infrastructure.
- **Tests and deterministic verification:** Isolation/invalidation/rebuild/disabled-mode fixtures.
- **Migration:** Disposable derived-index schema.

#### PRD-TBD-M10-02 — Optional Model-Assisted Derived Summaries

- **Epic:** EPIC-M10-01
- **Dependencies:** M5 and M6 complete; ADR-006; measured need.
- **Scope:** Provenance-bearing model summaries with deterministic baseline and source fallback.
- **Success criteria:** Model/configuration/input identity is recorded and inference-disabled mode remains complete.
- **Rollback:** Disable and regenerate deterministic artifacts.
- **Risks:** Hallucinated authority, privacy disclosure, nondeterminism.
- **Tests and deterministic verification:** Privacy/provenance/invalidation/fallback fixtures.
- **Migration:** Optional derived-artifact metadata only.

### M11 — Expanded Agent and Client Ecosystem

#### PRD-TBD-M11-01 — Agent Adapter Conformance and Capability Selection

- **Epic:** EPIC-M11-01
- **Dependencies:** M6 and M9 complete; ADR-006; evidence of adapter demand.
- **Scope:** Conformance boundary, capability metadata, explainable selection, and independently disableable initial vendor adapters.
- **Success criteria:** Adapters add no canonical semantics or authority; loss of any provider does not affect workflow correctness.
- **Rollback:** Disable individual adapters and require explicit human agent selection.
- **Risks:** Vendor leakage, opaque scoring, permission bypass.
- **Tests and deterministic verification:** Adapter goldens, cross-provider equivalence, denial, capability mismatch, and deterministic selection fixtures.
- **Migration:** Additive capability/health metadata only.

#### PRD-TBD-M11-02 — Optional Specialist Multi-Reviewer Coordination

- **Epic:** EPIC-M11-01
- **Dependencies:** PRD-TBD-M11-01, M9 complete; evidence of review benefit.
- **Scope:** Multiple specialist reviewers with explicit attribution and preserved disagreement.
- **Success criteria:** Consensus never substitutes for deterministic evidence or human approval.
- **Rollback:** Return to one independent reviewer plus deterministic verification.
- **Risks:** Cost growth, duplicated noise, majority-vote acceptance.
- **Tests and deterministic verification:** Conflict/duplicate/unavailable/specialty fixtures and disagreement-preservation audits.
- **Migration:** Optional reviewer-specialty metadata.

## 5. Original Backlog Disposition Map

| Original item | Disposition | New destination | Justification |
|---|---|---|---|
| PRD-012 | Reclassified as ADR | ADR-001 | Canonical state/event semantics is a human architecture decision. |
| PRD-013 | Reclassified as ADR | ADR-002 | Human identity and approval semantics must be decided before implementation. |
| PRD-014 | Reclassified as ADR | ADR-003 | Warrant and external-effect authority defines policy, not code scope. |
| PRD-015 | Reclassified as ADR | ADR-004 | Minimum host isolation requires explicit architectural risk acceptance. |
| PRD-016 | Reclassified as ADR | ADR-005 | Verification/audit/rollback governance is normative architecture. |
| PRD-017 | Reclassified as ADR | ADR-006 | Privacy, equivalence, and isolation policy requires human decision. |
| PRD-018 | Retained PRD | PRD-TBD-M1-01 | Independent canonical identity boundary. |
| PRD-019 | Retained PRD | PRD-TBD-M1-02 | Separate destructive-data and rollback boundary. |
| PRD-020 | Merged PRD | PRD-TBD-M1-03 | Shares reconciliation state, tests, and operating boundary with scan completeness. |
| PRD-021 | Merged PRD | PRD-TBD-M1-03 | Scan state is inseparable from truthful repository reconciliation. |
| PRD-022 | Merged PRD | PRD-TBD-M2-01 | Contracts without a usable IPC boundary create scaffolding-only architecture. |
| PRD-023 | Merged PRD | PRD-TBD-M2-01 | Local IPC is the first useful realization of shared contracts. |
| PRD-024 | Merged PRD | PRD-TBD-M2-02 | Read and mutation MCP cutover share the adapter boundary and parity suite. |
| PRD-025 | Merged PRD | PRD-TBD-M2-02 | Authority removal is only useful with complete MCP core-service cutover. |
| PRD-026 | Merged PRD | PRD-TBD-M2-03 | Status semantics and their dashboard/tray consumers share one boundary. |
| PRD-027 | Merged PRD | PRD-TBD-M2-03 | Presentation cutover is required to make authoritative status useful. |
| PRD-028 | Retained PRD | PRD-TBD-M2-04 | CLI is independently useful and independently removable. |
| PRD-029 | Merged PRD | PRD-TBD-M3-01 | Database and filesystem blocking boundaries share control-plane responsiveness tests. |
| PRD-030 | Merged PRD | PRD-TBD-M3-01 | Separate delivery would leave only a partially responsive control plane. |
| PRD-031 | Retained PRD | PRD-TBD-M3-02 | Capability reporting is useful without credential or client changes. |
| PRD-032 | Merged PRD | PRD-TBD-M3-03 | Credential and local-client trust share disclosure, auth, and rollback boundaries. |
| PRD-033 | Merged PRD | PRD-TBD-M3-03 | Same local trust boundary as credential and loopback enforcement. |
| PRD-034 | Retained PRD | PRD-TBD-M4-01 | Task state is a distinct schema and prerequisite. |
| PRD-035 | Merged PRD | PRD-TBD-M4-02 | Findings and verification records form one evidence-domain migration boundary. |
| PRD-036 | Merged PRD | PRD-TBD-M4-02 | Same evidence references, status rigor, and isolation tests. |
| PRD-037 | Retained PRD | PRD-TBD-M4-05 | Handoffs have a distinct schema and compatibility boundary. |
| PRD-038 | Retained PRD | PRD-TBD-M4-03 | Audit semantics carry distinct retention and rollback risk. |
| PRD-039 | Merged PRD | PRD-TBD-M4-04 | Warrant records without deterministic policy are inert scaffolding. |
| PRD-040 | Merged PRD | PRD-TBD-M4-04 | Same authorization boundary and golden decision corpus. |
| PRD-041 | Retained PRD | PRD-TBD-M4-06 | Decision migration and legacy semantics require separate review. |
| PRD-042 | Retained PRD | PRD-TBD-M5-01 | Content inventory is independently useful and prerequisite. |
| PRD-043 | Merged PRD | PRD-TBD-M5-02 | Provenance is useful only when invalidation consumes it. |
| PRD-044 | Merged PRD | PRD-TBD-M5-02 | Same artifact boundary, dependency metadata, and rollback. |
| PRD-045 | Retained PRD | PRD-TBD-M5-03 | Branch/worktree identity has independent Git risk and tests. |
| PRD-046 | Retained PRD | PRD-TBD-M6-01 | Manifest identity is independently useful and prerequisite. |
| PRD-047 | Merged PRD | PRD-TBD-M6-02 | Selection budgets and source fallback jointly determine safe context output. |
| PRD-048 | Merged PRD | PRD-TBD-M6-02 | Separate delivery could create budgeted but epistemically unsafe context. |
| PRD-049 | Retained PRD | PRD-TBD-M6-03 | Neutral contracts are a distinct vendor boundary. |
| PRD-050 | Merged PRD | PRD-TBD-M7-01 | Discovery alone is report scaffolding; runner establishes useful verification. |
| PRD-051 | Merged PRD | PRD-TBD-M7-01 | Same command trust, evidence, and process boundary. |
| PRD-052 | Retained PRD | PRD-TBD-M7-02 | Diff/log/invariant verification is independently useful and lower authority. |
| PRD-053 | Retained PRD | PRD-TBD-M8-01 | Worktree lifecycle has unique destructive-data risk. |
| PRD-054 | Retained PRD | PRD-TBD-M8-02 | Admission can ship safely before an agent supervisor is enabled. |
| PRD-055 | Merged PRD | PRD-TBD-M8-03 | Supervisor and mandatory terminal handoff share one attempt lifecycle. |
| PRD-056 | Merged PRD | PRD-TBD-M8-03 | Checkpoint/handoff is required for a safely useful supervisor. |
| PRD-057 | Retained PRD | PRD-TBD-M8-04 | External effects carry materially different authority and rollback risk. |
| PRD-058 | Merged PRD | PRD-TBD-M9-01 | Reviewer assignment without review context is not a useful review state. |
| PRD-059 | Merged PRD | PRD-TBD-M9-01 | Same review-attempt identity, isolation, and fallback boundary. |
| PRD-060 | Retained PRD | PRD-TBD-M9-02 | Finding disposition has a distinct durable lifecycle. |
| PRD-061 | Merged PRD | PRD-TBD-M9-03 | Acceptance and its human report must reflect the same canonical decision. |
| PRD-062 | Merged PRD | PRD-TBD-M9-03 | A separate report-only PRD would add no independent architectural capability. |
| PRD-063 | Retained PRD | PRD-TBD-M10-01 | Semantic index is optional and independently removable. |
| PRD-064 | Retained PRD | PRD-TBD-M10-02 | Model summaries have distinct privacy and hallucination risks. |
| PRD-065 | Merged PRD | PRD-TBD-M11-01 | Vendor adapters share conformance and capability-selection boundaries. |
| PRD-066 | Merged PRD | PRD-TBD-M11-01 | Avoid premature per-vendor PRDs before the neutral adapter boundary is proven. |
| PRD-067 | Merged PRD | PRD-TBD-M11-01 | Same neutral conformance boundary; adapters remain independently disableable. |
| PRD-068 | Merged PRD | PRD-TBD-M11-01 | Capability selection is the common architectural service supporting adapters. |
| PRD-069 | Retained PRD | PRD-TBD-M11-02 | Specialist multi-review is optional with distinct cost and consensus risk. |

No original implementation item is removed outright. Six items are reclassified as ADRs; the remainder are retained or merged into bounded implementation PRDs.
