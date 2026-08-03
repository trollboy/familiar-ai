# Familiar Canonical Command Model

**Status:** Normative  
**Date:** 2026-08-02

This document defines the primary interface contract for every operation that may mutate Familiar canonical state or initiate a governed external effect. It applies equally to commands originating through MCP, CLI, local socket, loopback HTTP, dashboard, tray, plugins, maintenance tools, background workers, and future adapters.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described by RFC 2119.

# Goals

The canonical command model exists to:

- provide one protocol-neutral mutation boundary;
- preserve the daemon as Familiar's sole mutable authority;
- make every requested mutation attributable to a typed principal and interface;
- enforce current-state authorization before governed change or execution;
- provide stable idempotency and optimistic concurrency semantics;
- make retries safe and duplicate behavior deterministic;
- bind commands to one project and an explicit target;
- atomically preserve canonical state changes and required material-action audit evidence;
- distinguish rejection, failure, interruption, ambiguity, cancellation, and success;
- preserve causality across commands, execution attempts, verification, review, acceptance, and completion;
- support crash recovery without guessing outcomes;
- behave identically across all adapters and callers; and
- evolve compatibly without embedding editor, agent, model, provider, or transport semantics.

# Non-goals

The command model does not:

- define a programming-language API;
- define a persistence layout;
- serve as a query contract;
- replace domain-specific validation or invariants;
- authenticate a principal by itself;
- grant human approval, delegation, authorization, or execution authority;
- replace a warrant for governed execution;
- encode general workflow orchestration;
- define verification, review, finding disposition, or acceptance as side effects of execution;
- make audit events an event-sourcing authority;
- make model output, client state, or derived intelligence canonical;
- guarantee exactly-once external effects;
- hide partial or ambiguous outcomes behind a successful response; or
- allow plugins, workers, maintenance tools, or adapters to bypass daemon-owned handlers.

# Command Lifecycle

A command has an immutable submitted envelope and a durable processing outcome. The envelope never changes as the command advances. Lifecycle labels describe command handling, not task, warrant, or product workflow state.

## 1. Constructed

The caller constructs the complete command envelope, including a stable `request_id`, current project and principal context, expected target version where applicable, and a versioned payload. Construction creates no authority and no canonical effect.

## 2. Received

The daemon-owned Command Layer receives the envelope through an adapter or authenticated internal boundary. It assigns or validates `command_id`, records trusted transport context separately from caller claims, and enforces size and structural limits.

Receipt does not mean acceptance, authorization, execution, or success.

## 3. Validated

The Command Layer validates:

- the envelope and payload version;
- field presence and format;
- project and target scope;
- principal and interface identity bindings;
- stable request semantics;
- expected-version requirements;
- command-specific preconditions that can be evaluated without mutation; and
- compatibility with supported command and payload versions.

Malformed, unsupported, internally contradictory, or scope-ambiguous commands are rejected before domain mutation.

## 4. Deduplicated

The Command Layer compares the `request_id` and semantic command fingerprint with prior requests in the same idempotency scope.

- An identical prior request with a terminal recorded outcome returns that outcome without re-executing the command.
- An identical prior request still in progress returns its current durable status or an explicit in-progress response; it does not start a second execution.
- Reuse of a `request_id` with a different semantic fingerprint is rejected as an idempotency conflict.

## 5. Concurrency Checked

When the target may be concurrently mutated, the Command Layer compares `expected_version` with the current canonical target version. A mismatch rejects the command as a concurrency conflict. The handler does not silently rebase, overwrite, merge, or reinterpret the request.

## 6. Authorized

The Policy and Authorization Engine evaluates the authenticated principal, interface, project, target, current policy, approval, delegation, warrant, host, task, and other applicable canonical state.

Authorization is evaluated against current state. Authentication, a valid signature, possession of a command, prior authorization, prior approval, or a prior successful attempt does not establish present authorization.

## 7. Accepted

A command is accepted only after validation, deduplication, concurrency checking, and authorization succeed and a domain handler is prepared to apply it.

Acceptance means Familiar has admitted the command for governed handling. It does not mean that an external effect occurred, execution completed, verification passed, review succeeded, or work was accepted.

## 8. Applied or Initiated

For an internal canonical mutation, the domain handler evaluates its invariants and prepares the canonical change and required audit evidence as one atomic unit.

