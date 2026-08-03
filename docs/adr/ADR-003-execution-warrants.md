# ADR-003: Execution Warrants

**Status:** Proposed — Decision Preparation  
**Date:** 2026-08-02

## Purpose

This document prepares the architectural decision for Familiar's unattended-execution contract. It evaluates alternative warrant models without selecting or recommending one.

The decision is not whether unattended execution is unconstrained. Familiar's philosophy and accepted architecture already require bounded work, explicit human approval gates, daemon-owned authorization, deterministic verification, auditability, rollback, and a safe stopping point. ADR-001 establishes transactional canonical state and material-action evidence. ADR-002 establishes that approval is distinct from execution and that execution authority is conveyed through bounded, revocable, capability-like warrants derived from approvals.

The unresolved decision is how much execution scope and lifecycle behavior the warrant itself must represent, and which guarantees belong in the canonical warrant model rather than only in the execution supervisor, policy engine, task, or approval records.

## Governing Constraints

The following constraints are fixed inputs to this decision:

- Humans retain architectural and risk-gate authority; AI agents cannot approve their own work or expand their authority.
- Work is explicit, bounded, measurable, verifiable, attributable, and stoppable.
- The repository and Git history remain authoritative for source-code state.
- Transactional SQLite records are authoritative for current Familiar operational state.
- Defined material actions emit append-oriented audit evidence under ADR-001.
- Every governed mutation passes through daemon-owned command handlers.
- The daemon is the single writer of canonical state.
- Approval records human consent; it does not directly authorize execution.
- Current canonical state determines authorization.
- A warrant is derived from effective approval and policy where human approval is required.
- A warrant cannot broaden its source approval, delegation, policy, task, or project scope.
- Typed principals and stable opaque IDs govern actor and executor binding.
- Agents, editors, models, and protocol adapters remain replaceable.
- Implementation work occurs in an isolated worktree or an equivalently isolated repository environment.
- Deterministic verification and independent review remain distinct from implementation.
- External effects use durable intent, attempt, observed-outcome, and compensation evidence because SQLite cannot atomically commit external reality.
- Local-first operation is mandatory; future team and distributed operation must remain possible without changing canonical semantics.
- Recovery, reconciliation, administrative action, and rollback are governed and auditable.

## Terms and Conceptual Boundaries

### What a warrant is

A warrant is a canonical, daemon-issued grant of bounded execution authority. It connects an authorized task and its effective approvals to a specific executor, execution environment, scope, constraints, validity boundary, required evidence, and permitted effects.

A warrant answers: **what may this principal cause Familiar to execute, against which project state, in which environment, under what limits, until which boundary, and subject to which checks?**

A warrant is capability-like because it conveys restricted authority and may be attenuated, consumed, expired, suspended, superseded, or revoked. Capability-like does not necessarily mean a portable bearer token. Possession of a warrant identifier is never sufficient by itself; daemon authorization must validate current canonical state.

### What a warrant is not

A warrant is not:

- human approval;
- proof that an approval remains effective;
- a prompt or agent instruction;
- a task specification or acceptance criterion;
- a shell script or arbitrary command bundle;
- a standing role or general service-account permission;
- a replacement for operating-system sandboxing or worktree isolation;
- a guarantee that commands, models, or tools will behave safely;
- a declaration that work is correct or complete;
- a substitute for deterministic verification or independent review;
- a commit, merge, publication, deployment, or external-effect record;
- a general-purpose workflow engine;
- an event-sourcing stream; or
- authority to reinterpret or expand its own scope.

### Approval, authorization, warrant, and execution

These remain distinct:

1. **Approval** records an eligible human's explicit decision about an immutable subject.
2. **Authorization** evaluates whether the requested operation is allowed by current canonical identity, approval, delegation, policy, task, and project state.
3. **Warrant issuance** materializes a bounded execution grant from that authorization decision.
4. **Execution** exercises the warrant through the daemon-owned supervisor.
5. **Verification** determines, through retained evidence, whether execution satisfied required conditions.
6. **Acceptance** is a separate governed conclusion and may require additional human approval.

## Common Warrant Content

The alternatives differ in lifecycle depth, but any object called a warrant must be able to bind or reference:

