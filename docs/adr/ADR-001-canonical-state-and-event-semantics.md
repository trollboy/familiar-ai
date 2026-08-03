# ADR-001 Decision Preparation: Canonical State and Event Semantics

## Status

Proposed decision preparation. No option has been selected.

This document frames the architectural decision required by ADR-001. It is not an implementation plan, migration plan, or recommendation.

## Context

Familiar currently persists projects, file summaries, decisions, and session rollups in conventional SQLite tables. The daemon and MCP process both access that database, while process-local runtime status and inference state remain fragmented. The target architecture changes the responsibility of persistence substantially: a central daemon becomes the sole authority for mutable Familiar state and must preserve tasks, decisions, findings, verification results, handoffs, approvals, warrants, audit events, and references to derived artifacts.

The repository and Git history remain authoritative for source code and repository change state. This ADR concerns canonical **Familiar operational and workflow state** and the history of material Familiar actions. It does not make Familiar's database authoritative over repository source.

The decision must establish whether operational state is represented primarily as current relational rows, reconstructed primarily from an event stream, or represented through a deliberately governed combination of current rows and first-class audit events.

## Decision That Must Be Made

Select the canonical persistence and state-transition model for Familiar's durable operational state and audit history:

- **Option A:** Conventional transactional tables plus append-oriented audit events.
- **Option B:** Full event sourcing with projections.
- **Option C:** A hybrid model where canonical operational state lives in transactional tables and material actions also emit append-oriented audit events.

The selected model must define:

- What constitutes canonical current state.
- What constitutes canonical historical evidence.
- Which state transitions and actions must be durable.
- Whether events are authoritative inputs, supporting audit records, or co-equal evidence.
- How commands, state mutations, and audit records share transaction boundaries.
- Which ordering and concurrency guarantees the daemon exposes.
- How state is recovered after crashes and daemon restarts.
- How schemas and event formats evolve.
- What rollback means for database state, repository state, worktrees, and external effects.
- How thin MCP, CLI, local-socket, and loopback-HTTP adapters observe and mutate state without becoming competing authorities.

## Terminology and Option Boundaries

The wording of options A and C overlaps unless their boundaries are made explicit for evaluation.

### Conventional tables

Normalized or purpose-specific relational tables contain current operational state. Application commands update those tables directly through transactions.

### Append-oriented audit event

An immutable-after-commit record describing an attributable material action or state transition. Append-oriented does not necessarily mean that all current state can be reconstructed from events.

### Projection

A query-optimized state representation derived from an authoritative event stream. Under full event sourcing, projections are disposable or rebuildable views rather than the primary historical authority.

### Option A boundary

Under Option A, conventional tables are canonical. Audit events are a supporting append-oriented record around conventional CRUD and domain operations. Audit completeness may be governed, but the audit stream is not required to be a complete replay source and does not define operational state semantics.

### Option B boundary

Under Option B, the event stream is canonical for Familiar operational state. Current tables and read models are projections built from events. State changes occur by appending accepted domain events.

### Option C boundary

Under Option C, transactional tables are canonical for current operational state, but material domain actions and their audit events form a first-class atomic architectural contract. Every governed material mutation must update current state and append its causal audit event in the same commit boundary. The audit stream is complete for defined material actions, but it is not assumed to be sufficient to reconstruct every byte of current operational state unless separately specified.

The evaluation must decide whether the stronger material-action invariant in Option C is meaningfully distinct and enforceable compared with Option A's supporting audit behavior.

## Governing Constraints

### Philosophy constraints

- The repository is authoritative for source; Familiar's database cannot replace source or Git history.
- Deterministic software must answer questions that do not require model reasoning.
- Architectural decisions remain human-owned and explicit.
- Durable knowledge must survive conversation and model turnover.
- Familiar must preserve architectural integrity, reproducibility, deterministic verification, project history, human approval gates, explicit decision records, and rollback capability.
- Hidden assumptions, silent architectural drift, and unverifiable success are defects.
- Coding agents are replaceable and cannot be sources of canonical authority.

### Target architecture constraints

