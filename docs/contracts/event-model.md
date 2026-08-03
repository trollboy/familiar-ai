# Familiar Canonical Event and Evidence Model

**Status:** Normative  
**Date:** 2026-08-03

This document defines Familiar's canonical event and evidence model. Events are immutable evidence describing material actions and attributable observations.

This model is **not event sourcing**. Events are not domain commands, are not canonical operational state, are not replay instructions, and are not required or sufficient to reconstruct current state. Familiar's transactional domain records remain authoritative for current operational state. Repository source and Git history remain authoritative for source-code state.

Events exist only as evidence.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described by RFC 2119.

# Goals

The event and evidence model exists to:

- preserve immutable, attributable evidence for defined material actions;
- record what was requested, authorized, attempted, observed, denied, failed, interrupted, reconciled, or concluded;
- bind material evidence atomically to canonical state changes where atomicity is possible;
- preserve causality without treating timestamps or append order as complete causal truth;
- distinguish current operational state from historical evidence;
- support deterministic audit, consistency checking, recovery, rollback analysis, and human review;
- preserve evidence for approvals, warrants, execution, verification, review, acceptance, handoff, repair, and external effects;
- make denied, failed, interrupted, partial, and ambiguous outcomes visible;
- provide stable event meanings across callers, agents, models, editors, providers, and interfaces;
- retain referenced evidence for as long as canonical records require it; and
- support an explicit historical boundary without fabricating events for unobserved legacy actions.

# Non-goals

The event model does not:

- reconstruct canonical operational state;
- define an event-sourced aggregate, projection, snapshot, replay, or upcaster architecture;
- authorize a command, mutation, approval, warrant, execution, verification, review, or acceptance decision;
- replace the canonical command model;
- replace the canonical query model;
- initiate execution or an external effect;
- make a historical actor, policy, approval, or outcome currently valid;
- guarantee that an external observation is complete or permanently true;
- make model output or reviewer opinion deterministic fact;
- erase an original action when compensation or rollback occurs;
- create synthetic history for legacy records; or
- turn informational telemetry into governance evidence after the fact.

# Foundational Distinctions

## Event

An event is an immutable, typed, attributable evidence record that states that Familiar recorded a defined action, decision, transition, attempt, observation, or outcome at a particular evidence boundary.

An event proves only what its envelope and referenced evidence support. For example, an “attempt started” event proves that the attempt boundary was recorded; it does not prove that the intended effect occurred. An “observed success” event proves an attributable success observation under defined rules; it does not make the affected external state permanently true.

## Evidence artifact

An evidence artifact is immutable or integrity-protected supporting material referenced by an event or canonical record. Examples include logs, exact command manifests, diffs, test output, source manifests, signatures, process observations, and external response receipts.

An artifact is not an event by itself. An event supplies identity, attribution, taxonomy, causality, authority context, outcome semantics, and retention linkage. The artifact supplies supporting content.

## Canonical state

Canonical state answers what Familiar currently knows and governs operationally. Events answer what Familiar recorded about past material actions and observations. A current-state decision reads canonical state through the query contract; it never derives present authority by replaying events.

## Informational telemetry

Informational telemetry supports diagnosis, health, performance, or presentation but is not required evidence for a material action. It may be sampled, aggregated, expired, or unavailable according to operational policy. It cannot satisfy a required audit, approval, warrant, verification, review, or acceptance evidence obligation.

# Event Lifecycle

An event has no mutable domain lifecycle after commitment. The following lifecycle describes creation, validation, retention, and integrity handling of an immutable evidence record.

## 1. Classified

Before a governed action is accepted, its contract identifies whether each possible action and outcome is material or informational and which event type applies. Materiality is determined by the versioned event taxonomy, not by caller preference, success, severity, or convenience.

An action that is material remains material when denied, failed, cancelled, interrupted, partially failed, or ambiguous.

## 2. Constructed

The responsible daemon-owned handler constructs the complete event envelope from trusted command context, authenticated principal and interface identity, current authority, exact target, prior and resulting versions, outcome, causality, and evidence references.

Callers, agents, plugins, workers, and external systems may submit observations or proposed evidence. They cannot authoritatively append a canonical event or choose its materiality.

## 3. Validated

Before append, Familiar validates:

- event type and semantic version;
- required envelope fields;
- project and target scope;
- actor and interface attribution;
- command, request, authority, and causal references;
- prior and resulting version relationships;
- outcome validity for the event type;
- evidence identity and integrity metadata;
- privacy, redaction, and retention classification; and
- compatibility with the registered taxonomy.

An invalid event cannot accompany a successful material mutation. Validation failure causes the governed action to fail closed when event atomicity is required.

## 4. Appended

For a material internal state change, the event is appended atomically with the canonical mutation and durable command outcome. Both commit or neither commits.

For a material denial or failure that produces no requested canonical mutation, its evidence is appended through a separate governed evidence action. If evidence persistence itself fails, Familiar reports an operational-integrity failure; it does not claim the action was audited.

For an external effect, intent, attempt, observed outcome, reconciliation, and compensation are separate events at their respective durable boundaries. No event claims atomicity with external reality.

## 5. Available

Committed events are readable only through the canonical query contract and current read authorization. Availability, indexing, rendering, and search do not change event meaning or authority.

## 6. Retained

Events and referenced evidence remain retained according to materiality, project policy, legal constraints, and canonical references. Required evidence cannot be removed while referenced by governed current state or another retained evidence obligation.

## 7. Archived or redacted

Archival may change storage location or access latency without changing event identity, content meaning, causal relationships, or integrity requirements.

Redaction is governed and evidenced. It does not silently edit history. When policy permits content removal, Familiar retains an integrity-preserving record that identifies the event, the governed redaction action, the affected evidence class, and the fact that content is no longer available, without retaining prohibited material.

## 8. Integrity checked

Consistency checks verify event identity, append order, required relationships, version transitions, evidence references, retention holds, causal structure, and agreement with canonical state relationships. A detected gap remains an explicit gap. Repair cannot fabricate the missing historical actor, command, event, approval, warrant, or outcome.

# Event Identity

Every event has a stable, opaque `event_id` assigned at or before canonical append. The ID identifies exactly one immutable envelope and cannot be reused, reassigned, or made to identify a revised event.

Event identity is independent of:

- event timestamp;
- append position;
- command or request identity;
- project-relative sequence;
- target version;
- evidence artifact identity;
- provider or model response identity; and
- transport or presentation correlation.

Two events may describe related observations of the same action but remain distinct events. Duplicate append detection uses both `event_id` and a semantic event fingerprint. Reusing an event ID with different content is an integrity violation. Retrying an identical append must not create a second canonical event.

Correction, clarification, reconciliation, redaction, supersession, and compensation create new events causally linked to the original. They do not mutate the original event.

# Event Taxonomy

The taxonomy defines stable event classes and their outcome vocabularies. Event types are semantic contracts, not log message names.

## Command events

Describe command receipt where material, admission, rejection, authorization outcome, application, cancellation, failure, interruption, ambiguity, and terminal outcome. They reference the immutable command and request identities.

Command events do not replace the command record and cannot be used to resubmit or replay a command.

## Canonical transition events

Describe material creation, change, revocation, supersession, expiration, deletion, reconciliation, repair, or administrative transition of canonical state. They identify exact prior and resulting versions.

## Identity and approval events

Describe principal enrollment and status change, authentication-binding change, approval presentation, approval or denial, signature validation, delegation, use, expiration, invalidation, revocation, and supersession.

An approval event is evidence that an approval action was recorded. Current approval effectiveness is determined from canonical state, not from the event.

## Warrant and lease events

Describe warrant issuance and every material state transition, lease issue and renewal, checkpoint boundary and outcome, consumption, suspension, ambiguity, recovery, revocation, expiration, supersession, failure, cancellation, and completion.

A warrant event does not convey authority. Only current canonical warrant and lease state, evaluated through current authorization, permits execution.

## Execution events

Describe execution admission, process start, command or tool attempt, resource use at governed boundaries, stop condition, cancellation, timeout, scope violation, process outcome, interruption, and terminal handoff.

Execution events report what was attempted or observed; they do not verify implementation correctness.

## External-effect events

Describe the required external-effect stages:

1. **Intent:** The exact authorized effect, target, idempotency identity, attempt limits, warrant, lease, and expected observation are durable before attempt.
2. **Attempt:** A specifically identified effect attempt crossed its controlled execution boundary.
3. **Observed outcome:** An attributable observation classified success, failure, denial, interruption, or ambiguity.
4. **Reconciliation:** Later authoritative evidence resolved or further characterized an ambiguous outcome.
5. **Compensation:** A separately authorized counter-effect was intended, attempted, and observed.