- stable warrant ID and schema version;
- project, task, objective, and immutable subject;
- base Git revision and authorized worktree or environment identity;
- source approvals, delegations, policy evaluation, and authorization decision;
- executing principal, daemon identity, daemon writer epoch, and permitted interfaces;
- allowed and prohibited operations;
- command, filesystem, network, tool, model, and external-effect scope;
- resource, time, concurrency, attempt, and use ceilings;
- required checkpoints, verification gates, stop conditions, and evidence;
- issue, activation, expiration, revocation, supersession, and completion data;
- request IDs, causal IDs, prior and resulting versions, and audit references; and
- rollback, compensation, handoff, and recovery expectations.

References may point to immutable, content-addressed policy or scope manifests rather than duplicate their contents. Indirection cannot permit the effective scope to change after issuance.

## Warrant Lifecycle Concepts

This section defines concepts that every option must address. It does not prescribe which concepts must be first-class states rather than derived facts.

### Warrant lifecycle

The conceptual lifecycle begins with a request for execution authority, continues through authorization and issuance, governs any attempt to exercise authority, and ends with a terminal or invalid state plus retained evidence. The lifecycle must preserve the distinction between authority being available, authority being exercised, effects being observed, and work being accepted.

### Warrant states

Candidate semantic states include:

- **Requested:** A principal has requested bounded execution authority.
- **Issued:** The daemon has created the immutable authority envelope following current authorization.
- **Active:** The warrant is eligible for execution in its bound environment.
- **Running:** An execution attempt currently holds a valid lease or ownership claim.
- **Checkpoint-pending:** Further execution requires checkpoint evaluation.
- **Suspended:** New governed actions are disallowed pending an explicit continuation decision.
- **Completed:** The warrant's declared work and terminal verification conditions were recorded.
- **Consumed:** Its authorized uses or attempts are exhausted.
- **Expired:** Its temporal or state-bound validity ended.
- **Revoked:** Authority was explicitly withdrawn.
- **Superseded:** A successor warrant replaced it without rewriting history.
- **Failed:** An attempt reached a defined non-recoverable or retry-gated failure boundary.
- **Cancelled:** Work was intentionally ended without satisfying completion.
- **Ambiguous:** Recovery cannot yet determine whether a governed effect occurred.

An option may store fewer states and derive others. It must nevertheless make every operational condition unambiguous and reject impossible combinations.

### Warrant revocation

Revocation is an authorized canonical transition that withdraws future authority. It must be effective at daemon-governed action boundaries and auditable even if no execution has begun. Policy must define its effect on running processes, pending tool calls, external intents, irreversible effects, checkpoint evaluation, and compensation.

Revocation cannot erase prior authorized attempts or observed effects. A revoked warrant cannot be reactivated; renewed authority requires a successor warrant or another explicit construct chosen by this ADR.

### Warrant expiration

Expiration ends authority when a declared time, revision, task state, approval state, daemon epoch, use count, resource budget, or other validity boundary is crossed. Expiration is fail-closed. Wall-clock time alone is insufficient for ordering or replay protection, so version, sequence, and state boundaries must accompany time where relevant.

Expiration during a command or external effect requires a defined boundary rule: validate only before starting, validate throughout interruptible work, or suspend at the next safe checkpoint. The choice may vary by operation class but cannot remain implicit.

### Warrant supersession

Supersession links a new warrant to an older warrant whose remaining authority it replaces. It preserves both records and their evidence. The system must define whether supersession immediately suspends or revokes the predecessor, how in-flight actions settle, and whether unused limits transfer. Authority must never be duplicated through supersession.

### Warrant consumption

Consumption records that some or all granted authority has been exercised or exhausted. It may be per warrant, attempt, command class, external effect, checkpoint interval, budget unit, or single use. Reservation and final consumption may be separate to handle concurrency and crashes.

Consumption is not completion. A warrant may be consumed by failed attempts, and completion may occur with unused authorized capacity. Retries must use explicit remaining authority or a successor grant rather than resetting counters informally.

### Warrant evidence

Warrant evidence is the retained, attributable record of issuance, validation, exercise, denial, suspension, continuation, expiration, revocation, supersession, consumption, completion, and recovery. It includes relevant command manifests, process and environment identities, logs, diffs, resource measurements, verification results, external-effect records, and handoffs.

Evidence must distinguish requested action, authorized action, attempted action, observed outcome, and verified conclusion. Evidence is subject to ADR-001 consistency and retention requirements.

### Warrant checkpoints

A checkpoint is a declared boundary at which the daemon records progress and evidence, reevaluates current authorization and warrant validity, inspects scope and budgets, applies verification gates, and decides whether execution may continue, must suspend, or must terminate.

Checkpoints may occur before or after command groups, model turns, filesystem mutations, network use, external effects, resource thresholds, time intervals, phase transitions, or detected policy conditions. A checkpoint is not merely a log message; it has deterministic inputs, an outcome, and an audit relationship.