For process execution or an external effect, the command records the applicable canonical authorization, warrant, lease, intent, or attempt boundary before the non-atomic action begins. The command cannot make external reality part of the canonical atomic unit.

## 9. Outcome Recorded

The Command Layer records one durable outcome classification:

- **Succeeded:** The command's defined canonical effect committed, or its defined externally observed outcome was authoritatively recorded.
- **Rejected:** The command was not admitted because validation, duplicate consistency, concurrency, authorization, scope, or preconditions failed.
- **Failed:** The admitted command could not produce its defined effect and the failure is known.
- **Cancelled:** An authorized cancellation prevented or stopped remaining work at a defined boundary.
- **Interrupted:** Processing stopped unexpectedly and evidence is insufficient to call it succeeded or failed, but no ambiguous external outcome is known.
- **Partially failed:** The command's internal atomic portion has a known result, while one or more separately governed non-atomic operations failed after some effects occurred.
- **Ambiguous:** Familiar cannot determine whether a process action or external effect occurred or what its authoritative outcome was.

An outcome is terminal for that `command_id` except that partial, interrupted, or ambiguous consequences may require separate reconciliation, compensation, cancellation, or successor commands. Such commands reference the original; they do not rewrite its outcome.

## 10. Response Delivered

The adapter renders the durable outcome without changing its meaning. Loss of the response does not erase the recorded result. A retry with the same `request_id` retrieves the same outcome or current processing status.

# Command Structure

Every command contains the following required fields. The complete set forms the immutable command envelope.

| Field | Meaning | Normative constraints |
|---|---|---|
| `command_id` | Stable identity of this accepted command envelope and its handling record. | Globally unique within Familiar's canonical scope. It remains unchanged across processing and outcome recording. A caller-proposed value is not trusted until admitted by the daemon. |
| `request_id` | Stable identity of the caller's logical mutation request across delivery attempts. | Reused for an exact retry; never reused for semantically different content within its idempotency scope. |
| `project_id` | Canonical project scope of the command. | Stable opaque project identity. Paths, repository URLs, names, and current directories are not substitutes. |
| `principal_id` | Typed principal accountable for requesting the command. | Stable opaque principal identity resolved through current authentication bindings. It is not inferred from `interface_id`. |
| `interface_id` | Identity of the interface or internal boundary through which the command arrived. | Identifies MCP, CLI, socket, HTTP, UI-mediated, plugin, scheduler, worker, maintenance, or future interface context without replacing the principal. |
| `command_type` | Stable semantic name of the requested operation. | Protocol- and vendor-neutral, version-compatible, and mapped to exactly one governed command contract. |
| `target` | Immutable identification of the command's intended domain object, subject, scope, or effect. | Project-scoped, type-explicit, unambiguous, and content- or version-bound when material content matters. |
| `expected_version` | Canonical target version the caller expects to mutate. | Required whenever concurrent mutation is possible. An explicit “no current version” value is used only for creation semantics. Omission is permitted only when the command contract proves versioning inapplicable. |
| `timestamp` | Caller-observed command creation time. | Informational and evidentiary; never sufficient for ordering, freshness, expiration, authority, or replay protection. Daemon receipt and canonical ordering are recorded separately. |
| `payload` | Versioned command-specific intent and inputs. | Complete, bounded, deterministic to validate, and immutable after submission. It contains no implicit authority. |
| `metadata` | Non-domain context needed for traceability, compatibility, causality, privacy, or delivery. | Bounded and namespaced. It cannot alter command semantics or authority unless the command contract explicitly promotes a named metadata field into its semantic fingerprint. |

## Immutable command envelope

The envelope comprises all required fields and any command-contract-defined optional fields. Once received and assigned a canonical `command_id`, it cannot be edited, patched, or enriched in place.

Trusted observations made by Familiar—such as daemon receipt time, authenticated binding, peer identity, writer epoch, authorization decision, outcome, and audit sequence—are attached as separate processing and evidence records. They do not overwrite caller-submitted fields.

Correcting any semantic field requires a new command with a new `command_id` and `request_id`. A successor or corrective command references the prior command through causal metadata.

## Payload versioning

Every payload identifies its contract version within the payload or through a command-type convention defined by the command registry. The version governs field meaning, defaults, validation, semantic fingerprinting, and response compatibility.

