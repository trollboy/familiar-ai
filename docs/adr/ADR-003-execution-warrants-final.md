# ADR-003: Execution Warrants

**Status:** Accepted  
**Date:** 2026-08-02

## Decision

Familiar shall use state-machine execution warrants for governed work.

A warrant is daemon-owned canonical state that conveys bounded execution authority. Warrants are the only mechanism by which execution authority is conveyed for governed work. Human approval remains a distinct record of human consent and never directly authorizes execution. The daemon derives a warrant from current authorization state, including the applicable immutable approval set, and the execution supervisor acts only within that warrant.

Every warrant contains an immutable maximum capability envelope. No transition, checkpoint, continuation, lease, retry, recovery action, or administrative operation may broaden that envelope or the scope of its source approvals. A warrant's available authority may only be consumed, narrowed, suspended, revoked, expired, superseded, or terminated. Any expansion of authority requires a successor warrant derived through a new authorization decision and, where the expanded subject exceeds existing approval, new human approval.

Only daemon-owned command handlers may issue warrants or transition their state. Every material transition updates canonical state and appends the corresponding material-action audit event atomically in one SQLite transaction under ADR-001. Remote executors, clients, adapters, plugins, background workers, and maintenance tools may request transitions but may not perform or persist them independently.

Warrant state machines describe execution authority and execution lifecycle only. They must never become a general workflow engine. Task definition, approval, authorization policy, verification, independent review, findings disposition, handoff, and acceptance remain separate canonical concerns even when warrant transitions reference their records.

Future distributed execution may introduce remote executors. It may not introduce multiple canonical writers or permit an executor to authoritatively transition warrant state while disconnected from the canonical daemon.

## Rationale

Unattended execution spans time, processes, tool calls, filesystem mutations, model turns, verification boundaries, and sometimes external effects. A static command list or an immutable grant without explicit lifecycle semantics does not by itself answer whether authority is currently active, held by a live executor, awaiting a checkpoint, suspended after a crash, exhausted, revoked, or blocked by an ambiguous external outcome.

A canonical state machine makes those conditions explicit and recoverable. It gives the daemon a deterministic place to enforce leases, checkpoint authorization, consumption, expiration, revocation, supersession, ambiguity, and completion. This supports bounded unattended work without confusing the human's approval with the daemon's execution grant.

The immutable maximum envelope prevents lifecycle machinery from becoming a path for authority escalation. Mandatory checkpoints and renewable leases bound how long an executor can operate without current-state reevaluation. Explicit ambiguity handling respects ADR-001's central limitation: SQLite can atomically record intent, but it cannot atomically commit external reality.

The model carries additional schema, transition, and verification complexity. That complexity is accepted because it centralizes execution safety and recovery semantics that would otherwise be fragmented across tasks, processes, logs, and interface-specific behavior.

## Architectural Invariants

1. Warrants are daemon-owned canonical state machines.
2. A warrant is the only source of execution authority for governed work.
3. Approval records consent; authorization evaluates current permission; a warrant conveys execution authority; execution exercises that authority.
4. A warrant never broadens its source approval, delegation, authorization, task, project, or policy scope.
5. Maximum warrant authority is immutable after issuance.
6. State transitions may only preserve or reduce available authority through consumption, attenuation, suspension, revocation, expiration, supersession, or termination.
7. Any authority expansion requires a successor warrant and a new current-state authorization decision.
8. A material change beyond an existing immutable approval subject also requires new approval before a successor warrant may be issued.
9. Only daemon-owned command handlers issue warrants and transition warrant state.
10. Every material warrant transition is atomic with its canonical state update and audit event under ADR-001.
11. Every transition request has a stable request ID and idempotency semantics.
12. Every concurrently mutable warrant record has a version, and transition commands enforce the expected version.
13. Every warrant binds one project, one task, one immutable subject, one worktree, one executor principal, one authorization decision, one approval set, one maximum capability envelope, resource budgets, and verification requirements.
14. Every warrant defines lifecycle, checkpoints, leases, revocation, expiration, supersession, consumption, completion, and ambiguity behavior.
15. Checkpoints are mandatory authorization boundaries, not optional log entries.
16. Long-running execution requires renewable, fenced execution leases.
17. No executor may continue after lease loss, expiration, failed renewal, suspension, revocation, supersession, terminal state, or an unresolved required checkpoint.
18. Crash recovery never guesses whether execution or an external effect occurred.
19. Ambiguous external effects prevent continuation of dependent authority until reconciliation.
20. Completion means only that the warrant's execution contract reached its terminal completion conditions.
21. Completion does not imply correctness, verification, review, acceptance, merge, publication, or deployment.
22. Verification, review, findings disposition, handoff, and acceptance remain separate workflow decisions.
23. A warrant state machine contains execution semantics only and never becomes a general workflow engine.
24. Future remote executors remain subordinate to the canonical daemon; multiple canonical warrant writers are prohibited.
25. Denied, failed, interrupted, expired, revoked, superseded, ambiguous, and administratively reconciled actions retain evidence.