### Warrant completion

Completion is a canonical conclusion that the warrant reached its defined execution boundary and produced required terminal evidence. It does not mean the implementation is correct, reviewed, accepted, merged, deployed, or free of findings. Those are separate verification and workflow decisions.

Completion must identify the terminal repository state, diff, attempts, effects, verification evidence, remaining findings, and handoff. Only the daemon may record canonical warrant completion after evaluating its declared conditions.

## Option A: Static Command Allowlists Attached Directly to Approvals

### Model

An approval contains or directly references a fixed allowlist of commands and coarse constraints. The execution supervisor consults the effective approval and permits listed commands while it remains valid. Warrant behavior is minimal: either the approval itself functions as the execution envelope or a thin execution reference points back to it.

Because ADR-002 requires approval and execution authority to remain distinct and states that human approval never directly authorizes execution, the literal form of this option conflicts with an accepted invariant. A conforming variant would require the daemon to derive a distinct execution grant from the attached allowlist. That variant retains the static-allowlist model but is no longer approval-as-authority.

### Authority boundary

The boundary is the approved command list plus current policy. It is easy to identify for exact commands but ambiguous for shell interpreters, command arguments, environment variables, scripts, build tools, plugin systems, and commands that invoke other commands. Authority risks residing partly in mutable scripts or configuration outside the approval subject.

### Command scope

Exact executable-and-argument matching is narrow but brittle. Prefixes, patterns, command categories, or shell strings are more ergonomic but can unintentionally authorize arbitrary behavior. Commands such as `make`, package managers, test runners, interpreters, Git hooks, and compilers have transitive execution surfaces not visible in an allowlist.

### Filesystem scope

An allowlist does not inherently constrain which paths a permitted command reads or writes. Separate sandbox or supervisor policy is required. Encoding paths as command arguments fails when tools resolve configuration, symlinks, worktrees, environment variables, or generated paths dynamically.

### Worktree isolation

Worktree identity can be included in the approval, but enforcement remains external to the allowlist. A permitted command can escape the worktree unless host isolation and path controls enforce the boundary. Worktree replacement or base-revision drift must invalidate the execution reference.

### Network permissions

Network authority is not represented naturally by a command list. A listed command may contact arbitrary endpoints directly or through dependencies, hooks, plugins, subprocesses, or model tools. Separate egress controls and endpoint policy are necessary.

### Tool permissions

Named tools fit the model only when their behavior and extension surfaces are stable. Tool aliases, version changes, plugins, configuration, and indirect invocation can expand authority without changing the allowlist.

### Model restrictions

An approval can name an agent or model, but static command rules do not bind actual model invocation, provider, capability profile, tool exposure, or replacement semantics. Separate principal and adapter checks are required.

### Principal binding

The approval may bind its commands to an executor principal. Doing so mixes the human decision subject with execution assignment and requires reapproval when an interchangeable agent changes unless the subject binds a neutral role. A thin derived reference can bind the actual executor but introduces a warrant-like artifact.

### Daemon ownership

The daemon must be the only evaluator and dispatcher of governed commands. Allowlist enforcement delegated to clients, shells, agents, or plugins would violate the command boundary and permit semantic divergence across interfaces.

### Approval relationship

The relationship is direct and understandable, but it conflates the content a human approves with executable authority. Every operational change to command scope risks requiring a new human approval even when policy could safely attenuate it. Conversely, approval may appear valid while current execution conditions have changed.

### Authorization relationship

Current-state authorization must still reevaluate identity, policy, task, subject, revocation, and environment before every governed action. If the static approval is treated as sufficient authority, the option violates ADR-002. If a separate authorization result is required, the operational model has already added a distinct grant boundary.

### Checkpoint semantics

Static allowlists do not express checkpoints. The supervisor can impose them externally, but their location, evidence requirements, and continuation authority are not part of the approved grant. Different supervisors could interpret identical approval scope differently.

### Suspension and resume

Suspension is typically process-level state rather than warrant state. Resume means rechecking the approval and continuing, which may lose the exact prior boundary, budget use, or reason for suspension. Safe continuation requires separate canonical attempt and progress records.

### Crash recovery

The daemon can reload approval validity, but it must reconstruct running commands, partial filesystem changes, budgets, and effects from separate records. An approval does not distinguish never started, running, completed, failed, or ambiguous execution.

### Replay protection

