# ADR-001: Canonical State and Event Semantics

## Status

Accepted

## Date

2026-08-02

## Decision

Familiar will use **transactional canonical state with first-class material-action audit events**.

Transactional SQLite tables are authoritative for current Familiar operational state. They hold the current project, task, decision, finding, verification, approval, warrant, execution, review, handoff, acceptance, and related operational records defined by Familiar's domain.

Git and repository source remain authoritative for source-code and repository state. Familiar's operational database, audit history, summaries, indexes, and other derived records do not replace source or Git history.

Append-oriented audit events are authoritative evidence that defined material actions occurred, were denied, failed, were interrupted, or remained ambiguous. Audit events answer what action was requested or observed, by whom, under what authority, against which state, and with what outcome. They do not replace canonical current-state tables.

Every governed state-changing command must update canonical state and append its audit event atomically in one SQLite transaction. A command may not report successful state mutation unless both the canonical mutation and required audit append have committed.

All mutations must pass through daemon-owned command handlers. The central daemon is the sole mutable authority for Familiar operational state.

MCP, CLI, HTTP, socket clients, dashboard, tray, plugins, maintenance tools, and background workers may not directly mutate canonical storage. They submit commands to daemon-owned handlers or issue read-only queries through shared core services.

Mutation-capable repository APIs must not be publicly accessible outside the command boundary. Read-only query APIs may be shared, but mutable persistence operations are internal capabilities of governed command handling, migration, repair, or reconciliation paths.

Commands require stable request IDs and defined idempotency semantics. Canonical records require version fields wherever concurrent mutation or stale-command rejection is possible.

Audit events require, where applicable:

- actor identity
- originating interface
- project identity
- task identity
- command type
- request ID
- prior canonical version
- resulting canonical version
- governing authority, including approval and warrant references
- outcome
- timestamp
- causal identifiers linking related commands, attempts, evidence, and effects

Denied, failed, interrupted, and ambiguous material actions must also produce evidence records even when no canonical domain mutation succeeds. These records must distinguish request, authorization, intent, attempt, observation, completion, compensation, and unresolved ambiguity rather than collapsing them into a generic failure.

Legacy records must be explicitly marked as predating the audited-history boundary. Familiar must not fabricate historical events for actions it did not observe.

Audit events are not a general event-sourcing stream and are not required to reconstruct all current state. Transactional tables remain authoritative for current state. Audit history is authoritative evidence for the defined material actions it records.

Full event sourcing, projections, replay-based authority, snapshots as operational authority, and event upcasters used to rebuild current state are rejected as disproportionate to Familiar's requirements. This decision does not prohibit disposable derived views or caches, but they are not canonical projections from an authoritative event stream.

External effects use durable intent, attempt, observed-outcome, and compensation records because SQLite cannot atomically commit external reality. A database record of intent or attempt is never proof that an external effect occurred.

Repair, migration, reconciliation, and administrative mutation paths must be governed and audited. Exceptional repair cannot silently rewrite operational state or pretend that missing historical evidence existed.

A deterministic consistency checker must detect missing, duplicate, or impossible state/audit relationships within the guarantees defined by this ADR.

The daemon must enforce single-writer ownership of canonical storage. Process-local convention, a PID file alone, or client cooperation is insufficient.

Restores must treat canonical state, audit history, and referenced evidence as one consistency boundary. A restore must not knowingly combine current state, audit events, or evidence artifacts from incompatible recovery points.

Audit retention may not remove evidence still referenced by canonical decisions, findings, verification results, approvals, warrants, handoffs, or acceptance records.

## Rationale

Familiar needs two different forms of truth without confusing them:

1. The current operational state needed for deterministic policy, task coordination, approvals, warrant validation, verification, review, and handoff.
2. Durable evidence explaining which material actions occurred and how current state was reached.