- One central daemon is the sole authority for mutable Familiar workflow state.
- MCP, CLI, local socket, and loopback HTTP are thin adapters over shared core commands and queries.
- Clients cannot directly mutate canonical storage or maintain competing state.
- Canonical records are project-scoped and carry stable identity, timestamps, provenance, and audit metadata.
- Canonical state must cover projects, tasks, decisions, findings, verification results, handoffs, warrants, approvals, audit events, and derived-artifact references.
- Transactional integrity, project isolation, referential integrity, schema evolution, recovery, durable audit history, and provenance retention are required.
- Material operations must be attributable to an actor, interface, task, warrant, and project as applicable.
- Failures, skipped checks, uncertainty, and disagreement must remain visible.
- External side effects require explicit authority and truthful rollback evidence.
- The system remains local-first, useful without an LLM, and compatible with SQLite unless evidence justifies replacement.

### Current-state constraints

- Existing SQLite data and repository abstractions are working systems to preserve where compatible.
- Current migrations are conventional forward schema migrations.
- SQLite is configured with WAL, foreign keys, and a busy timeout.
- The existing schema has no task, finding, verification, handoff, approval, warrant, or audit-event model.
- Current runtime authority is fragmented, but the delivery architecture requires authority to converge in the daemon.
- Migration complexity and recoverability matter because the system already has user data.

## Option A — Conventional Transactional Tables Plus Append-Oriented Audit Events

### Source of truth

Current operational tables are authoritative for Familiar state. The repository and Git remain authoritative for source. Audit events provide historical evidence but are not required to reconstruct all operational tables or to resolve discrepancies automatically.

If current tables and audit events disagree, operational tables determine current state unless a separate integrity or repair rule states otherwise. The audit log may reveal the discrepancy without being capable of correcting it.

### Transaction boundaries

Domain operations use ordinary SQLite transactions over current-state tables. Audit append may occur inside the same transaction, through a shared persistence layer, or through a separate best-effort mechanism. The decision would need to specify which operations require atomic audit append; without such a rule, audit completeness depends on call-site discipline.

### Ordering guarantees

Current state follows SQLite transaction serialization. Audit ordering can use an autoincrement sequence, transaction-local sequence, timestamp, or causal identifier. Audit order describes committed records but may not represent a complete domain-event order if some operations do not emit audit entries.

### Concurrency behavior

Optimistic version checks, uniqueness constraints, and transaction isolation govern concurrent updates. The central daemon can serialize selected commands or allow concurrent transactions. Conflicts are handled against current rows rather than stream versions.

### Crash recovery

SQLite WAL and transactions recover committed current state. Audit recovery depends on whether audit writes were atomic with state. If audit emission is separate, a crash can leave committed state without corresponding audit evidence or an audit attempt without the intended state.

### Daemon restart behavior

The daemon reads current tables directly and resumes from persisted lifecycle state. It does not replay the audit log to become operational. In-flight work still requires explicit attempt/checkpoint recovery records.

### Schema evolution

Conventional SQL migrations evolve tables. Audit payloads may be versioned, but older audit records need not remain executable as state transitions. Readers must tolerate historical event versions for operator inspection.

### Migration complexity from the current SQLite schema

This option is closest to the existing schema. Existing tables can remain canonical while new domain tables and an audit table are added incrementally. Existing records can receive conservative provenance or legacy status without synthesizing a complete event history.

### Audit completeness

Completeness ranges from partial operational logging to a governed record of defined material actions. Because audit is supporting rather than state-defining, missing audit records do not inherently prevent operational state from existing. Completeness requires explicit coverage rules and tests.

### Rollback semantics

Rollback is a new compensating transaction or restoration of prior data, combined with Git/worktree or external-effect rollback as applicable. The audit log records the rollback action but is not itself used to rewind state. Historical audit records remain append-only.

### Debugging and operator ergonomics

Operators can inspect current state with direct SQL and use familiar relational queries. Audit records provide chronology but may require correlation across heterogeneous payloads. Debugging discrepancies can be difficult when audit coverage is incomplete.

### Performance and storage cost

Current-state reads are direct and efficient. Writes add audit overhead only for emitted records. Storage grows with current tables plus the audit stream, but no projection rebuilds are required.

### Implementation complexity

Relative complexity is low to moderate. The primary risk is not storage mechanics but consistently applying audit coverage, causal identity, redaction, and authorization at every material mutation boundary.

### Long-term maintainability

The model uses widely understood relational practices and is approachable for maintainers. Audit semantics may drift if they remain secondary and distributed across call sites.

### Compatibility with SQLite

Strong. SQLite directly supports transactional tables, foreign keys, WAL recovery, indexes, and append-oriented audit tables. Single-writer characteristics align with a daemon-owned authority.

### Compatibility with thin adapters