Stable request IDs and canonical attempt records are required outside the approval. A still-valid allowlist may authorize the same command repeatedly unless explicit use limits and consumption are added.

### Idempotency

Command allowlisting says nothing about command idempotency. The supervisor needs operation-specific idempotency keys, deduplication, and recovery. Retrying a permitted non-idempotent command can duplicate mutations or external effects.

### External effects

Static allowlists are weakest where a permitted command causes remote or irreversible effects. Endpoint, object, amount, audience, and effect class may be hidden in input or configuration. ADR-001 intent, attempt, outcome, and compensation records remain mandatory and require a richer external-effect boundary.

### Verification gates

Required tests can be listed as commands, but the model does not inherently block continuation or completion based on their results. Gate evaluation and evidence retention live outside the authority object.

### Rollback behavior

The approval can name rollback commands, but cannot guarantee they reverse filesystem, Git, process, dependency, or external state. Rollback eligibility, checkpoints, retained base state, and compensation evidence require additional supervisor semantics.

### Auditability

It is straightforward to audit approved command text and actual command text. It is harder to explain transitive behavior, scope consumption, intermediate authorization, why execution resumed, or whether a command's effective behavior changed because scripts or configuration changed.

### Operator ergonomics

Simple tasks are easy to inspect. Complex tasks produce long, brittle allowlists that encourage wildcarding or repeated approvals. Operators may confuse “listed command” with “safe effect” and may be unable to assess transitive command behavior.

### Implementation complexity

Initial implementation complexity is comparatively low. Complexity migrates into command normalization, shell parsing, path and network enforcement, attempt tracking, recovery, checkpoints, and audit logic. Making the model conform to ADR-002 introduces a separate derived grant even if it remains thin.

### Future distributed operation

The model assumes central validation and a shared view of executable identity, scripts, configuration, filesystem, and policy. Portable command strings are not portable authority. Distribution requires signed immutable manifests, audience binding, revocation freshness, and remote enforcement semantics.

### Failure modes and enduring constraints

- A permitted interpreter or build command becomes an authority escape hatch.
- Mutable scripts change after approval without changing the allowlist.
- A valid command acts outside intended filesystem or network scope.
- Crash recovery repeats a non-idempotent command.
- Operators approve broad patterns to avoid excessive prompts.
- A conforming separation between approval and execution adds an implicit warrant whose semantics remain underspecified.

## Option B: Standalone Capability-Style Warrants Derived from Approvals

### Model

The daemon derives a standalone, immutable warrant from effective approvals and current authorization. The warrant describes bounded operations, scope, executor, environment, validity, budgets, and required evidence. It may be an opaque canonical record or a self-contained proof, but daemon validation against current canonical state remains mandatory. Execution attempts consume authority under the warrant.

Lifecycle beyond issued, active, consumed, expired, revoked, and completed may be represented in separate attempt, checkpoint, and workflow records rather than in the warrant state machine itself.

### Authority boundary

The warrant is the explicit authority boundary. It can attenuate approved scope based on policy and execution conditions. Its safety depends on precise capability vocabulary, no implicit ambient authority, and enforcement at every command, tool, filesystem, network, and effect boundary.

### Command scope

Capabilities may authorize exact commands, command classes, tool operations, or immutable command manifests. Structured operations avoid shell-string ambiguity but require adapters for tools. Broad command capabilities can recreate allowlist escape problems.

### Filesystem scope

Path and operation rights can be explicit: read, create, modify, delete, rename, or execute within repository-relative roots. Enforcement requires canonical path resolution, symlink and mount handling, generated-path policy, and host sandbox support.

### Worktree isolation

The warrant can bind a worktree ID, canonical path, repository identity, and base revision. The supervisor rejects execution outside it or after identity drift. Capability scope complements but does not replace OS-level isolation.

### Network permissions

Network capabilities can define deny-all, endpoint, protocol, direction, credential, data class, and effect constraints. Enforcement requires host support and must cover subprocesses and indirect tool access. DNS and endpoint identity changes complicate static binding.

### Tool permissions

Tool capabilities can identify operation schemas and versions rather than only executables. Plugins and transitive tools require explicit inclusion or prohibition. Tool updates that change semantics may invalidate the warrant's immutable manifest.

### Model restrictions

The warrant can bind an AI principal, agent role, provider/model restrictions, tool profile, context manifest, and token or cost budget. Binding too specifically impedes safe agent replacement; binding too broadly allows materially different capabilities under the same authority.

### Principal binding

Strong binding is natural. Proof-of-possession or daemon-mediated opaque handles can prevent bearer theft. Transfer to another agent requires explicit reassignment or successor authority and retained accountability.