Transactional relational state fits Familiar's current SQLite foundation, current query model, local-first deployment, and need for direct, deterministic current-state reads. It also keeps operational repair and schema evolution understandable without requiring replay infrastructure.

Supporting audit alone is insufficient because Familiar's target role includes unattended execution, explicit human gates, independent verification, durable findings, and acceptance decisions. A successful operational mutation without attributable evidence would violate the philosophy's requirements for project history, explicit decisions, human approval, reproducibility, and verifiable success.

Making material-action audit append atomic with governed state mutation turns evidence from optional diagnostics into a core command invariant. The command boundary becomes the unit of authority, transactionality, idempotency, and audit coverage.

Audit events remain evidence rather than replay authority. Familiar does not require historical reconstruction of every operational table from an event stream. Avoiding that requirement avoids permanent aggregate, projection, checkpoint, snapshot, replay, and upcaster machinery while preserving the evidence needed for stewardship.

The adversarial review identified the main weakness of this model: it can degrade into ordinary tables with incomplete audit if "material action" remains subjective or mutation paths bypass command handlers. This decision therefore makes material-action classification, mutation encapsulation, consistency checking, single-writer ownership, and audit coverage binding architectural obligations.

## Rejected Alternatives

### Option A — Conventional Transactional Tables Plus Supporting Audit Events

Rejected because supporting audit permits operational state to remain structurally valid without complete evidence for defined material actions. Audit completeness and atomicity would remain optional or dependent on distributed call-site discipline.

Strengthening Option A so that every governed mutation atomically emits required attributable evidence would give it the defining invariant accepted in this ADR. The weaker form does not satisfy Familiar's stewardship and auditability requirements.

### Option B — Full Event Sourcing with Projections

Rejected as disproportionate to Familiar's requirements and current architecture.

Familiar does not require replay-based authority or reconstruction of all current operational state from domain events. Full event sourcing would introduce permanent event contracts, aggregate boundaries, stream versioning, projection infrastructure, checkpointing, replay, snapshots, upcasters, rebuild tooling, projection-consistency rules, and a substantially more complex migration from the existing row-oriented SQLite schema.

Full event sourcing would not eliminate the need for separate evidence about denied commands, reads where auditing is required, process attempts, ambiguous external effects, tool output, or failures that do not mutate aggregate state. It would therefore add maximum persistence complexity without automatically delivering complete operational auditability.

## Architectural Invariants

The following invariants are binding on all downstream architecture and implementation.

1. **Repository authority:** Git and repository source are authoritative for source-code and repository state.
2. **Operational authority:** Transactional SQLite tables are authoritative for current Familiar operational state.
3. **Evidence authority:** Append-oriented audit events are authoritative evidence for defined material actions.
4. **No replay authority:** Audit events are not required or permitted to become the general replay source for canonical current state without superseding this ADR.
5. **Atomic governed mutation:** Every governed state-changing command updates canonical state and appends its required audit event in one SQLite transaction.
6. **No partial success:** A command cannot report successful mutation if canonical state or required audit evidence failed to commit.
7. **Daemon ownership:** Only daemon-owned command handlers may cause canonical mutation.
8. **Thin clients:** MCP, CLI, HTTP, socket, dashboard, tray, plugins, maintenance tools, and background workers have no direct canonical write authority.
9. **Encapsulated repositories:** Mutation-capable repository APIs are inaccessible outside governed command, migration, repair, and reconciliation boundaries.
10. **Single writer:** The daemon enforces exclusive writer ownership of canonical storage.
11. **Stable requests:** Every command has a stable request ID appropriate to its retry horizon.
12. **Idempotent outcomes:** Retrying the same request cannot repeat a logical state transition or external effect.
13. **Versioned concurrency:** Mutable canonical records carry versions wherever stale or concurrent commands are possible.
14. **Attributable evidence:** Audit events contain the actor, interface, scope, command, request, versions, authority, outcome, time, and causal references applicable to the action.
15. **Failure visibility:** Denied, failed, interrupted, and ambiguous material actions remain durable evidence.
16. **Legacy honesty:** Records predating the audit boundary are explicitly identified; no false historical events are synthesized.
17. **External-effect honesty:** Intent, attempt, observation, completion, ambiguity, and compensation are distinct states.
18. **Governed administration:** Migration, repair, reconciliation, and administrative changes are governed and audited.
19. **Detectable inconsistency:** Missing, duplicate, or impossible state/audit relationships are detectable deterministically.
20. **Consistent restore:** Canonical tables, audit events, idempotency state, and referenced evidence are restored from a compatible consistency boundary.
21. **Reference-safe retention:** Referenced audit evidence cannot be removed while a canonical record depends on it.
22. **No silent degradation:** Audit persistence failure blocks governed mutation rather than silently reducing evidence quality.
23. **No timestamp-only causality:** Ordering and causality use durable identifiers and versions; wall-clock time alone is insufficient.
24. **No direct repair fiction:** A repair may correct state but cannot rewrite history to imply the original action was valid or observed.

