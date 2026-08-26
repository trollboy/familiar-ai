# PRD-001 Sequencing Review

**Status:** Completed dependency review
**Date:** 2026-08-03  
**Subject:** Whether Canonical File Identity Boundary should remain the first implementation PRD

## Executive Summary

The existing repository provides enough persistence and runtime structure to implement the Canonical File Identity Boundary without first introducing a new canonical-state runtime. Familiar already has project records with stable database identifiers and repository roots, a migrated SQLite database, project-scoped file-summary persistence, a uniqueness constraint over project and path, daemon-driven watcher and summary processing, and deterministic in-memory storage tests. What it lacks is a single validated definition of the path portion of that existing identity.

The repository does not yet implement the normative command, query, and event contracts. The daemon's current command abstraction is an in-memory control channel, while MCP opens a separate writable database connection and exposes mutating storage operations. Those are material target-state gaps, but the approved roadmap assigns their correction to M2, independently of M1. Making all of M2—or parts of M2 and later audit work—a prerequisite for file identity would increase scope and churn without changing the path invariant that PRD-001 must establish.

A smaller predecessor containing only path identity primitives would not have a useful completion boundary separate from PRD-001. It would be scaffolding for the same producer, consumer, and persistence-boundary adoption that makes the identity effective. A broader “Canonical State Runtime” predecessor would duplicate current database and repository facilities or prematurely combine the M2 command/query boundary with later event and governance work.

## Review Basis

This review treats the architecture and contracts as governing direction while testing their proposed delivery order against the code that exists today.