Strong if all adapters call daemon-owned application services. Thin clients neither need nor receive audit-storage semantics. Direct client writes must still be removed.

### Compatibility with unattended execution, approvals, warrants, verification, and handoffs

Operational tables naturally represent current task, warrant, approval, attempt, finding, verification, and handoff state. Audit history can record material actions, but completeness and atomicity are additional governance obligations rather than consequences of the persistence model.

### Failure modes

- A state mutation commits without its audit record.
- Different application services emit audit records with inconsistent granularity.
- Audit payloads omit prior state, causal identity, approval, or warrant context.
- Direct maintenance writes bypass audit behavior.
- Current state is valid but historical reconstruction is incomplete.
- Duplicate audit entries appear after retries when idempotency is weak.
- Audit redaction removes evidence needed for later analysis, or insufficient redaction leaks secrets.

### Irreversible consequences

- Historical actions not recorded at commit time generally cannot be reconstructed reliably later.
- Synthesizing audit history from current rows creates inferred rather than observed evidence.
- If consumers begin treating an incomplete audit log as authoritative, later tightening of semantics can invalidate assumptions and reports.
- Schema choices that overwrite rather than preserve superseded state can permanently lose rationale unless separately audited.

## Option B — Full Event Sourcing with Projections

### Source of truth

The append-only domain event stream is authoritative for Familiar operational state. Current project, task, decision, finding, verification, handoff, approval, and warrant representations are projections derived from accepted events. The repository and Git remain independently authoritative for source and repository history.

Disagreement between a projection and the event stream is resolved by rebuilding or repairing the projection from events.

### Transaction boundaries

A command validates against a stream version or aggregate state and atomically appends one or more events. Multi-aggregate invariants require a defined transaction model, coordination aggregate, process manager, or SQLite transaction spanning affected streams. Projection updates may be synchronous in the append transaction or asynchronous with checkpointing.

### Ordering guarantees

The system can provide a global commit sequence, per-aggregate sequence, or both. Per-aggregate ordering supports conflict detection. Global ordering simplifies deterministic projection rebuild and audit chronology but centralizes sequencing.

The decision must define whether event ordering represents command acceptance, commit order, causal order, or all three through separate identifiers.

### Concurrency behavior

Expected stream versions provide optimistic concurrency control. Conflicting commands are rejected or retried after reloading events. Cross-aggregate invariants are more complex than row-level transactions and require explicit coordination semantics.

### Crash recovery

Committed events survive through SQLite transactions. Projections recover by replaying from a durable checkpoint. If events commit but a projection update fails, the projection is stale but reconstructible. Event append must remain atomic and durable.

### Daemon restart behavior

The daemon verifies projection checkpoints, catches projections up from the event log, and may rebuild invalid projections. Startup time and service availability depend on projection size, checkpoint validity, and replay strategy.

### Schema evolution

Event schemas are historical contracts. Evolution requires versioned event types, upcasters, compatibility readers, or new compensating events. Projections can evolve and rebuild, but old events must remain interpretable for the supported retention horizon.

### Migration complexity from the current SQLite schema

High. Existing rows do not contain the event history that would have produced them. Migration options include importing each current record as a baseline/snapshot event, creating synthetic historical events with explicit provenance, or establishing an initial state snapshot followed by real events. None recreates missing historical causality.

### Audit completeness

Potentially very high for domain state transitions because every accepted change is an event. Completeness still depends on modeling all material actions, including reads requiring audit, process starts, tool invocations, external effects, and failures that do not change aggregate state. A domain event stream is not automatically a complete operational security audit.

### Rollback semantics

Events are not deleted or reversed in place. Rollback is expressed through compensating events that produce a new state, or by creating a new branch/snapshot for analysis. Repository and external effects still require their own rollback mechanisms. Time-travel projection does not undo real-world side effects.

### Debugging and operator ergonomics

Event history provides rich causal inspection and reproducible state derivation. Current state is less convenient to diagnose if projections are stale or event tooling is weak. Direct SQL edits to projections are invalid repairs unless followed by a principled rebuild or corrective event.

### Performance and storage cost

Append operations can be efficient. Current reads depend on projections and can also be efficient. Storage grows without in-place replacement, and projection indexes duplicate selected state. Replay and rebuild cost increases with history unless snapshots or compaction strategies are introduced.

### Implementation complexity