No stage implies a later stage. Compensation does not erase the original effect.

## Repository and intelligence events

Describe repository discovery, identity reconciliation, file lifecycle, scan completeness, content observation, derived-artifact generation, reuse, invalidation, staleness, source conflict, and rebuild.

Intelligence events do not make summaries, indexes, or model output canonical source truth.

## Verification events

Describe verification intent, environment and revision binding, check start, exact operation, result, timeout, cancellation, missing check, stale result, invariant outcome, and evidence registration.

A passing verification event does not accept work. A failed or unavailable verification event cannot be rewritten as absence.

## Review and finding events

Describe reviewer assignment, independence evidence, review attempt, finding creation, conflict, disagreement, reviewer unavailability, review completion, and governed finding disposition.

Review events preserve claims and reasoning as attributable evidence. They do not turn reviewer opinion into deterministic fact or permit a reviewer to accept its own finding.

## Acceptance and handoff events

Describe acceptance evaluation, human gate, waiver or risk acceptance, accepted, rejected, revise, or blocked outcomes, handoff creation and acknowledgment, and final engineering-report generation.

An acceptance event records that an acceptance decision occurred. Current acceptance state remains canonical and independently queryable.

## Security and administrative events

Describe material authentication denial, authorization denial, scope violation, secret-access decision, daemon writer acquisition, recovery, migration, repair, backup, restore, retention, archival, redaction, integrity failure, and consistency-check outcome.

Administrative origin never lowers materiality or bypasses ordinary attribution and authority requirements.

## Operational informational events

Describe non-governing diagnostics such as transient health samples, performance measurements, cache statistics, queue depth, and non-material connection observations. They remain explicitly informational and cannot be referenced as sufficient evidence for a governed conclusion.

# Event Envelope

Every canonical material event contains the following required fields. When a field is inapplicable to an event type, the envelope uses an explicit `not_applicable` value defined by that type; omission is not permitted for fields needed to distinguish absence from missing evidence.

| Field | Meaning | Normative constraints |
|---|---|---|
| `event_id` | Stable identity of this immutable event. | Unique and permanently bound to one semantic fingerprint. |
| `event_type` | Registered semantic event name. | Stable, protocol-neutral, and defined by the versioned taxonomy. |
| `event_version` | Version of the event type and payload semantics. | Governs required fields, outcome vocabulary, compatibility, and interpretation. |
| `materiality` | `material` or `informational`. | Determined by registered taxonomy, never caller-selected at append time. |
| `project_id` | Canonical project scope. | Exactly one stable opaque project identity. Cross-project events are prohibited by this contract. |
| `task_id` | Related bounded task. | Required when a task applies; otherwise explicit `not_applicable`. |
| `actor_principal_id` | Typed principal attributable for the action or observation. | Stable opaque identity; system observation still identifies the responsible daemon or service principal. |
| `actor_principal_type` | Human, AI, daemon, interface, service account, organization, or other approved typed principal class. | Must agree with canonical identity at the action boundary. |
| `interface_id` | Interface or internal caller boundary through which the action arrived. | Distinct from actor identity; explicit internal boundary identity when no external interface applies. |
| `daemon_id` | Daemon installation or authority identity that admitted the event. | Required for canonical material events. |
| `writer_epoch` | Single-writer epoch under which append occurred. | Supports fencing and recovery; never confers authority by itself. |
| `command_id` | Governing command identity. | Required for command-driven material actions; otherwise explicit `not_applicable`. |
| `request_id` | Stable logical request identity. | Required when a command or external request applies; otherwise explicit `not_applicable`. |
| `target` | Exact object, subject, scope, transition, or effect described. | Project-scoped, typed, immutable in the envelope, and version- or content-bound where material. |
| `action` | Specific action or observation represented. | Must be valid for `event_type` and distinct from outcome. |
| `outcome` | Observed or decided result. | Uses the event type's closed vocabulary and distinguishes success, denial, failure, interruption, partial failure, cancellation, and ambiguity where applicable. |
| `prior_version` | Canonical target version before a transition. | Required for versioned transitions; explicit `not_applicable` or `no_prior_version` otherwise. |
| `resulting_version` | Canonical target version after a transition. | Required for successful versioned transitions; explicit `not_applicable` or `no_resulting_version` for denial, failure, observation-only, or creation semantics as defined. |
| `authority` | References to authentication, authorization, approval, delegation, warrant, lease, policy, and waiver records applicable to the action. | Records evidence of authority evaluated; never creates or extends authority. |
| `occurred_at` | Time the described action or external observation is believed to have occurred. | May be source-reported and uncertain; not ordering authority. |
| `observed_at` | Time Familiar or the named observer obtained the observation. | Distinct from occurrence and append time. |
| `recorded_at` | Time the event became durably appended. | Assigned by the canonical writer; still not complete causal order. |
| `append_sequence` | Monotonic append position within the canonical event boundary. | Represents commit order only, not universal occurrence or causal order. |
| `root_causal_id` | Stable identity of the bounded causal chain. | Shared across related work without implying authorization. |
| `parent_event_ids` | Direct predecessor events that caused or contextualized this event. | Forms a directed acyclic causal graph; empty only for defined causal roots. |
| `correlation_ids` | Non-causal grouping identifiers. | Used for search and diagnosis; cannot imply sequence or authority. |
| `evidence_refs` | Immutable references to supporting artifacts or canonical evidence records. | Integrity-protected, authorized, retention-aware, and sufficient for the event type. |
| `payload` | Versioned event-specific evidence content. | Bounded, immutable, validated, and free of undeclared secrets or authority semantics. |
| `metadata` | Namespaced compatibility, privacy, retention, redaction, and diagnostic context. | Cannot change event meaning, materiality, authority, causality, or outcome unless declared semantic by the registered version. |
| `integrity` | Hash, signature, or other approved integrity metadata for the envelope and referenced evidence manifest. | Detects alteration; does not prove current authority or truth beyond the evidence. |