- The roadmap explicitly permits M1 and M2 to proceed independently: repository identity correction does not require centralized IPC, and central authority does not require richer repository intelligence ([roadmap.md](../architecture/roadmap.md#L51)).
- M1 names the existing project identity, watcher events, summary pipeline, storage repositories, and containment behavior as its implementation foundation ([roadmap.md](../architecture/roadmap.md#L75)).
- The delivery backlog identifies ADR-001 plus those existing facilities as the dependencies of the canonical file identity epic ([delivery-backlog.md](../architecture/delivery-backlog.md#L95)). It separately assigns shared contracts and authenticated local IPC to M2 ([delivery-backlog.md](../architecture/delivery-backlog.md#L114)).
- PRD-001 intentionally excludes the future daemon command/query runtime, direct-client cutover, and audited event infrastructure ([PRD-001.md](PRD-001.md#L72)).
- The subsystem architecture requires the eventual daemon-owned, protocol-neutral command and query boundary, but also defines Repository Identity and Reconciliation as its own subsystem with source/Git authority and derived scan state ([subsystems.md](../architecture/subsystems.md#L18), [subsystems.md](../architecture/subsystems.md#L118)).

## Existing Repository Evidence

### Storage and migrations

`familiar-storage` already supplies a concrete `Database`, migration runner, and repositories. The initial migration creates `projects`, `file_summaries`, `decisions`, and `session_rollups`; projects have a unique repository root, and file summaries have a unique `(project_id, path)` key ([001_init.sql](../../crates/familiar-storage/migrations/001_init.sql#L1), [001_init.sql](../../crates/familiar-storage/migrations/001_init.sql#L14)). The migration runner is centralized and idempotently records applied versions ([migrate.rs](../../crates/familiar-storage/src/migrate.rs#L21)).

The file-summary repository already supports upsert, exact project/path lookup, project-scoped listing, deletion, module-prefix queries, and search ([file_summary.rs](../../crates/familiar-storage/src/repos/file_summary.rs#L9)). The storage shape therefore already represents the intended identity pair. Its defect is semantic: the `path` value is accepted without a shared canonical validation boundary.

Project, decision, and session-rollup repositories already persist their respective records. Their existence demonstrates that a new generic persistence runtime is not necessary merely to store or retrieve file identity. It does not imply that current storage meets the accepted daemon-only mutation architecture.

### Current daemon ownership

The daemon opens the configured database, runs migrations, owns that connection during its process lifetime, registers projects discovered by the watcher, and drives initial scans and summary work ([main.rs](../../crates/familiar-daemon/src/main.rs#L79), [main.rs](../../crates/familiar-daemon/src/main.rs#L439)). This is enough runtime ownership for daemon-originated file-summary identity changes.

Ownership is not yet exclusive. The MCP executable independently opens the same database and runs migrations ([familiar-mcp.rs](../../crates/familiar-mcp/src/bin/familiar-mcp.rs#L45)). Its `Storage` trait includes direct creation of decisions, session rollups, and file summaries, and `SqliteStorage` delegates those calls straight to repositories ([storage.rs](../../crates/familiar-mcp/src/storage.rs#L32), [storage.rs](../../crates/familiar-mcp/src/storage.rs#L101)). This is the M2 authority gap; it does not erase the reusable storage boundary available to M1.

### Concrete file-identity drift

The daemon summary worker converts observed paths with lossy string conversion, uses that value for staleness lookup, and stores it as the summary path ([summary_worker.rs](../../crates/familiar-daemon/src/summary_worker.rs#L174), [summary_worker.rs](../../crates/familiar-daemon/src/summary_worker.rs#L198)). Because watcher requests carry host paths, daemon-originated summaries can persist absolute, machine-specific identities.

The lazy MCP path takes a different route: it resolves a caller-supplied path within the project root but writes the supplied relative string unchanged ([get_file_summary.rs](../../crates/familiar-mcp/src/tools/get_file_summary.rs#L66), [get_file_summary.rs](../../crates/familiar-mcp/src/tools/get_file_summary.rs#L134), [get_file_summary.rs](../../crates/familiar-mcp/src/tools/get_file_summary.rs#L159)). The same repository entry can therefore acquire competing identities depending on its producer. Every additional summary produced before the boundary is corrected can enlarge the later reconciliation set.

### Current command, query, and event abstractions

The daemon's `DaemonCommand` currently covers LLM enable/disable, heavy-task pause/resume, and quit. It has no canonical envelope, stable request identity, optimistic concurrency, authorization, or audit atomicity ([command.rs](../../crates/familiar-daemon/src/command.rs#L14)). The MCP `Storage` trait is an interface-specific persistence façade, not the normative Query Layer or Command Layer.

There is no implementation of the normative event/evidence model. The accepted contracts eventually require every governed mutation to use daemon-owned command handlers, canonical reads to use the query contract, and material state changes to append evidence atomically ([command-model.md](../contracts/command-model.md#L499), [query-model.md](../contracts/query-model.md#L6), [event-model.md](../contracts/event-model.md#L568)). These are real architectural obligations. They are not proof that a generic runtime must precede a bounded correction to derived file-summary identity.

## Option A: PRD-001 Remains Canonical File Identity Boundary

### Exact dependencies

The option depends on:

1. ADR-001's distinction between canonical operational state, repository source truth, and derived evidence.
2. Existing project IDs and registered repository roots.
3. Existing watcher events and initial-scan inputs that associate observed files with repository roots.
4. The current summary worker, lazy lookup, and module-query paths.
5. The existing file-summary repository and `(project_id, path)` uniqueness boundary.
6. Existing containment behavior and filesystem facilities on supported hosts.
7. Compatibility reads for legacy absolute-path rows.

All are present. ADR-002 and ADR-003 are not functional dependencies because this work grants no human approval or execution authority. Full command/query/event implementations are not semantic dependencies of canonical path derivation.

### Architectural churn risk

Churn is low if canonical identity is expressed as a protocol-neutral repository-domain operation and enforced at the file-summary persistence boundary. M2 can later place daemon-owned command and query handlers around the same operation without changing its semantics. The operation must not be embedded only in an MCP tool, daemon loop, or SQLite statement.

The primary churn risk is allowing PRD-001 to treat today's direct storage callers as permanent authority. Its existing non-goals avoid that: it preserves compatibility with future core services while leaving client cutover to M2.

### Compatibility with the existing repository

This is an extension of working components rather than a replacement. It uses the existing project root, summary model, repository interface, scan, watcher, lazy lookup, and module queries. It requires no destructive migration and leaves legacy rows intact.

### Sufficiency of current storage abstractions

The current abstractions are sufficient for this bounded identity change. They already scope summaries by `project_id`, key them by `path`, and provide all affected reads and writes. They do not provide the eventual canonical runtime, but PRD-001 needs a validated identity value and consistent adoption—not a new general state engine.

Persistence-boundary validation is valuable precisely because current writes originate from more than one process. It prevents new invalid summary identities while the M2 authority cutover remains pending.

### Relationship to command, query, and event contracts

The contracts need not be implemented first. File summaries are derived repository intelligence; Git and source remain authoritative. Canonical path semantics can be established deterministically before protocol-neutral commands and queries wrap the subsystem.

Any file-summary mutation later classified as a governed material action must pass through the future command and evidence boundary. PRD-001 neither exempts that work nor attempts to implement it partially. Requiring the full contract runtime first would conflate the stable domain invariant with its eventual invocation and auditing mechanisms.

### Delay and absolute-path drift

Keeping PRD-001 first stops the known daemon absolute-path producer early. Delaying it allows additional machine-specific records, duplicates, and canonical-versus-legacy conflicts, increasing the risk and volume of PRD-TBD-M1-02 reconciliation.

### Rollback boundary

The rollback is narrow and non-destructive: revert new-write selection and canonical enforcement while retaining compatibility reads. No legacy rewrite is included. Any newly written relative rows remain intelligible under the existing project/path schema.

### Testability

The work is independently testable with deterministic path fixtures, temporary repositories, symlink and containment cases, two-project isolation cases, in-memory repository tests, daemon initial-scan/watcher convergence tests, and MCP lazy/module lookup tests. No long-running daemon IPC or authentication environment is required to prove the identity invariant.

### Scope size

The scope crosses several producers and consumers, but they share one invariant, one rollback boundary, and one end-to-end acceptance condition. Splitting out only the identity primitive would leave a temporary state in which the primitive exists but invalid producers continue writing absolute identities.

### Roadmap and backlog consistency

This option follows the explicit M1/M2 independence rule and the dependency ordering already recorded for PRD-TBD-M1-01, legacy reconciliation, and repository lifecycle reconciliation. It also preserves the backlog's judgment that contracts plus usable IPC belong together because contracts alone would be scaffolding-only architecture ([delivery-backlog.md](../architecture/delivery-backlog.md#L698)).

## Option B: A Foundational Runtime PRD Precedes File Identity

### Exact dependencies

A meaningful canonical runtime would depend on more than ADR-001. At minimum it would require accepted principal and local authentication semantics from ADR-002, daemon availability behavior, a protocol-neutral command/query service boundary, request identity and concurrency behavior, authenticated local IPC, adapter compatibility, and an authority cutover strategy. If material audit events were included, it would also need event taxonomy, atomic command outcome persistence, consistency checking, retention, and historical-boundary behavior.

Those dependencies substantially reproduce M2 and draw in later canonical-state and audit work. A runtime limited to the current `Database` and repositories would have no distinct capability beyond what exists.

### Architectural churn risk

The churn risk is high. A predecessor broad enough to be useful would alter daemon composition, storage access, MCP integration, command/query dispatch, identity binding, and likely persistence. Implementing only a subset risks creating a temporary authority layer that must be revised once approvals, warrants, audit semantics, and remaining canonical schemas arrive.

Conversely, a very small predecessor—such as a transaction wrapper or repository façade—would duplicate `Database`, the migration runner, and repository traits without satisfying the normative contracts.

### Compatibility with the existing repository

The current system is built around synchronous repository calls behind `Arc<Mutex<Database>>` in the daemon and MCP. A new runtime would require compatibility adapters or an early cutover across both processes. That work is possible, but it is materially broader than the file identity defect and has its own availability and rollback decisions.

### Duplication of existing infrastructure

There are two plausible meanings of “Canonical State Runtime”:

- A persistence-focused runtime would repeat existing SQLite opening, migration, transaction, and repository responsibilities. It would not resolve direct MCP authority, canonical command semantics, or evidence atomicity.
- An authority-focused runtime would implement M2's core contracts and local IPC, potentially plus portions of the later canonical state and audit milestones. It would not be a smaller predecessor.

Repository evidence supports neither as a necessary first step for file identity.

### Relationship to command, query, and event contracts

Implementing all three contracts first would be disproportionate. The command model requires current-state authorization, idempotency, optimistic concurrency, and atomic evidence. The query model requires explicit result states, authorization, provenance, consistency, and pagination. The event model requires taxonomy, causality, retention, and consistency relationships. These are coherent foundational capabilities for central authority, but they do not determine whether `src/main.rs` rather than `/host/repo/src/main.rs` is the durable project-scoped summary identity.

Implementing nominal envelopes without their required semantics would falsely claim architectural compliance and create later churn.

### Delay and absolute-path drift

This option leaves the demonstrated daemon write path active while the larger runtime is built. More absolute records can accumulate, and direct MCP relative writes can continue creating competing identities. The eventual runtime would still need the same canonical identity operation before it could validate file-summary commands.

### Rollback boundary

A runtime predecessor has a wider rollback surface: daemon startup and availability, IPC, MCP compatibility, database ownership, status truth, and mutation routing. Rolling it back may require maintaining dual paths, which temporarily preserves the competing authority the milestone is intended to remove.

### Testability

The runtime is testable, but not with the same bounded proof. It requires contract conformance, duplicate requests, concurrency conflicts, authentication and authorization, process restart, IPC failure, adapter parity, single-writer enforcement, and atomic state/evidence tests. That is a separate subsystem acceptance suite rather than prerequisite coverage for file identity.

### Scope size

A useful implementation exceeds a small single-purpose predecessor and approaches PRD-TBD-M2-01. Adding audit completeness would expand it further. A smaller version has no independently useful operating state and fails the backlog's merge-boundary rule against scaffolding-only work.

### Roadmap and backlog consistency

Putting this predecessor before M1 contradicts the roadmap's explicit independence of M1 and M2 and changes the delivery backlog without repository evidence that the ordering is invalid. It would also make PRD-TBD-M1-01 depend on later-milestone infrastructure even though M1's documented completion boundary is intentionally compatible with the existing daemon, MCP tools, and SQLite records.

## Comparative Assessment

| Criterion | Option A | Option B |
|---|---|---|
| Required existing dependencies | Present and bounded | Requires M2 decisions and potentially later audit capabilities |
| Architectural churn | Low if identity remains protocol-neutral | High across daemon, MCP, storage, IPC, and authority |
| Existing storage reuse | Direct reuse | Either duplicates storage or expands into M2 |
| Stops absolute-path drift | Immediately | Only after a larger predecessor and the deferred identity work |
| Rollback | Code-path rollback; no data rewrite | Multi-boundary runtime and adapter rollback |
| Independent deterministic testing | Strong and local | Strong eventually, but substantially broader and process-oriented |
| Single-PR scope | Bounded, though cross-cutting | Unlikely if semantically complete |
| Roadmap alignment | Direct | Reorders explicitly independent milestones |
| Backlog alignment | Direct | Recreates or splits PRD-TBD-M2-01 |
| Future contract compatibility | Preserved by a protocol-neutral domain boundary | Native only if the full runtime is implemented correctly |

## Sequencing Conditions

Keeping the current order is safe only if implementation preserves these boundaries already stated by PRD-001:

1. Canonical path derivation and validation are shared repository-domain behavior, not adapter-specific policy.
2. Storage enforcement applies only to the file-summary identity invariant and does not claim to be the future Command Layer.
3. Existing direct MCP mutation is treated as transitional and receives no new general authority.
4. No destructive legacy reconciliation is folded into the identity-boundary change.
5. The implementation remains callable from the future daemon-owned command and query services without changing path semantics.
6. No event-sourcing, approval, warrant, or generic workflow mechanism is introduced.

These are implementation constraints within PRD-001, not evidence for a predecessor.

## Recommendation

Keep PRD-001 as written