High. It requires event envelopes, aggregate boundaries, stream versioning, projection infrastructure, checkpointing, replay, idempotency, upcasting, snapshots or compaction policy, event-aware tests, and operator tooling.

### Long-term maintainability

The model preserves rich history and makes state transition semantics explicit. It also imposes permanent event-contract obligations and specialized operational knowledge. Poor aggregate or event design becomes costly to change.

### Compatibility with SQLite

Technically compatible. SQLite can store ordered streams and projections transactionally, especially under a single daemon writer. Large replay histories, global sequencing, projection concurrency, retention, and compaction require careful design but do not inherently require another database.

### Compatibility with thin adapters

Strong at the external boundary if adapters send commands and queries only. Adapters remain unaware of streams. Command responses may need to address eventual projection consistency if projection updates are asynchronous.

### Compatibility with unattended execution, approvals, warrants, verification, and handoffs

Strong for preserving lifecycle history, authorization decisions, revocation, checkpoints, findings, and handoffs as domain events. Cross-domain invariants—such as atomically consuming warrant authority, starting an execution attempt, and recording approval evidence—require carefully designed aggregate and transaction boundaries.

### Failure modes

- An event type captures an incorrect or insufficient domain fact permanently.
- Projection code produces incorrect current state.
- Projection lag causes stale queries or policy decisions.
- Upcasters change historical interpretation incorrectly.
- Cross-aggregate invariants admit inconsistent event combinations.
- Retry logic appends duplicate events.
- Snapshot or compaction policy prevents faithful reconstruction.
- An operational action is incorrectly assumed audited merely because domain state is event-sourced.
- Replay time or corrupted checkpoints delay daemon availability.

### Irreversible consequences

- Published event schemas and semantics become long-lived compatibility contracts.
- Historical events cannot be casually rewritten without undermining audit integrity and projection determinism.
- Incorrect event granularity or aggregate boundaries can require permanent translation layers.
- Migrating existing row state into synthetic events creates a historical boundary that cannot recover missing prior causality.
- Once downstream logic depends on replay and stream versions, returning to ordinary row authority requires a substantial semantic migration.

## Option C — Transactional Canonical State with First-Class Material-Action Audit Events

### Source of truth

Transactional tables are authoritative for current Familiar operational state. Append-oriented audit events are authoritative evidence that defined material actions occurred with specified actor, authority, causality, inputs, and outcomes. The two stores answer different questions: current tables answer "what is the current operational state?" and audit events answer "what material actions and transitions occurred?"

The repository and Git remain authoritative for source and repository change history. Neither database representation replaces them.

### Transaction boundaries

Every material state-changing command must update canonical tables and append its causal audit event atomically in one SQLite transaction. The command boundary—not arbitrary table mutation—is the unit of consistency. Material actions that do not change canonical state, such as a denied command or external process attempt, require explicit audit transactions with outcome records.

External effects cannot be atomically committed with SQLite. They require intent, attempt, observed outcome, and compensating/rollback evidence records governed as a state machine.

### Ordering guarantees

SQLite commit order provides an append sequence for audit events. Domain records may carry versions for optimistic concurrency. Causal identifiers connect command, approval, warrant, attempt, verification, review, and handoff records. The audit sequence establishes committed observation order, while causal links express relationships that timestamps alone cannot.

### Concurrency behavior

Current-table constraints and record versions govern conflicts. The daemon owns command serialization where policy requires it and can otherwise allow transactions to compete. Audit events are appended only for accepted, rejected, failed, or completed material actions according to defined coverage; retries require idempotency keys.

### Crash recovery

Atomic table mutation and audit append either both commit or both roll back. SQLite WAL recovers the committed pair. For external processes and effects, recovery examines durable intent/attempt state and audit evidence to classify interrupted, unknown, completed, or compensating work.

### Daemon restart behavior

The daemon reads current operational tables directly and performs bounded reconciliation of incomplete attempts, effects, or checkpoints using audit and lifecycle records. It does not replay all historical events to reconstruct normal current state.

### Schema evolution

Current tables use conventional migrations. Audit envelopes retain stable common metadata while versioned payloads evolve. Because audit events are evidence rather than the sole replay source, historical payloads must remain inspectable but do not necessarily require executable upcasters capable of rebuilding all current state.

### Migration complexity from the current SQLite schema

Moderate. Existing tables can remain canonical while new domain tables, record versions, common causal metadata, and audit-event storage are introduced. Existing records can be marked as legacy baseline state without fabricating material-action history. Future governed commands begin the complete audit era from an explicit boundary.