## Canonical Warrant Model

A canonical warrant consists of an immutable identity and authority portion plus versioned lifecycle state.

### Immutable identity and authority

At issuance, every warrant binds:

- a stable warrant ID and schema version;
- the project ID;
- the task ID and objective;
- the immutable or content-addressed execution subject;
- the repository identity and base Git revision;
- the authorized worktree or equivalently isolated environment;
- the executor's typed stable principal ID;
- the daemon installation or authority identity;
- the authorization-decision record;
- the complete approval set and relevant delegation lineage;
- the maximum capability envelope;
- resource and time budgets;
- checkpoint requirements and stop conditions;
- verification requirements;
- expiration conditions;
- permitted consumption and retry semantics;
- external-effect and compensation constraints;
- required evidence and retention references; and
- causal and issuance identifiers.

This portion is immutable. References in it must resolve to immutable or content-addressed records. A mutable policy, task, approval, tool configuration, model profile, or manifest may be consulted during authorization, but the warrant records the exact version or digest that bounded issuance.

### Versioned lifecycle state

Lifecycle state includes:

- current warrant state and version;
- remaining authority and consumed budgets;
- current lease, if any;
- attempts and active executor instance;
- latest satisfied checkpoint and next required checkpoint;
- suspension or ambiguity reason;
- expiration, revocation, supersession, and terminal facts;
- external-effect reconciliation dependencies; and
- references to transition and execution evidence.

Lifecycle state may reduce or make unavailable authority already present in the maximum envelope. It cannot add operations, paths, tools, network access, models, principals, effects, resources, time, attempts, or any other authority.

### Related records

Execution attempts, commands, checkpoints, leases, external-effect intents and outcomes, verification results, logs, diffs, findings, handoffs, and audit events remain distinct records with explicit warrant relationships. The warrant is not an event stream and need not contain copies of all evidence.

Canonical current state answers what authority remains available. Append-oriented audit events provide authoritative evidence for material transitions. Neither replaces repository source or Git history as authority for source-code state.

## Lifecycle

The warrant lifecycle is limited to execution authority. Its canonical states are:

1. **Issued:** The daemon has atomically created the immutable warrant from a successful authorization decision. It is not yet exercisable.
2. **Active:** Preconditions have been validated, the bound environment is available, and the warrant is eligible to grant a lease.
3. **Running:** A valid execution lease binds the warrant to one active executor instance and daemon writer epoch.
4. **Checkpoint Pending:** The prior execution interval ended or a mandatory boundary was reached. No further governed execution is permitted until the checkpoint is evaluated.
5. **Suspended:** Execution authority is temporarily unavailable because a defined condition requires resolution or an explicit continuation decision.
6. **Ambiguous:** Familiar cannot deterministically establish the outcome of an attempt or external effect. Dependent execution authority is unavailable pending reconciliation.
7. **Completed:** The warrant's execution completion conditions and terminal evidence were satisfied. Remaining authority is terminated.
8. **Consumed:** The warrant exhausted its authorized uses, attempts, or budgets without separately satisfying completion. It is terminal.
9. **Expired:** A declared validity boundary passed. It is terminal.
10. **Revoked:** Authority was explicitly withdrawn. It is terminal.
11. **Superseded:** A successor warrant replaced all remaining authority. It is terminal.
12. **Failed:** A non-recoverable execution or checkpoint condition terminated authority. It is terminal.
13. **Cancelled:** An authorized actor intentionally terminated execution without completion. It is terminal.