## Immutable envelope

Once appended, no event-envelope field may be edited, reordered, or reinterpreted. A corrected observation is a new event that references the prior event and states the correction basis. A superseding event changes how later readers understand current evidence; it does not change the historical bytes or meaning of the earlier event.

Event payloads and metadata are versioned. A new version may add semantics only under the compatibility rules in this contract. Existing event versions retain their original meaning indefinitely.

## Evidence references

Evidence references identify artifacts by stable identity and integrity metadata. They include content identity, artifact class, producing principal or tool, source revision or target, creation boundary, privacy classification, retention rule, and availability state where applicable.

A reference does not prove the artifact is currently accessible. Missing, corrupt, redacted, or archived evidence remains explicit. A material event that requires evidence cannot be considered structurally complete when its required reference was never committed.

# Required Fields by Outcome

All material events use the complete envelope. Additional outcome rules apply:

- **Successful canonical transition:** `prior_version`, `resulting_version`, governing `command_id`, `request_id`, authority, and evidence are required.
- **Creation:** explicit `no_prior_version` and the new `resulting_version` are required.
- **Denial or rejection:** the requested target, actor, interface, command/request, evaluated authority, denial class, and no-resulting-version marker are required without exposing protected detail.
- **Known failure:** the admitted action, failure phase, known negative outcome, any unchanged or failure-only resulting version, and evidence are required.
- **Partial failure:** completed, failed, and unattempted stages are distinguished; each external effect is separately referenced.
- **Interruption:** last authoritative boundary, unknown remainder, and recovery requirement are required.
- **Ambiguity:** exact unknown outcome, attempted observation methods, blocked dependent authority, and reconciliation requirement are required.
- **Cancellation:** cancellation command, safe boundary, stopped and already-completed work, and remaining ambiguity are required.
- **Informational observation:** fields may use a reduced registered envelope, but project, type, version, actor or observer, times, materiality, payload, and integrity remain explicit.

# Causality

Causality describes why an event exists and which prior actions directly led to it. It is an explicit directed acyclic graph, not a timestamp inference and not a replay graph.

Every material event identifies one `root_causal_id` and its direct `parent_event_ids`. Parent relationships include, where applicable:

- command to authorization outcome;
- authorization outcome to canonical transition or warrant issuance;
- warrant and lease to execution attempt;
- checkpoint to continuation or suspension;
- external intent to attempt to observed outcome;
- ambiguity to reconciliation;
- original effect to compensation;
- execution result to verification;
- verification and revision to review;
- review to finding;
- finding to disposition;
- verification, disposition, approvals, and policy to acceptance; and
- accepted or terminal work to handoff and completion.