Payload evolution follows these rules:

- A receiver may accept an older supported version without changing its meaning.
- Unknown required fields, unknown variants, or unsupported versions are rejected unless the version contract explicitly defines safe preservation behavior.
- New optional fields may be added only when omission retains the previous semantics.
- A default cannot change the meaning of an already supported version.
- Renaming, removing, narrowing, widening, or reinterpreting a semantic field requires a new payload version.
- Down-conversion may occur only when it is lossless and produces identical authorization and mutation semantics.
- Adapters cannot translate unsupported semantics into a superficially similar command.

## Target semantics

The `target` identifies what the command intends to change or cause. Depending on the command, it includes stable IDs, target type, immutable subject identity, content hash, base revision, worktree, effect target, or other exact scope.

A target must not rely solely on a mutable display name, absolute host path, provider session, editor tab, process ID, or caller current directory. Repository paths are canonical repository-relative paths. A command affecting multiple objects must declare the bounded set and its atomicity semantics; it cannot imply an unbounded project-wide target.

## Metadata semantics

Metadata carries standardized context such as:

- parent command and root causal identifiers;
- correlation or trace identifiers;
- adapter and protocol versions;
- client capability declarations;
- locale and redacted presentation context;
- privacy and retention labels;
- retry attempt transport information; and
- extension namespaces approved by this contract.

Metadata is not a dumping ground for domain payload, credentials, raw secrets, approval, policy waiver, warrant scope, or mutable client state. Security-relevant metadata is verified against trusted context before use.

# Idempotency

`request_id` identifies a logical mutation request. Its idempotency scope is at least the canonical project and initiating principal; command contracts may define a broader scope when required to prevent duplicate external effects. The scope must remain stable across adapters and daemon restarts.

A semantic command fingerprint includes every field that can change validation, authorization, target, mutation, external effect, or required evidence. At minimum it includes `project_id`, `principal_id`, `command_type`, `target`, `expected_version`, payload version, semantic payload, and any explicitly semantic metadata. It excludes transport retry counters and other non-semantic delivery metadata.

For one idempotency scope and `request_id`:

- the first admissible semantic fingerprint becomes binding;
- exact retries return or converge on the same durable command handling record;
- a different fingerprint is an idempotency conflict;
- concurrent deliveries cannot create multiple admitted commands;
- expiry of transient transport state does not permit semantic reuse; and
- retention cannot discard the deduplication fact while a duplicate external effect or canonical mutation remains possible.

Idempotency of command admission does not prove that an external provider implements idempotent effects. External operations require their own stable effect and attempt identifiers and observed-outcome records.

# Optimistic Concurrency

`expected_version` protects targets that can be concurrently mutated. Authorization is not a substitute for concurrency control.

The daemon compares the expected version during the same governed handling boundary that applies the mutation. If the current version differs, the command is rejected without applying the requested mutation. The response identifies the conflict without disclosing unauthorized state.

The daemon does not automatically refresh `expected_version`, merge payloads, replay intent against a newer target, or treat a semantically compatible state as equivalent. A caller may issue a new command after querying current state and explicitly reconsidering intent.

Commands that mutate multiple versioned targets declare all required expected versions or bind an immutable aggregate subject. The command contract states whether the operation is atomic across those targets. Partial internal mutation is prohibited when the contract declares atomicity.

# Retries and Duplicate Detection

A retry uses the original `request_id` and an envelope with the same semantic fingerprint. The same `command_id` should be returned when the original request was admitted. A transport may repeat delivery; it may not invent a new logical request merely because a response was lost.

Before retrying, callers should query the original request outcome when that query is available. Familiar must safely handle concurrent retry and outcome-query races.

Retries are classified as:

- **Delivery retry:** Re-delivery before the caller knows whether the daemon received the command. It uses the same `request_id`.
- **Processing continuation:** Resumption of an interrupted internal operation under an existing contract. It is allowed only when that command explicitly supports continuation and the outcome is deterministic.
- **Operation retry:** A new attempt after a known failure. It uses a new command and request identity, references the original cause, and consumes separately authorized retry capacity.
- **Reconciliation:** A new command that resolves an ambiguous outcome from evidence. It does not replay the original effect.
- **Compensation:** A new authorized command that attempts to counteract a prior effect. It is not a retry or erasure.