## Material-Action Definition Rules

A material action is an action whose occurrence, denial, failure, interruption, or ambiguity can affect authority, canonical state, repository/worktree state, external reality, engineering evidence, or a human's acceptance decision.

### Actions that are always material

- Creation, mutation, transition, supersession, archival, or deletion of canonical project, task, decision, finding, verification, handoff, approval, warrant, review, acceptance, execution, worktree, or external-effect state.
- Grant, denial, revocation, expiration, consumption, attempted expansion, or validation failure of approval or warrant authority.
- Start, checkpoint, cancellation, interruption, timeout, crash, recovery, or completion of supervised execution.
- Invocation and outcome of deterministic verification required by task or policy.
- Creation, disposition, acceptance, deferral, rejection, or risk acceptance of review findings.
- Acceptance, rejection, return-for-revision, closure, or rollback of a task.
- Creation, mutation, or retirement of an isolated worktree governed by Familiar.
- Intent, attempt, observed outcome, ambiguity, compensation, or confirmed rollback for any external effect.
- Use of credentials or network authority for a governed effect where disclosure policy or warrant scope applies.
- Migration, repair, reconciliation, restore, administrative mutation, or retention action affecting canonical state or referenced evidence.
- A consistency-check finding that indicates missing, duplicate, impossible, or contradictory state/audit relationships.
- Any attempted command rejected because of authentication, authorization, approval, warrant, version, scope, invariant, or policy failure.

### Conditionally material actions

Read operations are material when they:

- disclose secrets, private source, cross-project data, or remote-provider data;
- inspect approval-, warrant-, security-, or acceptance-sensitive evidence;
- are explicitly required by policy to be attributable; or
- contribute evidence used for a human or deterministic acceptance decision.

Background indexing, cache maintenance, health checks, and routine observations are material only when they mutate canonical state, invalidate evidence used by canonical decisions, cross a trust boundary, or produce a failure/ambiguity that affects workflow correctness.

### Actions that are not automatically material

- High-volume diagnostic telemetry with no canonical, authority, evidence, privacy, or external-effect consequence.
- Reconstructible cache updates that do not change canonical state or the validity of referenced evidence.
- Presentation-only rendering and local UI state.

Non-material classification does not authorize silent canonical mutation. The material-action taxonomy must be explicit, versioned, reviewed, and tested. New command classes must declare their material-action behavior before they can be accepted into the command boundary.

## Transaction and Idempotency Semantics

### Governed state-changing commands

A governed state-changing command executes within one SQLite transaction that includes:

1. Authentication and authority context resolution that must be durable for the command.
2. Request-ID lookup and idempotency validation.
3. Current canonical record and version reads required by the command.
4. Deterministic policy, precondition, and invariant evaluation.
5. Canonical state mutation using expected prior versions where applicable.
6. Resulting-version assignment.
7. Audit event append with required attribution, authority, outcome, versions, and causal identifiers.
8. Durable idempotency outcome recording.