### Daemon ownership

The daemon issues, validates, attenuates, consumes, revokes, and records warrants. Clients may present warrant IDs but cannot construct authoritative warrants or interpret scope independently. A self-contained token never bypasses current daemon validation.

### Approval relationship

The warrant references exact effective approvals and can only narrow their subjects and conditions. It preserves ADR-002's consent/authority distinction. Material subject changes or invalid approval dependencies invalidate further exercise.

### Authorization relationship

Issuance is an authorization decision, and exercise requires reauthorization against current canonical state. The warrant records granted maximum scope; current policy may further restrict or deny it. Whether every individual low-level operation requires a full policy evaluation or a validated execution lease remains to be defined.

### Checkpoint semantics

The warrant can declare checkpoint requirements and maximum uninterrupted scope. Checkpoint state and continuation decisions may live in separate canonical records. This separates authority from workflow but risks fragmented recovery or inconsistent lifecycle interpretation.

### Suspension and resume

Suspension can revoke an execution lease while leaving unused warrant authority intact. Resume requires current authorization and a new lease or continuation record. Without a first-class suspended warrant state, operators must understand the combined warrant, attempt, lease, task, and checkpoint records.

### Crash recovery

Canonical warrant, reservations, consumption, attempts, and evidence support recovery. The daemon must fence stale processes using writer epochs or leases and reconcile ambiguous commands and external effects before reissuing authority. Immutable warrants simplify validation but not progress reconstruction.

### Replay protection

Stable request IDs, nonce or audience binding, attempt IDs, use counters, canonical reservations, and atomic consumption prevent reuse. Opaque daemon references reduce token theft but not duplicate requests. Self-contained capabilities increase replay and revocation risks.

### Idempotency

The command boundary can require stable operation keys and atomically reserve authority with canonical intent. Idempotency remains operation-specific. A failed or ambiguous attempt cannot automatically return authority to the available pool.

### External effects

Capabilities can bound effect type, target, credentials, quantity, and attempt count. Each effect still follows ADR-001's intent, attempt, observed-outcome, and compensation model. Ambiguity freezes relevant authority until reconciliation rather than assuming success or failure.

### Verification gates

The warrant can require deterministic checks before particular capabilities become usable or before completion. Gate evaluations may be separate records. The system must prevent an executor from treating its own claims as verification evidence.

### Rollback behavior

The warrant may reserve rollback or compensation capability and require checkpoints. Rollback authority should be explicit because remediation may itself be destructive or externally visible. Revocation policy must decide whether it withdraws all authority or retains emergency containment authority.

### Auditability

The grant, attenuation, validation, reservation, use, denial, and consumption history can be precise. Operator explanation may be difficult when effective authority is computed across the warrant, current policy, approvals, delegations, attempts, and external-effect state.

### Operator ergonomics

A concise rendered capability manifest can communicate exact bounds better than commands alone. Fine-grained capability vocabularies can overwhelm operators and lead to broad grants. Tools must explain both maximum warrant scope and currently exercisable scope.

### Implementation complexity

Complexity is moderate to high: canonical capability schema, policy derivation, enforcement adapters, attenuation, consumption, leases, recovery, and evidence are required. Keeping lifecycle outside the warrant reduces warrant-state complexity but increases cross-record invariants.

### Future distributed operation

Capability-style warrants can support remote executors if issuer, audience, holder binding, validity, revocation freshness, clock, policy version, and evidence return are defined. Local canonical validation is straightforward; independent offline validation would require another consistency and trust decision.

### Failure modes and enduring constraints

- Coarse capabilities recreate ambient authority.
- A bearer-style warrant leaks or is replayed.
- Separate attempt and checkpoint state diverges from warrant validity.
- Revocation races with an executor holding a lease.
- Capability vocabulary becomes a rigid compatibility surface.
- Operators cannot understand effective authority assembled from multiple records.

## Option C: State-Machine Execution Warrants

### Model

The warrant is a canonical state machine that combines a bounded capability envelope with explicit lifecycle transitions for issuance, activation, execution, checkpointing, suspension, continuation, expiration, revocation, supersession, consumption, failure, and completion. Attempts and external-effect records remain distinct evidence but are constrained by warrant transitions.

The state machine does not perform work itself. Daemon-owned command handlers validate transitions and the execution supervisor acts only under the current warrant state.

### Authority boundary

Authority is determined jointly by the immutable scope envelope and current lifecycle state. This makes availability of authority explicit but risks placing workflow responsibilities into the warrant model. Transition guards must prevent lifecycle state from broadening scope.