Issued, Active, Running, Checkpoint Pending, Suspended, and Ambiguous are non-terminal. Completed, Consumed, Expired, Revoked, Superseded, Failed, and Cancelled are terminal and cannot be reactivated.

A request to execute is not itself a warrant state. Approval and authorization precede issuance in their own canonical records. Verification, review, acceptance, merge, publication, deployment, and task completion follow or accompany execution in their own domain state; they are not warrant states.

## State Transition Rules

All transitions are explicit daemon commands. They require a stable request ID, expected warrant version, authenticated actor, interface identity, current authorization evaluation, transition-specific evidence, and an allowed origin state. The command atomically updates canonical state and appends its audit event.

The permitted transition families are:

- **Issue:** creates `Issued` from an authorization decision; it is not a transition from another warrant.
- **Activate:** `Issued` or `Suspended` to `Active` after validating immutable bindings, current approvals, policy, worktree, executor, budgets, and unresolved dependencies.
- **Lease:** `Active` to `Running` by atomically creating a fenced execution lease.
- **Reach checkpoint:** `Running` to `Checkpoint Pending`, recording the attempt boundary and evidence expected for evaluation.
- **Continue:** `Checkpoint Pending` to `Active` after mandatory checkpoint authorization and evidence evaluation.
- **Suspend:** any non-terminal state to `Suspended` at an enforceable safe boundary; active leases are invalidated or marked for bounded termination.
- **Mark ambiguous:** `Running`, `Checkpoint Pending`, or `Suspended` to `Ambiguous` when outcome cannot be established.
- **Reconcile:** `Ambiguous` to `Suspended`, `Active`, or an appropriate terminal state after authoritative evidence resolves the ambiguity and current authorization permits the transition.
- **Complete:** `Checkpoint Pending` or `Suspended` to `Completed` only after execution completion conditions and required terminal evidence are satisfied.
- **Consume:** any eligible non-terminal state to `Consumed` when use, attempt, or budget authority is exhausted.
- **Expire:** any non-terminal state to `Expired` when a validity boundary passes.
- **Revoke:** any non-terminal state to `Revoked` through an authorized withdrawal command.
- **Supersede:** any non-terminal state to `Superseded` atomically with or causally linked to issuance of its authorized successor; predecessor authority cannot remain exercisable.
- **Fail:** any non-terminal state to `Failed` when a defined non-recoverable condition occurs.
- **Cancel:** any non-terminal state to `Cancelled` through an authorized termination command.

Transition guards must reject:

- stale expected versions;
- duplicate requests with conflicting payloads;
- invalid origin or terminal states;
- missing or ineffective approval dependencies;
- changed immutable subjects, worktrees, repository identity, or base revision where prohibited;
- actor, executor, daemon, interface, or lease mismatches;
- expired, revoked, superseded, exhausted, or over-budget authority;
- capability, resource, effect, or verification requests outside the envelope;
- continuation with unresolved checkpoint failures or ambiguity;
- successor authority that is represented as an in-place mutation; and
- any transition that embeds task, review, or acceptance workflow into warrant state.

Revocation, expiration, supersession, cancellation, and failure do not erase attempts or reverse effects. They stop future authority and require policy-defined containment, handoff, rollback, or compensation through authority that is itself valid.

## Capability Envelope

The capability envelope defines the warrant's maximum authority. It must be deterministic, inspectable, immutable, and sufficiently structured for enforcement. It includes, where applicable:

- allowed command or operation schemas and explicit prohibitions;
- permitted executables, arguments, scripts, and content or version bindings;
- repository-relative filesystem read, create, modify, rename, delete, and execute scopes;
- authorized worktree and repository boundaries;
- network deny/allow policy, endpoints, protocols, credentials, data classes, and effect classes;
- permitted tools, plugins, versions, configurations, and transitive execution constraints;
- permitted model or agent capability profiles without making vendor identity canonical;
- environment variables, secrets, host capabilities, and subprocess rights;
- external-effect targets, effect types, quantities, attempt limits, and compensation bounds;
- concurrency, process, model-turn, token, cost, CPU, memory, storage, and elapsed-time budgets;
- retry, use, and command-count ceilings;
- checkpoint frequency and event-triggered checkpoint boundaries; and
- explicit stop conditions.