Causal references explain dependence; they never confer authority. A parent event's historical authority does not make a child action currently authorized. The child action independently references its current authority records.

Causal cycles are invalid. A later event may supersede the interpretation of an earlier observation but cannot become its historical cause.

# Correlation

Correlation groups related events for search, diagnostics, or reporting without asserting cause or order. Correlation may bind:

- one task, session, attempt, worktree, verification run, review assignment, provider invocation, host recovery, or operator incident;
- repeated observations of the same external target;
- events delivered through different interfaces; or
- a human-visible trace across commands and queries.

Correlation IDs are not authority, identity, ordering, idempotency, or causality. Sharing a correlation ID does not prove that events describe the same action or that one resulted from another.

# Material and Informational Events

## Material events

An event is material when the described action or outcome changes, attempts to change, governs, exercises, evaluates, or conclusively reports state or authority important to engineering integrity, security, execution, verification, review, acceptance, recovery, rollback, or audit.

Always-material classes include:

- canonical state mutation and attempted governed mutation;
- human approval, denial, revocation, expiration, supersession, and delegation;
- authorization allow, deny, or indeterminate decisions used for governed action;
- warrant and lease issuance or transition;
- governed execution admission, attempt, checkpoint, violation, and outcome;
- external-effect intent, attempt, observation, ambiguity, reconciliation, and compensation;
- deterministic verification intent and result;
- review assignment, finding, disposition, and completion;
- acceptance, waiver, risk acceptance, rejection, and completion decisions;
- repair, migration, retention, redaction, backup, restore, and consistency action; and
- security-relevant denial, boundary violation, writer ownership, or integrity failure.

Material events receive the full envelope, required atomicity, durable retention, consistency checking, and compatibility guarantees.

## Informational events

An event is informational only when losing it cannot alter, obscure, or make unverifiable any canonical state, authority decision, approval, warrant, execution boundary, external effect, verification, review, finding disposition, acceptance, rollback, recovery, or required audit conclusion.

Informational events may be sampled, aggregated, delayed, or discarded under policy. Their absence cannot be treated as evidence that an action did not occur. They cannot satisfy material evidence requirements or become the sole basis for a governed decision.

## Materiality governance

Every event type declares materiality before use. Conditional materiality is permitted only when deterministic, versioned rules identify the condition from trusted context.

Callers cannot downgrade materiality. A successful, failed, denied, cancelled, interrupted, or ambiguous outcome does not change an action's material class. If uncertainty exists, the event is treated as material until an approved taxonomy decision establishes otherwise.

An informational event cannot be promoted retroactively to claim that a past material action was fully audited. A new event may record that informational evidence was discovered, but the historical evidence gap remains explicit.

# Event Ordering

Familiar preserves several distinct ordering relations:

1. **Append order:** `append_sequence` records canonical commit order for events within the single-writer boundary.
2. **Canonical version order:** `prior_version` and `resulting_version` order transitions of one canonical target.
3. **Causal order:** Parent relationships state direct dependence across actions and observations.
4. **Attempt order:** Explicit attempt and checkpoint identifiers order bounded execution or external-effect attempts.
5. **Observation chronology:** `occurred_at`, `observed_at`, and `recorded_at` report different times with explicit uncertainty.

No one relation substitutes for the others. Append order is not complete causal order. Timestamp order is not authority and may differ because of clock skew, delayed observation, offline activity, or recovery. Correlation is not order.

For canonical mutations committed with their event, the state transition and event share one atomic boundary. For denial and failure events recorded separately, append order records only when evidence committed. For external effects, canonical intent precedes attempt, but external occurrence and outcome remain separately observed.

Queries over events define deterministic total ordering and stable tie-breakers under the canonical query model. A presentation may choose chronological rendering but must preserve the underlying ordering and causality distinctions.

# Evidence Retention

Retention protects the ability to explain and verify governed history. At minimum, events and evidence remain retained while referenced by:

- canonical decisions and architectural records;
- approvals, delegations, authorization decisions, warrants, and leases;
- tasks, attempts, checkpoints, and external effects;
- findings, verification results, reviews, and dispositions;
- handoffs, acceptance records, reports, and rollback or compensation records;
- unresolved ambiguity, repair, reconciliation, migration, or integrity findings; and
- historical-boundary or legal retention obligations.