### Audit completeness

High for the explicitly defined set of material actions if the core command boundary prevents unaudited writes. Completeness is testable as a command invariant. It remains necessary to define which queries, denials, tool calls, process events, external effects, and failures are material.

Unlike full event sourcing, audit completeness does not imply complete reconstructibility of every current table. Unlike Option A, omission of a required event is a violation of the core mutation contract rather than merely reduced diagnostic coverage.

### Rollback semantics

Rollback is a new canonical state transition with its own audit event and evidence. Current tables move to the resulting state; historical audit events remain. Repository/worktree and external-effect rollback remain distinct mechanisms linked through causal identity.

### Debugging and operator ergonomics

Operators inspect current state directly with relational queries and follow material actions through a common ordered audit envelope. Repair normally occurs through governed compensating commands. Direct database repair remains an exceptional operator action that must be separately recorded.

### Performance and storage cost

Current reads remain direct. Each material mutation adds an append write and indexes for causal lookup. Storage grows with audit history but avoids projection duplication and routine replay. External process logs and artifacts can dominate storage independently and require retention policy.

### Implementation complexity

Moderate to high. It requires a strict core command boundary, atomic state/audit transactions, common event envelopes, idempotency, record versioning, coverage tests, causal linkage, redaction, and recovery state machines for non-transactional effects. It does not require general projection/replay infrastructure.

### Long-term maintainability

The relational operational model remains familiar. The first-class audit contract centralizes historical semantics and discourages secondary call-site logging. Maintainability depends on keeping "material action" definitions, event payload versions, and command coverage explicit.

### Compatibility with SQLite

Strong. SQLite supports atomic mutations and audit appends, ordered identifiers, foreign keys, WAL recovery, and daemon-owned single-writer coordination. External effects still require an application-level transactional-outbox/state-machine pattern because no database can atomically commit arbitrary host or network effects.

### Compatibility with thin adapters

Strong. Adapters call core commands and queries; only the core can execute atomic state/audit transitions. Adapter identity and request causality enter the command context but do not change persistence semantics.

### Compatibility with unattended execution, approvals, warrants, verification, and handoffs

Strong for direct current-state queries and complete material-action evidence. Task, warrant, approval, execution attempt, finding, verification, and handoff tables represent operational state; atomic audit events record authorization and transitions. External execution and effects require durable intent and observed-outcome stages.

### Failure modes

- A mutation path bypasses the governed core command boundary.
- A material action is incorrectly classified as non-material and not audited.
- An event commits with insufficient causal, actor, approval, or warrant context.
- Retry without an idempotency key repeats a state transition or audit event.
- External effect state becomes unknown after a crash between effect and observation.
- Current-table and audit schemas evolve inconsistently.
- Operators directly edit current tables without a governed repair record.
- Audit volume or retention policy makes causal inspection incomplete.

### Irreversible consequences

- Material actions omitted before the audit boundary cannot be reconstructed reliably later.
- Once consumers rely on specific audit event meanings, those meanings become durable compatibility contracts even though events are not replay authority.
- An overly broad or narrow definition of material action can permanently create excessive sensitive history or historical blind spots.
- Treating audit events as replayable state after the fact would require semantics they may not contain.
- External effects recorded with inadequate intent/outcome boundaries may remain permanently ambiguous.

## Decision Matrix

The matrix uses qualitative characteristics, not scores. It is intended to expose tradeoffs rather than rank the options.