The effective authority at any moment is the intersection of:

1. the immutable maximum capability envelope;
2. the scope of effective source approvals and delegations;
3. current project, task, host, and policy authorization;
4. the warrant's current state and remaining budgets;
5. the active lease, if execution is running; and
6. any narrower restrictions imposed by completed checkpoints or safety controls.

No component in this intersection can expand another. If required work falls outside the intersection, execution stops and requests a successor warrant rather than silently widening scope.

Host isolation and tool mediation enforce capability boundaries before or during execution where practical. Deterministic diff, log, process, and invariant checks detect violations afterward. Detection does not retroactively authorize an out-of-scope action.

## Execution Leases

A lease is a short-lived, renewable, canonical reservation of exercisable warrant authority to one executor instance. A lease is not a new approval or independent warrant and cannot outlive or broaden its warrant.

Every lease binds:

- warrant and warrant version;
- executor principal and executor-instance identity;
- daemon identity and writer epoch;
- worktree or execution-environment identity;
- issue and expiration boundary;
- attempt and checkpoint interval;
- scope subset available during the lease;
- last successful renewal; and
- stable request and causal identifiers.

Only one exclusive execution lease may exercise the same exclusive warrant authority at a time. Lease creation, renewal, release, and invalidation are daemon-owned commands with idempotency and optimistic concurrency. A lease must be short enough to bound revocation latency and long enough to tolerate expected local scheduling delays.

Long-running execution requires periodic lease renewal. Renewal reevaluates current authorization, warrant state, executor and environment identity, remaining budgets, revocation and expiration, required checkpoints, and unresolved external effects. Renewal may preserve or narrow authority; it cannot expand it.

Failure to renew before expiration ends the executor's authority. The executor must stop at the earliest safe enforcement boundary and may not assume that a network failure, daemon restart, or unavailable clock extends its lease. A stale process is fenced by daemon writer epoch, lease identity, canonical version, and host controls. Process liveness alone never proves current authority.

## Checkpoints

Checkpoints are mandatory authorization boundaries. Every warrant declares periodic, phase, resource, and event-triggered checkpoint rules appropriate to its risk. At a checkpoint, the daemon:

- ends or fences the current execution lease;
- records the exact attempt boundary;
- captures required logs, commands, diffs, worktree state, resource use, model/tool provenance, and external-effect evidence;
- verifies that observed actions remained within the capability envelope;
- evaluates required deterministic verification gates;
- reevaluates approval, delegation, policy, subject, principal, worktree, expiration, revocation, and budget state;
- records findings, stop conditions, and unresolved ambiguity;
- atomically records the checkpoint result and warrant transition; and
- either permits a new lease, narrows available authority, suspends execution, marks ambiguity, or terminates the warrant.

Checkpoints are required at minimum before and after governed external effects, before authority crosses a higher-risk boundary, when a lease interval ends, when a declared budget threshold is reached, when scope or invariant checks fail, and before completion.

A checkpoint result does not approve new scope. Continuation occurs only within the existing envelope. A material change to the subject or a need for broader authority produces a successor-warrant request and, when required, a new human approval request.

## Recovery Semantics

Crash recovery must never guess execution state. On daemon restart or detected executor loss, Familiar:

1. acquires and records a new single-writer epoch;
2. prevents stale leases and prior-epoch processes from exercising authority;
3. loads canonical warrant, lease, attempt, checkpoint, consumption, and effect state;
4. observes the bound process and worktree without treating observation as proof of prior outcome;
5. records interruption evidence;
6. classifies each interrupted warrant as deterministically recoverable, suspended, ambiguous, or terminal according to evidence; and
7. requires reconciliation and current authorization before issuing any continuation lease.

Absence of a running process does not prove that a command failed. Presence of a process does not prove that its lease remains valid. Filesystem changes do not by themselves prove which command caused them. Missing evidence fails closed.