### Command scope

The immutable envelope can use structured command and operation capabilities, while states determine when each class is exercisable. Phase-specific scope can be modeled as predeclared subsets; mutable expansion requires a successor warrant rather than a state transition.

### Filesystem scope

Repository-relative rights, worktree identity, operation classes, and prohibited paths are part of the envelope. Checkpoints can validate observed diffs and suspend on scope breach. Host enforcement remains necessary between checkpoints.

### Worktree isolation

The warrant binds worktree, repository, and base revision throughout its lifecycle. Activation can require clean isolation evidence; checkpoints can verify containment; completion can require final diff evidence. A lost or corrupted worktree leads to suspension, failure, or cancellation rather than implicit recreation under the same authority unless policy defines recovery.

### Network permissions

Network authority can be phase- and state-sensitive, such as dependency retrieval before execution or publication only after verification. This is expressive but increases transition and enforcement complexity. Subprocess egress still requires host controls.

### Tool permissions

Tool permissions can vary across predeclared phases without changing maximum authority. Tool and schema versions must be immutable inputs. Dynamic tool discovery cannot silently expand scope.

### Model restrictions

The warrant can bind executor principals and permitted model capability profiles per phase or continuation. Reassignment may be an explicit suspended-to-active transition or require supersession. The model must preserve agent neutrality and avoid vendor-specific lifecycle states.

### Principal binding

The state machine records the assigned executor, current execution lease, prior assignees, daemon epoch, and any authorized reassignment. Every transition retains actor and interface identity. Binding supports fencing but increases concurrency rules.

### Daemon ownership

Only daemon command handlers may issue or transition warrants. The daemon serializes transitions, enforces optimistic versions where necessary, leases running authority, and fences stale workers. Background workers act as typed principals through the same command boundary.

### Approval relationship

Issuance references effective immutable approvals. Later transitions reevaluate approval and delegation dependencies according to policy. Continuation cannot reinterpret human consent or increase approved scope. Material change requires a new approval and successor warrant.

### Authorization relationship

Every material transition and governed effect is authorized against current canonical state. Within a running lease, low-level operations may use prevalidated scope, but checkpoint and effect boundaries must reevaluate revocation, expiration, policy, principal, and state. The exact reevaluation frequency is a decision within the model.

### Checkpoint semantics

Checkpoints are first-class transitions with expected evidence, observed repository state, resource use, verification results, next allowed phase, and continuation outcome. Missing, failed, late, or contradictory checkpoints suspend or fail closed. Excessive checkpoint granularity can turn the state machine into a general workflow engine.

### Suspension and resume

Suspension is explicit and removes active execution authority without erasing unused scope. Resume is a continuation transition requiring current authorization, resolved suspension reason, valid environment, reconciled effects, and a new fenced lease. Resume from ambiguous state may be prohibited until reconciliation.

### Crash recovery

The daemon reloads canonical warrant state, detects stale running leases by epoch and liveness evidence, records interruption, and transitions to suspended or ambiguous state. It reconciles worktree state, command outcomes, budgets, checkpoints, and external effects before continuation. The state machine makes recovery visible but cannot infer external reality without evidence.

### Replay protection

State versions, stable request IDs, transition IDs, leases, nonces, attempt counters, and atomic transition events prevent duplicate progression. Old commands fail against resulting versions. External executors must not cache transition authority beyond their lease.

### Idempotency

Each transition and governed command has a stable idempotency key. Canonical state and audit evidence update atomically under ADR-001. Repeated transition requests return the prior result. External effects retain distinct intent and attempt identifiers and may remain ambiguous despite idempotent state transitions.

### External effects

Effect intents can require a checkpoint or substate before attempt. The warrant constrains targets and attempt counts, and ambiguity forces suspension or an explicit reconciliation state. Compensation may use reserved authority or a separate successor warrant. Modeling every effect as a warrant state would overfit the lifecycle and duplicate ADR-001 records.

### Verification gates

Verification gates are explicit transition guards. Evidence must identify exact revision, environment, command, result, and requirement. A failed gate may allow bounded remediation, require suspension, or terminate authority according to predeclared policy. Completion cannot bypass mandatory gates.

### Rollback behavior

Checkpointed repository and environment state supports bounded rollback. The warrant may reserve containment or compensation authority and record rollback transitions. Rollback does not erase evidence or necessarily restore external reality. If rollback exceeds original approval scope, it requires separate authority.

### Auditability