| Criterion | Option A: Tables + supporting audit | Option B: Full event sourcing | Option C: Tables + first-class material-action audit |
|---|---|---|---|
| Canonical current state | Transactional tables | Event stream; projections serve reads | Transactional tables |
| Canonical historical evidence | Audit is supporting and may be incomplete unless separately governed | Domain event stream for state transitions; separate operational audit may still be needed | Audit stream is authoritative for defined material actions |
| State reconstruction | From current tables/backups | From events plus projection logic/snapshots | From current tables/backups; audit supports explanation and reconciliation |
| Mutation/audit atomicity | Optional unless explicitly mandated | Event append is the mutation | Required for governed state-changing commands |
| Ordering | Database commits; audit ordering may be partial | Explicit per-stream/global event ordering | Commit sequence plus causal identifiers |
| Concurrency model | Row constraints/versions and transactions | Expected stream versions; explicit cross-aggregate coordination | Row constraints/versions, transactions, and daemon command coordination |
| Crash recovery | WAL current-state recovery; audit gaps possible if non-atomic | Event recovery plus projection catch-up/rebuild | WAL state/audit recovery; explicit incomplete-effect reconciliation |
| Restart cost | Direct state load | Projection verification, catch-up, or replay | Direct state load plus bounded attempt/effect reconciliation |
| Schema evolution | Conventional SQL; audit readers tolerate old payloads | Permanent event-version/upcaster/projection obligations | Conventional SQL plus versioned evidence payloads |
| Migration from current schema | Low to moderate | High | Moderate |
| Audit completeness | Depends on distributed coverage rules | High for modeled state transitions; not automatically complete operational audit | High for defined material actions when command invariant is enforced |
| Rollback representation | Compensating table mutation plus audit | Compensating events | Governed state transition plus audit evidence |
| Current-state query ergonomics | Direct relational queries | Projection queries | Direct relational queries |
| Historical debugging | Limited by audit coverage | Rich event history; requires event tooling | Rich material-action history plus direct current state |
| Storage cost | Current state plus selected audit | Full history plus projections/snapshots | Current state plus complete material-action audit |
| Implementation complexity | Low to moderate | High | Moderate to high |
| Specialized operational knowledge | Low | High | Moderate |
| SQLite fit | Strong | Compatible, with careful replay/projection design | Strong |
| Thin-adapter fit | Strong with daemon command boundary | Strong; possible projection-consistency concerns | Strong with daemon command boundary |
| Unattended execution fit | Operational state is simple; audit rigor must be added | Lifecycle history is natural; cross-aggregate/effect semantics are complex | Current lifecycle plus atomic evidence; external effects still need state machines |
| Approval/warrant fit | Direct rows; audit may vary | Events preserve changes and revocations | Direct rows plus required attributable action events |
| Verification/handoff fit | Direct rows and optional chronology | Natural historical sequence and rebuildable views | Direct rows plus complete governed chronology |
| Reversibility of architectural choice | Relatively easier before consumers rely on audit semantics | Hard once event contracts and replay are foundational | Moderate; audit contracts persist, but current-state authority remains relational |

## Cross-Cutting Failure Modes

These failure modes apply regardless of the selected option and therefore cannot be delegated to the persistence choice alone:

- Repository source and derived Familiar state are confused.
- A client bypasses the daemon and mutates storage directly.
- Actor, project, task, approval, warrant, or causal identity is missing.
- A retry performs a material action twice.
- A failed or denied action is omitted from evidence because it did not change current state.
- External filesystem, process, Git, or network effects are assumed transactional with SQLite.
- Sensitive values appear in event payloads, logs, or retained artifacts.
- Clock timestamps are used as the sole ordering or causal mechanism.
- Schema migration succeeds partially or cannot be replayed deterministically.
- Database backup is mistaken for repository, worktree, or external-effect rollback.
- Audit retention deletes evidence still referenced by decisions, findings, verification, or handoffs.
- Direct operator repair silently changes canonical state.

## Later Milestones Affected

### M2 — Central Core Authority and Shared Interfaces

ADR-001 determines the shared command/query semantics, transaction ownership, idempotency model, local IPC response consistency, and how direct MCP mutation authority is retired.

Affected provisional PRDs:

- `PRD-TBD-M2-01` — Core Contracts and Authenticated Local IPC.
- `PRD-TBD-M2-02` — MCP Core-Service Cutover.
- `PRD-TBD-M2-03` — Authoritative Status, Dashboard, and Tray Adapters.
- `PRD-TBD-M2-04` — Core CLI Adapter.

### M3 — Responsive Control Plane and Host Trust Baseline

The choice affects storage-worker ownership, transaction sequencing, projection or audit processing, shutdown drainage, and recovery responsiveness.

Affected provisional PRDs:

- `PRD-TBD-M3-01` — Blocking Work Execution Boundary.
- `PRD-TBD-M3-03` — Local Client, Loopback, and Credential Trust Boundary.

### M4 — Canonical Stewardship State, Policy, Audit, and Memory

This milestone is most directly blocked. ADR-001 determines whether task, finding, verification, approval, warrant, decision, and handoff records are canonical rows, projections, or row state paired with first-class events.

Affected provisional PRDs:

- `PRD-TBD-M4-01` — Canonical Task State.
- `PRD-TBD-M4-02` — Canonical Finding and Verification Evidence State.
- `PRD-TBD-M4-03` — Append-Oriented Audit Events.
- `PRD-TBD-M4-04` — Approval, Warrant, and Deterministic Policy State.
- `PRD-TBD-M4-05` — Canonical Handoff State.
- `PRD-TBD-M4-06` — Decision Provenance and Supersession.

### M5 — Content-Addressed Repository Intelligence

The choice affects how invalidation, source observations, branch/worktree identity, and provenance changes are recorded and recovered.

Affected provisional PRDs:

- `PRD-TBD-M5-01` — Content-Hashed File Inventory.
- `PRD-TBD-M5-02` — Derived Artifact Provenance and Invalidation.
- `PRD-TBD-M5-03` — Branch and Worktree Intelligence Identity.

### M6 — Reproducible Context Compilation and Agent Contracts

Context manifests require stable state versions, event or record identities, provenance, and snapshot consistency. The choice affects how a context package identifies its exact input state.

Affected provisional PRDs:

- `PRD-TBD-M6-01` — Immutable Context Manifest.
- `PRD-TBD-M6-02` — Budgeted Selection and Authoritative Source Fallback.

### M7 — Deterministic Verification and Evidence

The choice determines how verification attempts, logs, results, failures, retries, and policy evaluations become durable and ordered.

Affected provisional PRDs:

- `PRD-TBD-M7-01` — Verification Policy Resolution and Supervised Runner.
- `PRD-TBD-M7-02` — Diff, Log, Repository, and Invariant Verification.

### M8 — Bounded Isolated Execution

Execution requires precise transaction and recovery semantics for warrant admission, worktree allocation, process intent, attempt start, checkpoint, cancellation, external effects, and terminal handoff.

Affected provisional PRDs:

- `PRD-TBD-M8-01` — Safe Worktree Lifecycle.
- `PRD-TBD-M8-02` — Warrant Validation and Execution Admission.
- `PRD-TBD-M8-03` — Bounded Agent Supervisor and Terminal Handoff.
- `PRD-TBD-M8-04` — External Effects and Rollback Evidence.

### M9 — Independent Adversarial Review and Acceptance

The choice affects ordering and auditability of reviewer assignment, finding creation, disposition, acceptance, waiver, and report generation.

Affected provisional PRDs:

- `PRD-TBD-M9-01` — Independent Reviewer Assignment and Review Context.
- `PRD-TBD-M9-02` — Review Finding Ingestion and Disposition.
- `PRD-TBD-M9-03` — Acceptance Gate and Engineering Report.

### M10 and M11 — Optional Enhancements

Optional semantic artifacts, adapter observations, model health, selection decisions, and specialist review actions inherit the chosen provenance and audit semantics but do not independently determine them.

Affected provisional PRDs:

- `PRD-TBD-M10-01` and `PRD-TBD-M10-02`.
- `PRD-TBD-M11-01` and `PRD-TBD-M11-02`.

## Evidence Required Before Deciding

The decision should be supported by evidence rather than architectural fashion. Useful evidence includes:

- Expected volumes for tasks, actions, verification logs, findings, and retained audit history.
- Required maximum daemon restart time and acceptable degraded availability.
- Concrete cross-record invariants for task, warrant, approval, attempt, verification, finding, and handoff transitions.
- Examples of crash points around database commits, process starts, worktree changes, and external effects.
- The set of actions that must be reconstructible versus merely attributable.
- Operator workflows for inspecting state, repairing corruption, recovering interrupted work, and explaining an acceptance decision.
- SQLite tests for append throughput, index size, WAL recovery, long-history queries, and any proposed projection rebuild.
- A migration proof using representative existing databases, including legacy decisions, rollups, and summaries.
- A schema-evolution exercise showing how an old event or audit payload remains interpretable after domain changes.
- A redaction exercise demonstrating that audit completeness does not require retaining secrets.

## Open Questions