Automatic retries must never repeat a non-idempotent or ambiguous external effect merely because no successful response was observed.

# Causality

Every command has a causal identity independent of wall-clock time. Causality supports audit explanation, rollback analysis, and recovery across commands and external effects.

Metadata represents, where applicable:

- a root causal ID for the bounded body of work;
- the immediate parent command or triggering record;
- the task, approval, authorization decision, warrant, lease, checkpoint, attempt, verification, review, finding, handoff, or acceptance records involved; and
- predecessor or successor relationships for supersession, compensation, reconciliation, and rollback.

A command may have multiple evidence dependencies but has one declared immediate triggering context. Causal links cannot confer authority and cannot be changed after admission. Timestamps help explain chronology but canonical ordering, versions, request identity, and explicit causal links determine relationships.

# Cancellation

Cancellation is a separate governed command. A cancellation request names the target command, attempt, lease, or warrant and supplies its expected version where applicable. The original command envelope is never modified.

Cancellation:

- requires current authorization;
- is idempotent;
- is effective only at a defined safe boundary;
- cannot reverse a committed canonical mutation;
- cannot guarantee reversal of an external effect already attempted;
- invalidates or narrows future execution authority according to warrant rules;
- records whether work stopped, had already completed, could not be interrupted, or became ambiguous; and
- may require a separate compensation or rollback command.

An interface disconnect, caller timeout, or lost response is not cancellation unless the command contract explicitly defines and safely enforces that behavior.

# Command Rejection

Rejection means the requested mutation was not admitted. Rejection occurs for reasons including:

- malformed or unsupported envelope or payload;
- unknown or inactive project, principal, interface, target, or command type;
- ambiguous or cross-project scope;
- failed authentication binding;
- failed current-state authorization;
- missing human approval or execution warrant;
- stale `expected_version`;
- request-ID conflict;
- immutable-subject mismatch;
- unmet precondition, expired authority, or exceeded budget;
- unavailable required host capability; or
- prohibited extension behavior.

A rejection produces no requested domain mutation or external attempt. Denied and other security-relevant rejections produce material evidence as required by policy and ADR-001. Rejection details must be sufficient for authorized remediation without leaking secrets or unauthorized project state.

# Command Failure

Failure means a command was admitted but its defined effect did not succeed and the negative outcome is known. Internal canonical mutation failure leaves no partial canonical state when atomicity is required.

A failure record identifies:

- the processing phase;
- the domain invariant or operation that failed;
- whether any external attempt occurred;
- the authoritative observed outcome;
- retained evidence;
- remaining warrant or retry authority;
- whether reconciliation, compensation, rollback, or human action is required; and
- which state, if any, changed solely to record the failure.

Failure cannot be rewritten as rejection, success, or absence. A later successful command references rather than replaces it.

# Partial Failure

Partial failure is permitted only where the command contract crosses an external or otherwise non-atomic boundary and explicitly defines separable stages. Familiar must not expose partial success for canonical mutations that are required to be atomic.

When partial failure occurs:

- completed internal and external stages are identified exactly;
- unattempted stages are distinguished from failed stages;
- every observed effect remains recorded;
- remaining authority is recalculated rather than assumed;
- dependent execution suspends when safety requires;
- compensation is treated as a new governed effect; and
- the response never collapses the result into a general success flag.

If Familiar cannot establish which stages occurred, the result is ambiguous rather than partially failed.

# External Effects

An external effect is a change to reality that cannot be committed atomically with Familiar's canonical state, including remote publication, network mutation, process-visible action, deployment, or another system's state change.

Every governed external effect uses four durable stages:

1. **Intent:** Before attempting the effect, Familiar records the exact target, requested effect, authority, warrant, lease, idempotency key, attempt limits, and causal identity.
2. **Attempt:** Familiar records that a specifically identified attempt began under valid current authority.
3. **Observed outcome:** Familiar records success, failure, denial, interruption, or ambiguity based on attributable evidence.
4. **Compensation or reconciliation:** A separate authorized command records any corrective effect or authoritative resolution.

The intent record must be durable before the attempt. An effect provider's idempotency feature should be used where available but does not replace Familiar's command idempotency. If the outcome cannot be proven, Familiar marks it ambiguous, blocks dependent continuation, and does not retry automatically.