Deterministically idempotent commands may be retried only when their idempotency contract and prior outcome are established. Non-idempotent or externally effective operations cannot be retried merely because the daemon did not record success. Retries consume declared authority and use stable attempt and operation identifiers.

Repair, reconciliation, migration, and administrative state changes pass through governed daemon command handlers and emit audit evidence. They cannot rewrite history, fabricate outcomes, reactivate terminal warrants, or expand maximum authority.

## External Effect Semantics

External reality cannot participate in a SQLite transaction. Every governed external effect therefore uses ADR-001's durable sequence:

1. **Intent:** Before the effect, the daemon atomically records the exact intended effect, warrant, capability scope, target, idempotency key, attempt limit, and authorization evidence.
2. **Attempt:** The executor records that a specific attempt began under a valid lease and checkpoint boundary.
3. **Observed outcome:** Familiar records the best attributable observation of success, failure, denial, interruption, or ambiguity.
4. **Compensation or reconciliation:** When required, a separately authorized action records containment, compensation, or authoritative reconciliation.

External effects require a checkpoint before attempt and after observed outcome. A crash, timeout, lost connection, contradictory response, missing idempotency guarantee, or unverifiable target state makes the effect ambiguous when success or failure cannot be established.

Ambiguity immediately prevents continuation of authority that depends on the effect's outcome. The warrant enters `Ambiguous`; its active lease is invalidated. Continuation requires reconciliation evidence and an authorized transition. Familiar never assumes failure and retries, assumes success and proceeds, or edits canonical state merely to make it consistent with an expectation.

Compensation is a new external effect, not a rollback of history. It requires remaining explicitly reserved capability or a successor warrant. Revocation of the original warrant does not silently create compensation authority.

## Verification Relationship

A warrant declares required verification operations, evidence, checkpoints, and terminal execution conditions. Verification results remain separate canonical records produced by deterministic systems or attributable reviewers.

Verification gates may control warrant continuation or completion. They cannot broaden authority, rewrite the approved subject, or convert execution completion into acceptance. An implementing agent's assertion that work is correct is never sufficient verification.

The following remain separate decisions:

- **Warrant completion:** the daemon determines that the bounded execution contract and terminal evidence are complete.
- **Deterministic verification:** verification services establish reproducible facts about the resulting revision and environment.
- **Independent review:** a separate reviewer produces findings and reasoning.
- **Findings disposition:** governed policy or a human determines whether findings block progress or constitute accepted risk.
- **Acceptance:** policy and any required human gate determine whether the result is accepted.
- **Merge, publication, deployment, or other effect:** each occurs only under its own authorization and warrant scope.

A completed warrant may have failed verification, unresolved findings, or an unaccepted result. A verified result may still lack review or acceptance. No status is inferred across these boundaries.

## Consequences

### Benefits

- Current execution authority is explicit, queryable, bounded, and recoverable.
- Immutable maximum scope prevents in-place privilege escalation.
- Mandatory checkpoints limit drift and provide deterministic continuation boundaries.
- Renewable leases bound stale-executor and revocation risk.
- Explicit suspension and ambiguity prevent unsafe guessing after crashes or uncertain effects.
- Approval, authority, execution, verification, review, and acceptance retain clear accountability boundaries.
- Central daemon ownership keeps semantics identical across MCP, CLI, socket, HTTP, dashboard, tray, plugins, and future clients.
- Remote executors can be added without creating competing canonical writers.

### Costs and tradeoffs

- Canonical schemas must represent warrants, states, versions, transitions, leases, checkpoints, consumption, and evidence relationships.
- Transition guards, timers, fencing, recovery, and consistency checking require substantial deterministic test coverage.
- Checkpoint and lease frequency introduces runtime overhead and requires careful host integration on macOS and Linux.
- Fine-grained capability envelopes can be difficult for humans to inspect and for tools to enforce consistently.
- External-effect ambiguity may suspend work until a human or authoritative system can reconcile it.
- Terminal warrants cannot be reopened; even modest authority expansion requires successor issuance.
- Durable state semantics become compatibility obligations that must evolve carefully.
- Keeping warrant state execution-only requires discipline when workflow features seek convenient reuse of its state machine.

## Verification Requirements