The command is successful only after the transaction commits. A successful response must not be emitted before commit.

### Stable request IDs

Each command carries a stable request ID generated at or before the first submission. Retries of the same logical command reuse that request ID.

Idempotency identity must be scoped so requests cannot collide across actors, interfaces, projects, or command types. The stored request record must bind the request ID to a canonical fingerprint of the command's semantically relevant input.

If the same request ID and equivalent input are received after commit, the daemon returns the recorded outcome without repeating the mutation. If the same request ID is reused with different semantically relevant input, the daemon rejects it as an idempotency conflict and records evidence where the attempted reuse is material.

The retention horizon for idempotency state must be at least as long as the supported retry and ambiguity horizon for the command. Commands capable of external effects may require indefinite or effect-lifetime idempotency records.

### Record versions and concurrency

Canonical records that can receive concurrent or stale mutation must carry monotonically changing versions. Commands declare expected prior versions when correctness depends on the state observed by the caller or policy engine.

A version conflict does not silently overwrite state. It produces a deterministic conflict outcome and material evidence when the attempted mutation is governed.

Multi-record invariants must be evaluated and mutated in one SQLite transaction when they are wholly internal to canonical state. The daemon may serialize command classes where optimistic conflict handling is insufficient, but serialization must not replace record-level integrity checks.

### Audit ordering and causality

Audit events carry an append order representing SQLite commit order. Commit order is not treated as complete causal order.

Causal identifiers must link related request, command, approval, warrant, task, attempt, verification, finding, review, handoff, acceptance, external-effect, compensation, and repair records. Timestamps provide observation time but do not replace these relationships.

### Denied and failed commands

Commands rejected before canonical mutation because of authentication, authorization, policy, warrant, approval, version, scope, or invariant failure produce evidence records when classified as material. Since no canonical mutation succeeds, the evidence append is committed in its own governed SQLite transaction.

Serialization, persistence, or audit failure may prevent creation of the evidence it was attempting to record. Such a failure must be surfaced as a daemon health and operational-integrity failure; it cannot be represented as an audited denial if no record committed.

## External-Effect Recovery Model

SQLite cannot atomically commit a database transaction and external filesystem, Git, process, network, provider, publication, or deployment reality. External effects therefore use a durable state machine.

### Required stages

1. **Intent:** Canonical state records the exact authorized effect, target, authority, request ID, expected preconditions, idempotency mechanism, and compensation plan where one exists. Intent commit does not mean the effect occurred.
2. **Attempt:** Before or at the controlled boundary of execution, Familiar records the attempt identity, start conditions, and actor/process responsible. Attempt does not mean success.
3. **Observed outcome:** Familiar records deterministically observed evidence of success, failure, timeout, cancellation, or ambiguity. Local return status alone is insufficient when the external system may have completed despite an error.
4. **Reconciliation:** Ambiguous outcomes remain explicitly unresolved until an authoritative observation or human disposition resolves them. Automatic retry is permitted only when idempotency and warrant authority make it safe.
5. **Compensation:** If reversal is authorized and possible, Familiar records compensation intent, attempt, observed outcome, and remaining divergence. Compensation does not erase the original effect.

### Recovery after crash or restart

On restart, the daemon identifies intents without attempts, attempts without terminal observed outcomes, ambiguous outcomes, and incomplete compensation. It does not infer success from intent, attempt, or elapsed time.

The daemon reconciles against authoritative local or remote state where possible. When authoritative observation is unavailable, the state remains ambiguous and unattended progress stops at the applicable policy gate.

An external-effect retry must reuse or derive from the original stable idempotency identity and must still be authorized by a valid warrant. Authority consumption, expiration, or revocation rules from the warrant ADR apply independently of the technical ability to retry.