Rollback of internal state cannot erase external reality. Compensation is itself an external effect and requires its own authorization and, for governed execution, warrant authority.

# Atomicity Requirements

Every governed state-changing command must update all canonical records within its declared internal mutation boundary and append its required material-action audit event atomically. Either the complete internal mutation and evidence metadata commit, or none of them do.

Atomicity includes:

- command admission and idempotency binding where admission becomes durable;
- expected-version comparison and resulting version update;
- all canonical target changes declared atomic by the command contract;
- authorization-decision linkage required for the mutation;
- warrant, lease, checkpoint, consumption, or approval transitions when they are the command target; and
- the material-action audit event and required causal references.

Denied, failed, interrupted, cancelled, partially failed, and ambiguous material actions also require durable evidence. Where their evidence is recorded after an attempted external effect, the record must preserve the possibility that canonical recording itself was interrupted.

No command may claim atomicity across external reality. Durable intent precedes the effect; outcome and compensation follow as separate recorded stages.

# Stage Relationship

The following sequence defines separation of authority and judgment for a complete governed body of work:

```text
Command
  ↓
Authorization
  ↓
Execution
  ↓
Verification
  ↓
Review
  ↓
Acceptance
  ↓
Completion
```

The arrows mean that a later stage may depend on evidence from an earlier stage. They do not collapse stages into one command or require every command to traverse every stage. Read-only commands do not exist under this contract; mutating administrative commands may not require execution, review, or acceptance. Warrant execution completion may occur before verification and does not imply acceptance. In this sequence, **Completion** means the governed task or acceptance workflow's terminal conclusion, not command-outcome recording or warrant completion.

## Command

**May:** Express one bounded mutation intent; identify its target and expected version; carry a versioned payload; reference prior evidence and causal records; request cancellation, reconciliation, or compensation through an appropriate command type.

**May not:** Authorize itself; grant approval; convey execution authority; mutate storage directly; execute an external effect merely by being submitted; infer success; or combine unrelated unbounded work.

## Authorization

**May:** Authenticate the relevant context through separate identity services; evaluate current principal, policy, approval, delegation, warrant, project, task, host, target, and risk state; allow or deny the exact command; explain governing conditions.

**May not:** Create human approval; broaden approval or warrant scope; change the command; perform execution; waive policy implicitly; use an LLM as authority; or make a prior decision permanently valid.

## Execution

**May:** Exercise only the authority in a current eligible warrant and lease; perform bounded process, filesystem, tool, model, network, or external-effect operations; emit checkpoint and outcome evidence; stop safely.

**May not:** Begin governed work without a warrant; expand authority; alter approval; decide verification or acceptance; hide subprocesses or effects; continue after lease or checkpoint authority ends; or mark its own claims correct.

## Verification

**May:** Run authorized deterministic checks; inspect the exact revision, diff, logs, repository state, environment, and declared invariants; produce reproducible results; block continuation or acceptance when policy requires.

**May not:** Modify implementation state; repair failures; issue or broaden warrants; treat missing checks as success; perform subjective acceptance; or convert a passing result into approval.

## Review

**May:** Independently challenge the implementation and evidence; produce attributable findings; preserve disagreement and uncertainty; request additional verification or changes.

**May not:** Be the same model declaring its own implementation correct where separation is required; alter implementation; dismiss or accept its own findings; replace deterministic evidence; issue human approval; or decide final acceptance.

## Acceptance

**May:** Apply deterministic acceptance policy to verification, review, findings disposition, approvals, waivers, and evidence; require an explicit human gate; record accepted, rejected, revise, or risk-accepted outcomes.

**May not:** Rewrite prior command, execution, verification, or review evidence; infer approval from consensus; waive a required human gate; treat warrant completion as success; or perform merge, publication, or deployment without separate authority.

## Completion

**May:** Record that the bounded task or acceptance workflow reached its declared stopping point; preserve final status, accepted revision if any, evidence, findings, decisions, rollback path, and handoff.

**May not:** Erase failures or ambiguity; imply acceptance when acceptance did not occur; retroactively authorize prior work; reactivate or broaden a warrant; or conflate terminal administrative closure with engineering success.

# Command Invariants