Conforming implementations must provide deterministic evidence that:

1. Governed execution cannot begin or continue without an eligible canonical warrant.
2. Approval records, authorization decisions, warrants, execution attempts, verification results, reviews, and acceptance records remain distinct.
3. Every warrant binds the required project, task, immutable subject, worktree, executor principal, authorization decision, approval set, capability envelope, resource budgets, and verification requirements.
4. Maximum authority cannot be edited after issuance.
5. No transition, continuation, lease renewal, retry, recovery, repair, migration, or administrative command can expand authority.
6. Authority expansion is rejected unless represented by a successor warrant with current authorization and required approvals.
7. Only daemon-owned command handlers can transition warrant state.
8. Every material transition atomically updates canonical state and appends the required ADR-001 audit event.
9. Duplicate transition requests are idempotent, while request-ID reuse with different content is rejected.
10. Stale warrant versions, invalid origin states, impossible transitions, and terminal-state reactivation are rejected.
11. Only one valid exclusive lease can exercise the same exclusive authority at a time.
12. Lease expiry, renewal failure, daemon epoch change, revocation, supersession, or suspension fences further execution.
13. Long-running execution cannot exceed its lease without successful current-state renewal.
14. Every declared checkpoint blocks further execution until its evidence and current authorization are evaluated.
15. Checkpoint continuation never expands the maximum envelope.
16. Command, filesystem, worktree, network, tool, model, resource, and external-effect boundaries are enforced or deterministically detected as declared.
17. Scope violations suspend or terminate authority and produce evidence; detection never legitimizes the violation.
18. Crash recovery classifies interrupted execution from evidence and never infers an unobserved outcome.
19. Prior-epoch or stale processes cannot resume authority after daemon restart.
20. Ambiguous external effects invalidate dependent leases and block continuation until reconciliation.
21. External-effect intent, attempt, observed outcome, and compensation records retain warrant, lease, request, and causal identity.
22. Consumption and retry accounting cannot restore authority after failed or ambiguous attempts unless the immutable contract explicitly permits it and evidence supports it.
23. Revocation, expiration, supersession, consumption, failure, cancellation, and completion terminate future authority as defined.
24. Completion requires terminal execution evidence and eliminates remaining executable authority.
25. Completion does not create verification, review, acceptance, merge, publication, or deployment state.
26. Consistency checks detect missing, duplicate, contradictory, or impossible warrant, transition, lease, checkpoint, effect, evidence, and audit relationships.
27. Backup and restore preserve warrants, their evidence, referenced approvals, authorization decisions, leases, effects, and audit history as one consistency boundary without enabling replay.
28. Identical semantics apply through MCP, CLI, local socket, loopback HTTP, dashboard, tray, plugins, maintenance tools, and background workers.
29. Remote executors cannot mutate canonical warrant state directly or act beyond daemon-issued leases.
30. The warrant state model contains no task-planning, review, findings-disposition, or acceptance transitions.

## Supersession Conditions

This ADR should be reconsidered only if evidence establishes that one or more of the following is necessary:

- A different bounded-authority model provides equivalent or stronger enforcement, recovery, revocation, and audit guarantees with materially less complexity.
- Supported host platforms cannot provide reliable lease fencing, checkpoint enforcement, or worktree isolation required by this model.
- The capability vocabulary cannot represent required execution safely without becoming indistinguishable from ambient authority.
- Required unattended operations cannot make progress under mandatory checkpoint or ambiguity rules without disproportionate human intervention.
- Familiar adopts a distributed canonical-state architecture in which a single daemon can no longer own transitions; such a change also requires superseding ADR-001 and defining consensus, fencing, and partition semantics.
- Regulatory or organizational constraints require a different execution-authority or evidence model.
- Deterministic operational evidence shows that the state machine has become a general workflow engine despite enforceable subsystem boundaries.

Any superseding decision must preserve bounded authority, explicit human approval gates, approval/execution separation, immutable maximum scope, daemon-governed or equivalently singular canonical transitions, fail-closed recovery, external-effect ambiguity handling, deterministic verification, auditability, rollback or compensation capability, and the separation of completion from acceptance unless it explicitly supersedes the constitutional basis for one of those constraints.