1. Which exact records constitute canonical operational state, and which are derived projections or artifacts?
2. Which actions are "material" and must be recorded even when they do not mutate current state?
3. Must the historical record reconstruct current workflow state exactly, explain it sufficiently, or satisfy both requirements for selected domains?
4. Is global ordering required, or are per-project/per-task ordering plus causal links sufficient?
5. Which commands require optimistic record/stream versions, and which must be serialized by the daemon?
6. Must audit append be atomic with every canonical mutation, or only with a defined subset?
7. How are denied commands, failed validations, failed model/tool requests, and read access represented?
8. How are retries identified so state transitions, effects, and audit evidence remain idempotent?
9. What is the recovery state machine when a process or external effect may have occurred but its outcome is unknown?
10. What restart-time and projection-rebuild limits are acceptable?
11. How long must audit events, verification logs, model responses, and external-effect evidence be retained?
12. Which payload fields are structured columns, versioned JSON, external artifacts, or hashes?
13. How are sensitive values redacted while retaining sufficient evidence?
14. What operator actions are permitted for repair, and how are repairs themselves audited?
15. Can any audit history be compacted, summarized, archived, or deleted without violating project history and decision evidence?
16. How should existing data be marked at the historical boundary without fabricating events that never occurred?
17. If Option B is selected, what are the aggregate boundaries and consistency rules for multi-aggregate transitions?
18. If Option A is selected, what mechanism prevents audit coverage from remaining best-effort?
19. If Option C is selected, what formal test distinguishes its first-class material-action invariant from Option A?
20. What consistency does each thin adapter observe immediately after a successful command response?

## Decision Criteria

The human decision should explicitly evaluate each option against these criteria:

1. **Correct authority:** The model maintains one daemon-owned operational authority without displacing repository truth.
2. **Deterministic integrity:** State transitions, ordering, conflicts, and recovery have testable deterministic semantics.
3. **Audit sufficiency:** Material actions, authorization, evidence, failure, and disposition can be reconstructed to the required level.
4. **Crash safety:** Database mutations, process attempts, and external effects have explicit recovery states.
5. **Rollback truth:** Compensating transitions and real-world rollback are distinguishable from historical replay or database restoration.
6. **Migration safety:** Existing SQLite data can cross the historical boundary without fabricated causality or unacceptable loss.
7. **Schema longevity:** Current and historical formats can evolve without silent reinterpretation.
8. **Operational clarity:** A local operator can inspect, diagnose, repair, back up, and recover the system.
9. **SQLite fit:** The design works predictably within SQLite's concurrency, WAL, storage, and migration characteristics.
10. **Thin-client consistency:** MCP, CLI, socket, and HTTP observe the same command and query semantics.
11. **Execution readiness:** Approvals, warrants, attempts, verification, findings, handoffs, and external effects can be modeled safely.
12. **Complexity proportionality:** Added infrastructure demonstrably improves correctness, repeatability, safety, velocity, or observability.
13. **Maintainability:** Future maintainers can understand and evolve the model without specialized machinery disproportionate to Familiar's scope.
14. **Irreversibility:** Permanent event contracts, missing historical evidence, or authority assumptions are understood before adoption.

No criterion is assigned a weight in this preparation document. Weighting and any disqualifying threshold require human approval as part of the decision.

## Proposed Decision Record Template

The final ADR may use the following structure after the open questions are resolved:

```markdown
# ADR-001: Canonical State and Event Semantics

## Status

Accepted | Rejected | Superseded

## Date

YYYY-MM-DD

## Decision Owners and Approvers

- Decision owner:
- Human approver(s):

## Context

Describe the current persistence model, target responsibilities, and the
specific authority and audit problem being decided.

## Decision

Selected option: A | B | C

Define precisely:

- canonical current-state authority
- canonical historical evidence
- transaction and atomicity rules
- ordering and concurrency rules
- idempotency requirements
- crash and restart recovery
- schema/event versioning
- audit coverage
- rollback and compensation semantics
- direct-storage prohibitions
- thin-adapter consistency

## Decision Criteria Applied

Record criteria weights or disqualifying thresholds and the evidence used.

## Options Considered

### Option A

Record accepted benefits, rejected risks, and evidence.

### Option B

Record accepted benefits, rejected risks, and evidence.

### Option C

Record accepted benefits, rejected risks, and evidence.

## Consequences

### Positive

### Negative

### Irreversible or Costly-to-Reverse Consequences

## Invariants

List the persistence, authority, audit, and recovery invariants that all later
milestones must preserve.

## Historical Boundary and Migration Constraints

Define how existing data is represented without fabricating prior events.

## Verification Evidence

List prototypes, crash tests, SQLite measurements, migration rehearsals, and
operator exercises supporting the decision.

## Downstream Impact

List blocked milestones, epics, and provisional PRDs whose boundaries change.

## Rollback or Supersession Conditions

State what evidence would invalidate the decision and what can or cannot be
reversed after implementation.

## Open Follow-Ups

Record only questions that do not prevent accepting the ADR.
```