1. Every canonical mutation and governed external-effect initiation enters through one daemon-owned Command Layer.
2. Every command has one immutable envelope and one stable semantic fingerprint.
3. Every command is scoped to exactly one canonical project.
4. Every command has distinct initiating principal and interface identities.
5. Every command type defines one bounded semantic operation and explicit target.
6. Every concurrently mutable target uses optimistic concurrency.
7. Every admitted request has durable idempotency and outcome semantics.
8. A duplicate cannot cause a second canonical mutation or external attempt.
9. A retry cannot silently become a new operation.
10. A causal relationship cannot convey authority.
11. A command cannot broaden approval, delegation, authorization, or warrant scope.
12. A command outcome is distinct from execution, verification, review, acceptance, and task completion.
13. Correction, reconciliation, compensation, rollback, and supersession use new causally linked commands.
14. Cross-project commands are outside this contract and therefore prohibited.

# Security Invariants

1. All caller fields are untrusted until independently validated.
2. `principal_id` is verified through current replaceable authentication bindings.
3. `interface_id` cannot substitute for a principal or human approval.
4. Authentication does not imply authorization, approval, delegation, warrant authority, or acceptance.
5. Authorization is evaluated against current canonical state at the required command boundary.
6. AI, daemon, interface, organization, and service-account principals cannot satisfy human approval requirements.
7. Service accounts may request or execute delegated authority but cannot originate human approval.
8. Governed execution requires a current eligible warrant and lease.
9. Payload and metadata cannot smuggle credentials, hidden authority, policy waivers, or cross-project references.
10. Secrets are referenced and resolved at their authorized point of use; they are not embedded in commands unless an explicit security contract requires protected transport and retention.
11. Errors and rejections do not disclose unauthorized state, secrets, or another project's existence.
12. Plugins, workers, adapters, and maintenance tools have no raw mutation authority.

# Auditing Invariants

1. Every material command outcome retains actor, interface, project, task where applicable, command, request, target, authority, prior and resulting versions, outcome, timestamp, and causal identity.
2. Required canonical mutation and material-action audit evidence commit atomically.
3. Denied, failed, cancelled, interrupted, partially failed, and ambiguous material actions remain evidenced.
4. Audit evidence is append-oriented and cannot become authority for reconstructing current state.
5. Caller timestamps never replace daemon receipt, canonical ordering, version, or causal evidence.
6. Legacy or missing evidence is marked explicitly and never fabricated.
7. Evidence referenced by approvals, warrants, decisions, findings, verification, review, handoffs, acceptance, or rollback cannot be removed while the reference remains governed.
8. External-effect intent, attempt, observed outcome, and compensation remain distinct and causally linked.
9. Administrative, repair, recovery, reconciliation, and compatibility commands receive the same audit treatment as ordinary commands.
10. Audit and canonical state are restored and checked as one consistency boundary with referenced evidence.

# Compatibility Requirements

- Command semantics are independent of MCP, CLI, socket, HTTP, UI, plugin, editor, model, provider, and operating system.
- Every adapter maps the same logical operation to the same `command_type`, payload version, authorization rules, idempotency scope, and outcome semantics.
- Command and payload versions are explicit and independently evolvable only when their compatibility rules remain deterministic.
- Existing payload versions retain their documented field meanings, defaults, and authorization consequences.
- Receivers reject unsupported semantics rather than approximate them.
- Unknown optional metadata is preserved only when safe and ignored only when it is explicitly non-semantic.
- Outcome classifications and stable error categories remain portable across transports even when presentation differs.
- A newer sender may discover receiver capabilities before submitting a version the receiver does not support.
- Local-first operation requires command validation, authorization, idempotency, and outcome retrieval without network services.
- Future remote executors or team clients use the same envelope and canonical semantics; they do not become canonical writers.

# Extension Rules

New command types and payload versions may be added only through the registered Command Layer extension contract.

An extension command:

- must define one approved, bounded purpose;
- must declare target and project scope;
- must define required `expected_version` behavior;
- must define its semantic fingerprint and idempotency scope;
- must define authorization inputs and any human approval requirement;
- must define whether execution requires a warrant and lease;
- must define internal atomicity and every non-atomic external boundary;
- must define rejection, failure, cancellation, partial failure, ambiguity, retry, reconciliation, compensation, and recovery behavior as applicable;
- must define material audit and evidence requirements;
- must define payload version compatibility and size limits;
- must define deterministic verification of its invariants;
- must remain protocol-, editor-, model-, and provider-neutral; and
- must preserve project isolation and all accepted ADR constraints.