Retention operates on a consistency boundary that includes canonical records, events, idempotency evidence, and referenced artifacts. Removing one component cannot leave another claiming complete evidence.

Retention actions are governed commands and material events. They verify references, privacy classification, archive integrity, restore capability, and project isolation before removal or archival.

Archival preserves event identity, semantics, causality, integrity, and authorized retrievability. Redaction and legally required deletion preserve an explicit integrity-safe record of what class of evidence became unavailable and why, without retaining prohibited content.

Events from the audited era cannot be silently deleted to make current history appear cleaner. Legacy records predating the audited-history boundary are marked explicitly; Familiar never fabricates events to fill that boundary.

# Relationship to Commands

Commands express bounded mutation intent and are the only interface for governed mutation. Events describe evidence about command evaluation and outcome.

- A command may cause one or more material events.
- Every successful material canonical mutation emits its required event atomically with the state change and command outcome.
- Material denials, failures, cancellations, interruptions, partial failures, and ambiguities emit evidence when the event can be durably recorded.
- An event cannot create, modify, cancel, retry, reconcile, compensate, or supersede a command.
- Replaying an event cannot replay a command.
- A corrective action uses a new command and produces new events linked to the original.
- Command idempotency prevents duplicate mutation; event append idempotency prevents duplicate evidence. Neither substitutes for the other.

Events cannot replace commands.

# Relationship to Queries

Queries are the only contract for reading canonical state and authorized evidence. Events may be returned as evidence through query results, with project scope, authorization, provenance, retention, availability, and ordering applied.

An event stream is not a substitute for a canonical query. A consumer that receives an event notification must query current canonical state before making a current-state decision. Missing an event must not make current state unknowable. Replaying all events is neither required nor sufficient to answer a canonical query.

Events cannot replace queries.

# Relationship to Canonical State

Canonical operational records are authoritative for current projects, tasks, identities, approvals, decisions, findings, verification results, handoffs, warrants, leases, checkpoints, acceptance, and other governed state.

Events are authoritative evidence that defined material actions or observations were recorded. They are not authoritative current values for those records. A consistency checker may compare events with canonical relationships and report missing, duplicate, or impossible combinations, but it cannot repair a discrepancy by choosing an event as current state or fabricating a transition.

Canonical state may reference events and artifacts as evidence. Events may reference prior and resulting versions. This bidirectional relationship supports explanation, not event sourcing.

Events never become canonical operational state.

# Relationship to Warrants

Canonical warrant state and current authorization determine whether execution authority exists. Warrant events record issuance, transitions, leases, checkpoints, consumption, ambiguity, revocation, expiration, supersession, failure, cancellation, and completion.

Possessing, reading, replaying, copying, or verifying a warrant event does not convey a capability, reactivate a lease, restore consumed authority, or prove that authority remains current. Execution admission must query current warrant and lease state and pass current authorization.

Events cannot authorize execution.

# Relationship to Verification

Canonical verification results identify the exact requirement, revision, environment, operation, result, and evidence. Verification events document verification intent, attempts, observations, failures, interruption, and result registration.

The event does not perform the check, make the output correct, or turn an implementing agent's claim into deterministic evidence. A passing event cannot substitute for the underlying reproducible result and artifacts. Missing, unavailable, stale, partial, or ambiguous verification remains explicit.

Verification events do not imply review or acceptance.

# Relationship to Review

Canonical review assignments, findings, and dispositions preserve the governed review state. Review events document assignment, independence evidence, attempts, findings, disagreement, completion, and disposition actions.

Reviewer claims remain attributable claims supported by evidence. The fact that an event exists does not prove a finding true, permit the reviewer to dismiss it, or establish consensus. A later disposition event records a separate governed decision and does not erase the original finding.

Review events do not imply verification or acceptance.

# Relationship to Acceptance

Canonical acceptance state records the current governed conclusion. Acceptance events document policy evaluation, required human gate, waivers or risk acceptance, and accepted, rejected, revise, blocked, superseded, or revoked outcomes.

An acceptance event proves that the recorded decision occurred under the referenced evidence and authority. It does not make prior execution authorized retroactively, erase failures, guarantee source correctness, perform merge or publication, or remain effective after canonical supersession or revocation.