Database rollback, event deletion, projection of prior state, or restoration of an old backup never constitutes rollback of external reality.

## Historical Boundary and Migration Constraints

The introduction of audited command semantics creates an explicit historical boundary.

### Legacy records

Existing projects, file summaries, decisions, and session rollups may be migrated into new canonical schemas, but they must carry an explicit legacy or pre-audit marker. Their current values may be preserved as baseline state.

Familiar must not generate synthetic audit events implying that it observed the creation, approval, mutation, or rationale of legacy records. A migration record may state that legacy baseline state was imported at a particular time, but that record describes the migration action, not the historical actions that originally produced the data.

### Migration authority

Schema and data migrations are daemon-governed or otherwise run under an explicitly governed administrative boundary. Migration identity, version, start, completion, failure, and reconciliation results are material evidence.

Migrations that mutate canonical state must preserve transactionality appropriate to SQLite and must not leave a database that appears fully audited when only part of the data crossed the boundary.

### Mixed-version operation

The system must not allow old mutation paths to remain active after a record or command class is declared governed by audited command semantics. Direct MCP writers, legacy daemon repositories, maintenance tools, or older processes must be excluded from canonical write ownership before the audit boundary is considered active for that scope.

Rollback to an older application version must not allow that version to write canonical state it cannot audit or whose audit event versions it cannot safely preserve. If safe write compatibility is absent, rollback is read-only, fail-closed, or requires restoration of the entire consistency boundary.

### No inferred history

Reconciliation may detect missing or contradictory evidence, but it may not invent a prior actor, approval, warrant, request, outcome, or rationale. Unknown historical facts remain explicitly unknown.

## Consequences

### Positive consequences

- Current operational queries remain direct and deterministic.
- Existing SQLite and relational repository concepts remain viable.
- Material state changes and their evidence share an atomic commit boundary.
- Thin clients have one command authority and cannot create competing state.
- Human approvals, warrants, verification, findings, and acceptance can be attributed without requiring replay infrastructure.
- Daemon restart normally loads current state directly rather than replaying history.
- Legacy data can be preserved without fabricating causal history.
- Audit coverage becomes a testable command invariant rather than optional diagnostic logging.
- External-effect ambiguity is modeled honestly rather than hidden by transaction language.

### Negative consequences

- Every material mutation incurs audit write and index overhead.
- Audit-storage failure blocks governed state mutation.
- The material-action taxonomy becomes a long-lived architectural contract requiring review and evolution.
- Current state and historical evidence use linked but different representations, increasing debugging and tooling requirements.
- Exact time-travel reconstruction of all current state is not provided.
- Audit payload schemas require durable compatibility even though they are not replay events.
- Long-lived audit history increases backup, restore, integrity-check, retention, and redaction complexity.
- Strict daemon ownership requires governed replacements for direct SQL maintenance and background mutation.
- Single-writer ownership and SQLite write serialization can become performance constraints under materially higher workloads.
- External effects still require reconciliation and can remain permanently ambiguous or irreversible.

### Costly-to-reverse consequences

- Consumers may depend on material-action event meanings and causal relationships.
- Audit-era boundaries and legacy markers become permanent historical facts.
- Record-version and request-id semantics become part of command compatibility.
- Moving later to full event sourcing would require defining replay semantics not present in these audit events.
- Weakening audit requirements later would create a visible and potentially unacceptable evidence gap.

## Downstream Impact

### M2 — Central Core Authority and Shared Interfaces

- Core contracts must distinguish commands from queries.
- Local IPC must carry stable request, actor, interface, authority, and causal context.
- MCP mutation must move behind daemon commands before audited authority is declared active.
- Dashboard, tray, CLI, HTTP, socket clients, and plugins remain thin and read-only with respect to storage.
- Authoritative status must expose single-writer, audit integrity, consistency-check, and recovery health.

Affected provisional PRDs:

- `PRD-TBD-M2-01`
- `PRD-TBD-M2-02`
- `PRD-TBD-M2-03`
- `PRD-TBD-M2-04`

### M3 — Responsive Control Plane and Host Trust Baseline

- SQLite write ownership and command transactions must remain off blocking async control paths.
- The host capability model must include enforceable single-writer ownership, lock health, backup/restore consistency, and administrative access constraints.
- Credential and client identity feed audit attribution without granting clients authority.

Affected provisional PRDs:

- `PRD-TBD-M3-01`
- `PRD-TBD-M3-02`
- `PRD-TBD-M3-03`

### M4 — Canonical Stewardship State, Policy, Audit, and Memory

- Canonical domain records require versioning and legacy-boundary semantics.
- Audit events use a common envelope and material-action taxonomy.
- Approval, warrant, policy, handoff, and decision mutations occur only as governed commands.
- The consistency checker becomes part of canonical integrity.

Affected provisional PRDs:

- `PRD-TBD-M4-01` through `PRD-TBD-M4-06`

### M5 and M6 — Repository Intelligence and Context

- Canonical intelligence mutations and invalidations follow governed command semantics when material.
- Reconstructible cache writes remain non-canonical unless referenced by canonical evidence.
- Context manifests identify canonical record versions and causal evidence without treating audit as a replay stream.

Affected provisional PRDs:

- `PRD-TBD-M5-01` through `PRD-TBD-M5-03`
- `PRD-TBD-M6-01` through `PRD-TBD-M6-03`

### M7 — Deterministic Verification

- Verification intent, attempt, result, failure, interruption, and ambiguity are material.
- Large logs and artifacts may be referenced rather than embedded, but retention must honor canonical references.
- Required-check outcomes and policy evaluation carry request, version, and causal identities.

Affected provisional PRDs:

- `PRD-TBD-M7-01`
- `PRD-TBD-M7-02`

### M8 — Bounded Isolated Execution

- Worktree, warrant, attempt, checkpoint, cancellation, effect, compensation, and terminal-handoff transitions use governed commands.
- External effects follow the intent/attempt/observed-outcome/compensation model.
- Lost responses and retries use stable idempotency semantics.

Affected provisional PRDs:

- `PRD-TBD-M8-01` through `PRD-TBD-M8-04`

### M9 — Independent Review and Acceptance

- Reviewer assignment, finding creation/disposition, waiver, acceptance, rejection, revision, and rollback are material actions.
- Acceptance reports reference canonical versions and retained evidence.

Affected provisional PRDs:

- `PRD-TBD-M9-01`
- `PRD-TBD-M9-02`
- `PRD-TBD-M9-03`

### M10 and M11 — Optional Enhancements

- Model, semantic, adapter, and reviewer actions inherit the same command, attribution, privacy, evidence, and retention rules when material.
- Optional derived artifacts do not become canonical projections or replay authority.

## Verification Requirements

Conformance with this ADR requires deterministic evidence in the following areas.

### Command-boundary enforcement

- Static/API-boundary checks prove mutation-capable repository operations are not publicly available to clients, presentation layers, plugins, or background workers.
- Integration tests prove all supported mutation interfaces reach daemon-owned command handlers.
- Negative tests prove direct storage mutation is unavailable through MCP, CLI, HTTP, socket, dashboard, tray, plugin, and worker boundaries.

### Atomicity

- Failure-injection tests at each mutation/audit step prove state and required audit event commit together or neither commits.
- Tests prove a successful response is not emitted before commit.
- Database-integrity tests verify canonical versions and audit prior/result versions agree.

### Idempotency and concurrency

- Same-request/same-input retries return the recorded outcome without repeating mutation.
- Same-request/different-input reuse is rejected and evidenced.
- Concurrent expected-version conflicts cannot silently overwrite state.
- Lost-response and daemon-restart tests preserve idempotent outcomes.
- Idempotency retention tests cover the declared retry horizon.