Extensions must not:

- add direct storage mutation;
- register a handler outside daemon ownership;
- use metadata to avoid a payload-version change;
- introduce hidden or ambient authority;
- let a plugin, agent, UI, or worker authorize itself;
- interpret a command as human approval;
- bypass warrants for governed work;
- create an event-sourcing dependency on audit records;
- make derived intelligence canonical; or
- introduce cross-project access without a separately accepted architectural decision.

# Normative Requirements

1. Every mutating Familiar operation **MUST** be represented as a canonical command and pass through a daemon-owned command handler.
2. Queries **MUST NOT** use this contract to perform hidden mutations.
3. Every command **MUST** contain `command_id`, `request_id`, `project_id`, `principal_id`, `interface_id`, `command_type`, `target`, `expected_version`, `timestamp`, `payload`, and `metadata` according to this contract.
4. The admitted command envelope **MUST** be immutable.
5. Every payload **MUST** identify a supported semantic version.
6. Adapters **MUST** preserve command semantics and **MUST NOT** implement independent mutation policy.
7. The daemon **MUST** authenticate caller context, validate scope, detect duplicates, enforce concurrency, and evaluate current-state authorization before applying a command.
8. A `request_id` **MUST** remain stable across exact delivery retries.
9. Reuse of a `request_id` with different semantic content **MUST** be rejected.
10. Duplicate delivery **MUST NOT** create duplicate canonical mutation, authority, execution, or external-effect attempts.
11. A command targeting concurrently mutable state **MUST** provide and enforce `expected_version` unless its command contract proves versioning inapplicable.
12. A stale version **MUST** cause rejection and **MUST NOT** trigger automatic merging or rebasing.
13. Authorization **MUST** be evaluated against current canonical state and **MUST NOT** be inferred from authentication, signature, approval, prior success, or command possession.
14. Human approval **MUST** remain distinct from command authorization and execution authority.
15. Governed execution **MUST** require an eligible warrant and lease and **MUST NOT** be authorized by a command alone.
16. Commands **MUST NOT** broaden approval, delegation, policy, authorization, or warrant scope.
17. Every governed canonical mutation **MUST** commit its required canonical changes and material-action audit evidence atomically.
18. Audit evidence **MUST NOT** be used as the authority for reconstructing current canonical state.
19. External effects **MUST** use durable intent, attempt, observed-outcome, and compensation or reconciliation records.
20. Familiar **MUST NOT** claim atomicity across external reality.
21. Ambiguous external outcomes **MUST** block unsafe dependent continuation and **MUST NOT** be retried automatically.
22. Cancellation, retry, reconciliation, compensation, rollback, correction, and supersession **MUST** use explicit commands with causal links; they **MUST NOT** rewrite the original envelope or outcome.
23. Command rejection **MUST NOT** apply the requested domain mutation or initiate its external effect.
24. Command failure, partial failure, interruption, cancellation, and ambiguity **MUST** remain explicit and evidenced where material.
25. Verification **MUST NOT** mutate implementation state or imply acceptance.
26. Review **MUST NOT** accept or silently dismiss its own findings.
27. Acceptance **MUST** remain a separate governed decision and **MUST NOT** be inferred from command success, warrant completion, verification, or model consensus.
28. Completion **MUST** preserve the actual acceptance and evidence status and **MUST NOT** erase failure or ambiguity.
29. All commands, evidence, caches, and extensions **MUST** enforce project isolation.
30. Agents, interfaces, service accounts, plugins, and background workers **MUST NOT** originate human approval or obtain direct canonical mutation authority.
31. Command processing **SHOULD** return stable, machine-readable outcome and error categories consistently across interfaces.
32. Callers **SHOULD** query an unknown command outcome before requesting a new operation.
33. External providers **SHOULD** receive stable idempotency keys when they support them.
34. Commands **SHOULD** be small, bounded, measurable, and have an explicit stopping condition.
35. Extensions **MAY** add command types only when they satisfy the extension rules and do not introduce an alternate authority path.
36. Implementations **MAY** cache non-authoritative validation or authorization inputs, but they **MUST** invalidate them by all relevant canonical versions and **MUST** perform required current-state checks at the governed boundary.