Acceptance remains separate from command success, warrant completion, verification, review, and workflow completion.

# Causality and Relationship Example

The following illustrates evidence relationships, not a replayable workflow:

```text
command admitted event
  └─ authorization decision event
       └─ warrant issuance event
            └─ lease event
                 └─ execution attempt event
                      ├─ checkpoint event
                      └─ external-effect intent event
                           └─ attempt event
                                └─ observed-outcome event

execution outcome event
  └─ verification result event
       └─ review assignment and finding events
            └─ finding disposition event
                 └─ acceptance decision event
```

At every step, current canonical state—not the parent event—governs whether a new command is authorized. Branches may fail, suspend, remain ambiguous, or never occur. The diagram does not require every body of work to produce every event type.

# Event Invariants

1. Events exist only as immutable evidence.
2. Events never become canonical operational state.
3. Events do not reconstruct all current state and are not replay instructions.
4. Every material event has one immutable, versioned envelope and one stable identity.
5. Every event is scoped to exactly one canonical project.
6. Every material action class has deterministic, versioned materiality rules.
7. Materiality does not depend on whether the action succeeded.
8. Successful material canonical mutations and required events share one atomic boundary.
9. External reality never shares that atomic boundary.
10. Denial, failure, interruption, cancellation, partial failure, and ambiguity remain explicit evidence outcomes.
11. Append order, canonical version order, causal order, attempt order, and timestamps remain distinct.
12. Causal links never confer authority.
13. Corrections, reconciliation, compensation, redaction, and supersession create new linked events rather than edit history.
14. Referenced evidence remains retained while canonically required.
15. Legacy history is marked, not invented.
16. Events are read only through the canonical query contract.
17. Event types remain protocol-, editor-, agent-, model-, and provider-neutral.

# Security Invariants

1. Caller-provided event content is untrusted until validated and attributed by the daemon-owned evidence boundary.
2. Only daemon-owned handlers append canonical material events.
3. Actor principal and interface identity remain distinct and are verified against trusted context.
4. Event authority references describe prior evaluation; they do not grant present authority.
5. Event possession, signature validation, integrity verification, append order, or causal ancestry cannot authorize mutation or execution.
6. Event queries enforce current read authorization and project isolation.
7. Unauthorized callers cannot infer protected event existence, count, type, timing, causality, or evidence availability.
8. Secrets and protected content are excluded, referenced safely, redacted, or encrypted according to policy without destroying required attribution and integrity.
9. Plugins, agents, external systems, presentation surfaces, and background workers cannot append canonical events directly.
10. Cross-project event and evidence access is prohibited by this contract.
11. Correlation IDs, provider IDs, timestamps, and metadata cannot substitute for stable principal, command, request, project, or causal identity.
12. Informational telemetry cannot satisfy a security, approval, warrant, verification, or acceptance evidence requirement.

# Compatibility and Extension Rules

Every event type is registered with:

- a stable semantic name and version;
- fixed materiality or deterministic conditional-materiality rule;
- allowed actions and outcome vocabulary;
- required envelope and payload fields;
- target, version, authority, causality, correlation, and evidence rules;
- privacy, redaction, retention, and integrity requirements;
- relationships to canonical state and command types;
- ordering and consistency-check invariants; and
- representative deterministic conformance evidence.

Existing event versions retain their original meanings indefinitely. A new version is required to change materiality, action or outcome meaning, required authority, target interpretation, causal semantics, version relationships, integrity scope, or retention obligations.

New optional fields may be added compatibly only when omission preserves prior meaning, evidence completeness, security, and consistency checks. Unknown required fields or unsupported versions are rejected. Readers may preserve unknown optional fields only when the version contract declares them non-semantic and safe.

An extension may add event types only through the explicit event taxonomy contract. It must not:

- create event-triggered mutation authority;
- introduce replay-based current state;
- make an event a command or query substitute;
- make a plugin or external producer a canonical appender;
- downgrade an existing material action;
- redefine prior event meaning;
- treat an informational record as sufficient material evidence;
- omit project, actor, interface, authority, outcome, or causal attribution required by this contract;
- introduce cross-project evidence access; or
- make model or provider semantics canonical.

# Normative Requirements