The lifecycle produces a coherent timeline of scope, state, actors, checkpoints, effects, verification, interruption, continuation, and completion. Rich state creates more impossible-state and consistency-check requirements, and operators must distinguish lifecycle facts from audit events.

### Operator ergonomics

Operators can see why execution is running, stopped, or complete and what is required next. More states and guards increase cognitive load, particularly for short tasks. Good rendering must explain current state, maximum and remaining authority, blocking conditions, and safe available actions.

### Implementation complexity

This option has the highest direct complexity: transition model, guards, concurrent versions, leases, timers, checkpoints, continuation, recovery, consistency checks, and exhaustive tests. It may centralize complexity that Options A and B distribute across task, attempt, supervisor, and audit records.

### Future distributed operation

Explicit state and leases clarify remote executor coordination, but a single canonical daemon remains the transition authority. Multi-writer or offline transition would require consensus, fencing, revocation-freshness, and partition semantics beyond this ADR. Remote workers can remain thin if all transitions return to the daemon.

### Failure modes and enduring constraints

- State explosion creates untestable or impossible transition combinations.
- Workflow policy becomes embedded in a generic warrant state machine.
- Long-running transitions or leases obscure immediate revocation behavior.
- Checkpoints become ceremonial rather than evidence-based.
- Recovery chooses an unsafe state when external outcomes are ambiguous.
- Schema and state semantics become difficult to evolve once durable warrants exist.

## Cross-Option Architectural Questions

### Enforcement depth

All options need layers of enforcement: daemon authorization, supervisor dispatch, host isolation, worktree containment, tool mediation, and post-action verification. The warrant model must define which layer is authoritative when enforcement observations disagree and how violations halt execution.

### Authority granularity

Very fine grants improve least privilege but increase policy size, operator fatigue, and transition volume. Coarse grants are usable but leave more discretion to an agent and toolchain. The decision must identify the minimum stable vocabulary for commands, files, network, tools, models, resources, and external effects.

### Ambient authority

Environment credentials, filesystem access, inherited processes, Git configuration, hooks, package-manager configuration, model tools, and network connectivity can exceed the documented warrant. A warrant is meaningful only if the host and supervisor can prevent or detect authority outside it.

### Attempt ownership and fencing

The daemon must prevent two workers or a restarted daemon from exercising the same exclusive authority concurrently. Possible mechanisms include canonical reservations, writer epochs, execution leases, process identity, host locks, and monotonic versions. Their failure behavior must be fail-closed.

### Revocation latency

No architecture can revoke an irreversible effect already performed. The contract must define the maximum interval between revocation and an executor's next enforced validation point, including during long commands, model calls, disconnected workers, and external operations.

### Completion authority

The daemon may conclude that a warrant's execution contract is complete. It may not infer that the resulting implementation is accepted. Required deterministic verification, adversarial review, findings disposition, and human gates remain downstream canonical decisions.

## Open Questions

1. Which lifecycle conditions must be canonical warrant states, and which may be derived from attempts, checkpoints, tasks, or audit evidence?
2. Does every attended execution also require a warrant, or only unattended and policy-governed execution?
3. What is the smallest stable capability vocabulary for commands, files, tools, network, models, and effects?
4. Are shell commands an admissible authority primitive, or must all governed operations use structured tool schemas?
5. How are interpreters, build systems, hooks, plugins, and scripts content-bound to prevent transitive authority expansion?
6. Which filesystem read operations require warrant authority, and how are repository-external dependencies handled?
7. Must host isolation enforce path restrictions before execution, or may some restrictions be verified after execution for low-risk work?
8. What network scopes are enforceable consistently on both macOS and Linux?
9. Are network targets bound by hostname, resolved address, service identity, protocol, credential, or effect type?
10. How are secrets and environment credentials excluded unless explicitly authorized?
11. Should a warrant bind an exact AI principal, a replaceable agent role, a capability profile, or a combination?
12. Which model changes are material enough to require warrant supersession or new approval?
13. What is the maximum allowed interval between authorization reevaluations during active execution?
14. Can long-running commands be safely interrupted on revocation, and who determines a safe interruption boundary?
15. What canonical evidence proves that a process is still running after daemon restart?
16. Which lease and fencing mechanism works reliably across supported macOS and Linux hosts?
17. When does a failed attempt consume authority, and may unused authority be returned after deterministic failure?
18. Are retry counts global, per operation, per checkpoint, or per external-effect intent?
19. Which actions require a checkpoint before execution, after execution, or both?
20. Can checkpoint continuation be automatic under preapproved policy, or must some checkpoints return to a human?
21. What evidence is sufficient to resume after a daemon crash, host restart, agent disconnect, or worktree drift?
22. How is ambiguous external-effect state represented in warrant availability?
23. Does revocation retain emergency containment or compensation authority, or must that use a separate warrant?
24. How does supersession prevent simultaneous use of predecessor and successor authority?
25. Does completion consume all remaining authority automatically?
26. Which verification failures permit remediation within the same warrant?
27. What resource budgets are enforceable deterministically across models and tools?
28. What audit and evidence retention period applies to completed, denied, expired, and unused warrants?
29. Can warrants be copied during backup or restore without enabling replay on a cloned host?
30. May remote executors operate while disconnected from the canonical daemon, and if so, with what revocation and evidence guarantees?
31. How should operators inspect effective authority without reading multiple underlying policy and lifecycle records?
32. Which impossible state/audit relationships must the ADR-001 consistency checker reject?