### Audit completeness and attribution

- Every governed command class has explicit material-action coverage tests.
- Required audit fields are structurally and semantically validated.
- Denied, failed, interrupted, ambiguous, repair, migration, and reconciliation scenarios produce the required evidence.
- Redaction tests prove secrets do not enter audit payloads while required authority and causality remain usable.

### Single-writer ownership

- Process-level tests prove a second daemon or writer cannot acquire canonical write ownership.
- Crash and stale-lock tests prove ownership can be recovered without admitting concurrent writers.
- Backup, integrity-check, and read-only diagnostic access cannot accidentally obtain mutation authority.

### Consistency checking

- The deterministic checker detects missing audit relationships, duplicate request outcomes, impossible version transitions, orphaned evidence references, contradictory terminal outcomes, and invalid authority references.
- Checker output distinguishes corruption, legacy/pre-audit state, incomplete external effects, and unsupported event versions.
- The checker never fabricates evidence to repair a gap.

### External-effect recovery

- Crash-matrix tests cover every boundary among intent commit, attempt start, effect execution, response loss, outcome observation, compensation, and daemon restart.
- Ambiguous effects remain unresolved until authoritative observation or human disposition.
- Retry tests prove effect idempotency and continuing warrant authority.
- Compensation tests prove that compensation evidence does not erase original effect evidence.

### Migration and historical boundary

- Representative legacy databases migrate without fabricated audit history.
- Legacy records remain explicitly identifiable.
- Mixed-version tests prove older mutation paths cannot write after governed cutover.
- Rollback tests prove older binaries fail closed or operate read-only when they cannot preserve current audit semantics.

### Restore and retention

- Backup/restore tests treat canonical tables, audit history, idempotency state, and referenced evidence as one consistency boundary.
- Incompatible restore points are detected.
- Retention tests prevent deletion of evidence referenced by canonical decisions, findings, verification, approvals, warrants, handoffs, or acceptance.
- Integrity checks remain valid after approved archival or redaction.

### Performance and operability

- SQLite measurements cover material-action throughput, transaction latency, WAL growth, checkpoint behavior, backup/restore duration, integrity-check duration, and long-history query cost.
- Operational diagnostics expose audit persistence health, ownership health, consistency-check status, unresolved external effects, and restoration boundaries.
- Evidence volume and index design remain bounded by approved retention policy.

## Conditions That Would Justify Superseding This ADR

This ADR may be reconsidered only with explicit human architectural approval and evidence that one or more foundational assumptions no longer hold. Relevant conditions include:

- Exact deterministic reconstruction of canonical operational state from complete history becomes a mandatory product requirement rather than a diagnostic convenience.
- Temporal queries, branching workflow state, or replicated/offline state require event authority that material-action audit events cannot safely provide.
- Measured SQLite single-writer, audit-volume, retention, or recovery limits cannot satisfy required workloads despite bounded schema and indexing improvements.
- The daemon is no longer the single local mutable authority and a distributed consistency model becomes necessary.
- A new persistence engine provides materially stronger required guarantees, and its operational cost is justified by evidence.
- The material-action taxonomy proves impossible to define or mechanically enforce without unacceptable blind spots.
- Dual current-state and historical-evidence semantics repeatedly produce contradictions that deterministic consistency checking and governed repair cannot resolve safely.
- Audit compatibility, redaction, or retention requirements make append-oriented evidence legally or operationally untenable.
- External-effect coordination requirements change so substantially that the accepted intent/attempt/observation/compensation model is inadequate.
- Operational evidence demonstrates that full replay, projections, snapshots, and event evolution would reduce total system complexity rather than increase it.

Supersession must preserve the existing historical boundary, must not reinterpret audit events as replayable domain events without proof that their semantics support it, and must include an explicit migration and rollback decision approved before implementation.