1. Events **MUST** exist only as immutable evidence describing defined actions, observations, decisions, transitions, or outcomes.
2. Events **MUST NOT** become canonical operational state.
3. Events **MUST NOT** be used to reconstruct current state by replay.
4. Familiar **MUST NOT** introduce projections, snapshots, or upcasters that make this evidence stream authoritative for current state under this contract.
5. Events **MUST NOT** authorize execution, mutation, approval, delegation, verification, review, acceptance, or any other governed action.
6. Events **MUST NOT** replace commands.
7. Events **MUST NOT** replace queries.
8. Current operational decisions **MUST** read canonical state through the canonical query contract and act through a newly authorized command where mutation is required.
9. Every canonical material event **MUST** have a stable unique `event_id`, registered `event_type`, and explicit `event_version`.
10. Every material event **MUST** contain the complete required envelope and all event-type-specific evidence fields.
11. Every event **MUST** be scoped to exactly one canonical project.
12. Only daemon-owned handlers **MUST** append canonical material events; callers and workers **MUST NOT** append them directly.
13. The admitted event envelope **MUST** be immutable.
14. Corrections, reconciliation, compensation, redaction, and supersession **MUST** create new causally linked events and **MUST NOT** silently edit prior events.
15. Every event type **MUST** declare materiality before use.
16. Callers **MUST NOT** select or downgrade materiality.
17. A material action **MUST** remain material when denied, rejected, failed, cancelled, interrupted, partially failed, or ambiguous.
18. Every successful material canonical mutation **MUST** append its required event atomically with canonical state and the durable command outcome.
19. If required event append fails, the associated material canonical mutation **MUST NOT** be reported successful.
20. Material denials and failures **MUST** produce durable evidence when persistence is available; evidence-persistence failure **MUST** be reported as an operational-integrity failure rather than an audited outcome.
21. External effects **MUST** use separate durable intent, attempt, observed-outcome, reconciliation, and compensation events as applicable.
22. Familiar **MUST NOT** claim atomicity between an event append and external reality.
23. An external intent event **MUST NOT** imply attempt, and an attempt event **MUST NOT** imply success.
24. Ambiguous external outcomes **MUST** remain explicit until resolved by authoritative reconciliation or human disposition.
25. Compensation events **MUST** preserve and reference the original effect evidence and **MUST NOT** erase it.
26. Every material event **MUST** identify actor, actor type, interface, project, target, action, outcome, authority, timestamps, causality, and applicable command, request, task, version, warrant, lease, and evidence references.
27. Event timestamps **MUST NOT** be treated as complete causal order, replay order, or authority.
28. Append order **MUST** represent commit order only and **MUST NOT** be represented as universal occurrence or causal order.
29. Causal relationships **MUST** be explicit, acyclic, and non-authorizing.
30. Correlation **MUST NOT** be interpreted as causality, ordering, identity, idempotency, or authority.
31. Duplicate append of the same event identity and fingerprint **MUST NOT** create duplicate evidence.
32. Reuse of an event identity with different content **MUST** be treated as an integrity violation.
33. Canonical state and event relationships **MUST** be checked for missing, duplicate, contradictory, and impossible combinations without fabricating repairs.
34. Events and referenced evidence **MUST** remain retained while required by canonical decisions, approvals, warrants, tasks, findings, verification, reviews, handoffs, acceptance, effects, rollback, reconciliation, or integrity obligations.
35. Retention, archival, redaction, and deletion actions **MUST** be governed, attributable, and evidenced.
36. Archived events **MUST** preserve identity, meaning, causality, integrity, project isolation, and authorized retrievability.
37. Legacy records **MUST** be marked as predating the audited-history boundary when applicable, and Familiar **MUST NOT** fabricate historical events.
38. Material event payloads and referenced artifacts **MUST** exclude or protect secrets and sensitive content according to policy while retaining required attribution and integrity.
39. Verification events **MUST NOT** imply review or acceptance.
40. Review events **MUST NOT** imply deterministic correctness or acceptance.
41. Acceptance events **MUST NOT** retroactively authorize execution or erase prior failure, ambiguity, findings, or effects.
42. Warrant events **MUST NOT** convey, reactivate, restore, or broaden execution authority.
43. Informational events **MAY** be sampled or discarded only when their absence cannot affect any governed evidence requirement.
44. Informational events **MUST NOT** become the sole evidence for a material conclusion.
45. Event extensions **MUST** preserve this contract's authority separation, project isolation, compatibility, retention, causality, and immutability requirements.