## Decision Matrix

The matrix compares architectural properties without ranking or recommending an option.

| Criterion | Option A: Approval-attached allowlists | Option B: Standalone capability warrants | Option C: State-machine warrants |
|---|---|---|---|
| Core authority representation | Static command scope attached to approval; conforming form needs a distinct derived grant | Immutable bounded capability-style grant | Bounded capability envelope plus explicit canonical lifecycle |
| ADR-002 separation | Literal form conflicts; mediated variant can preserve separation | Directly preserves approval/warrant separation | Directly preserves separation and models ongoing exercise |
| Command scope | Simple for exact commands; weak for transitive behavior | Structured operations or command capabilities | Structured scope with state- or phase-sensitive availability |
| Filesystem scope | Requires separate controls | First-class capability dimension | First-class dimension with checkpoint validation |
| Worktree isolation | Referenced but enforced externally | Strong immutable binding | Strong binding plus lifecycle checks |
| Network and tool scope | Awkward and indirect | Explicit capability dimensions | Explicit and potentially phase-sensitive |
| Model and principal binding | Possible but entangles approval subject | Natural immutable binding | Binding plus reassignment and lease transitions |
| Daemon ownership | Central evaluator required | Issuer, validator, and consumer | Sole transition authority and execution coordinator |
| Current-state authorization | Must be added beside approval | At issuance and exercise | At issuance, transition, checkpoint, and effect boundaries |
| Checkpoints | External supervisor behavior | Declared in warrant; state may be separate | First-class lifecycle transitions |
| Suspension and resume | Ad hoc process/attempt state | Lease or continuation records outside core warrant state | Explicit suspended and continuation semantics |
| Crash recovery | Must reconstruct execution around approval | Reconcile warrant, attempts, leases, and evidence | Explicit interruption states and guarded recovery transitions |
| Replay and idempotency | Separate attempt machinery required | Canonical reservation and consumption | Versioned transitions, leases, reservation, and consumption |
| External effects | Poor fit; requires separate rich records | Scope plus ADR-001 effect records | Scope, lifecycle gates, and ADR-001 effect records |
| Verification gates | External to authority model | Constraints with separate gate results | Transition guards and checkpoint evidence |
| Rollback | Named commands or external supervisor policy | Explicit rollback/compensation capability | Checkpointed rollback or compensation transitions |
| Auditability | Clear commands, weak lifecycle explanation | Precise grant and use evidence across several records | Coherent lifecycle with greater state complexity |
| Operator ergonomics | Simple initially; brittle at scale | Concise if capability rendering is good | Most explanatory; highest cognitive load |
| Implementation complexity | Low initially; hidden complexity in enforcement and recovery | Moderate to high | Highest and most centralized |
| Risk of ambient authority | High | Depends on capability and sandbox completeness | Depends on capability completeness and transition enforcement |
| Risk of state fragmentation | High | Moderate: attempts and checkpoints may diverge | Lower conceptual fragmentation; higher state-machine risk |
| Revocation precision | At command boundaries if reevaluated | At validation or lease boundaries | Explicit state with defined transition and checkpoint boundaries |
| Local-first fit | Strong under central daemon | Strong with opaque canonical grants | Strong under canonical daemon state machine |
| Future distributed fit | Weak without substantial additional semantics | Potentially strong with audience, holder, and revocation design | Clear remote coordination; distributed writers remain complex |
| Primary architectural hazard | Conflating consent with authority and mistaking commands for effects | Capability leakage, coarse grants, and fragmented lifecycle | State explosion and transformation into a workflow engine |

