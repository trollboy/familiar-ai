# ADR-002: Human Identity and Approval Semantics

**Status:** Accepted  
**Date:** 2026-08-02

## Decision

Familiar shall use a hybrid of the capability-based and explicit durable-approval models evaluated in the ADR-002 decision-preparation document.

Human approval is represented by an explicit, durable, canonical approval record. Approval records are cryptographically signed where practical. A signature proves the integrity and provenance of the signed approval content; it does not establish that the signer currently has authority, that the approval remains effective, or that execution is permitted. Authorization is always evaluated against current canonical state.

Approval and execution are distinct. Human approval records informed consent to a specific immutable subject. It never directly authorizes execution. Execution authority is conveyed through a separate warrant derived from one or more effective approvals and current policy. Warrants are capability-like, bounded, revocable execution grants enforced by the daemon.

Humans, AI agents, daemons, interfaces, organizations, and service accounts are typed principals with stable opaque identifiers. Authentication methods are replaceable bindings to those identities. AI agents, daemons, interfaces, and service accounts never satisfy a requirement for human approval. Service accounts may exercise properly delegated execution authority but may not originate human approval.

Approval subjects must be immutable or content-addressed. Any material change to an approved subject invalidates the approval for the changed subject and requires a new approval. Approval lifecycle, expiration, supersession, revocation, delegation, and evidence semantics are binding architectural invariants.

Familiar must operate fully in local-first mode. Its canonical identity, approval, authorization, and warrant semantics must also permit future team and distributed operation without reinterpretation.

## Rationale

Familiar exists to preserve human judgment while safely supervising replaceable coding agents. A boolean approval flag cannot adequately preserve who approved what, under which conditions, or whether the approved subject changed. Conversely, treating approval itself as an executable capability conflates human consent with machine authority and makes revocation, policy reevaluation, and accountability harder to reason about.

Explicit canonical approval records preserve durable human intent and evidence. Capability-like warrants translate that intent—together with current policy and authorization state—into narrowly bounded execution authority. This separation supports least privilege, revocation, crash recovery, deterministic verification, and clear attribution without requiring network identity services or making cryptographic signatures the source of authority.

Signing approval records where practical strengthens tamper evidence and portability. Authority nevertheless remains a current-state determination because signatures cannot prove present role membership, project ownership, policy compliance, non-revocation, or the continued validity of an approved subject.

## Architectural Invariants

1. A human-required approval gate can be satisfied only by an eligible natural-person principal.
2. Approval is an explicit canonical record, never an inference from authentication, authorization, UI state, configuration, prior execution, or possession of a credential.
3. Authentication, authorization, approval, delegation, warrant issuance, execution, and accountability remain distinct concepts and records.
4. Authorization is evaluated against current canonical state at every governed decision point.
5. Human approval does not itself confer execution authority.
6. Execution requires an effective warrant whenever policy requires governed authority.
7. Warrants are bounded, capability-like, revocable grants derived from effective approvals and current policy.
8. All principals have stable opaque canonical IDs and explicit principal types.
9. Authentication credentials and external identity-provider identifiers are replaceable bindings, not canonical identity.
10. AI agents, daemons, interfaces, organizations, and service accounts cannot impersonate or substitute for a human approver.
11. Service accounts cannot originate human approval.
12. Approval subjects are immutable or content-addressed, and material changes invalidate prior approval.
13. Approval issuance, denial, expiration, supersession, revocation, delegation, attempted use, and warrant derivation are governed material actions under ADR-001.
14. Approval evidence remains retained while referenced by a decision, finding, verification result, warrant, handoff, acceptance record, or other canonical record subject to retention.
15. The daemon owns approval and authorization mutations. Clients and background processes cannot mutate canonical approval state directly.
16. Local-first operation cannot depend on a network identity provider, remote clock, cloud signing service, or organizational directory.
17. Future team or distributed operation may extend authentication and policy sources but cannot change these canonical semantics.

## Principal Model

### Human principal

A human principal represents one natural person accountable for an approval or other action. Its stable opaque principal ID is independent of username, operating-system account, email address, provider subject, signing key, device, or interface. Those values may be replaceable bindings or attributes.

A human principal may hold project or organizational roles that make the person eligible to approve a class of action. Role possession is evaluated at authorization time; it is not standing approval.

### AI principal

An AI principal represents an attributable non-human worker or reviewer invocation, agent installation, or durable agent role. Its record must retain enough provider, model, instance, task, and attempt provenance to support accountability without making a vendor identity canonical. An AI principal may request work, propose decisions, execute under a warrant, or produce findings. It cannot provide human approval.

### Daemon principal

A daemon principal identifies the Familiar installation or service authority performing policy enforcement and canonical state transitions. Individual process instances and writer epochs must be distinguishable for recovery and audit. A daemon may validate approvals and issue warrants through governed command handlers; it cannot originate human approval.

### Interface principal

An interface principal identifies the adapter or client boundary through which an action arrived, such as MCP, CLI, local socket, loopback HTTP, dashboard, tray, or plugin. Interface identity supplements rather than replaces the initiating human, AI, daemon, or service-account identity. An interface cannot provide human approval.

### Service-account principal

A service-account principal represents non-human automation or integration. It may authenticate, request operations, and execute explicitly delegated authority within policy. It never satisfies a human approval requirement and cannot originate human approval.

### Organization principal

An organization principal represents a durable ownership and policy scope. It may own projects and define roles, membership, thresholds, and delegation rules. An organization is not a natural person; organizational policy is satisfied through attributable actions by eligible principals.

### Project identity

A project is a stable stewardship and authorization scope, not an acting principal. Its opaque identity is independent of a mutable filesystem path, worktree, remote URL, branch name, or display name. Approval, delegation, and warrants are project-scoped unless policy explicitly defines a broader scope.

## Approval Model

An approval records an eligible human's explicit decision concerning an exact subject. At minimum, a canonical approval must identify:

- the approval ID and schema version;
- the human approver principal;
- the project and applicable task;
- the immutable or content-addressed subject and its type;
- the requested approval class;
- the decision and any conditions or limitations;
- the policy and authority basis evaluated when accepted;
- creation, effective, and expiration times or state-based expiry conditions;
- supersession and revocation relationships;
- delegation lineage, if delegation is involved;
- the authentication binding and interface used;
- the canonical rendering or manifest presented to the human;
- signature envelope and credential binding when signed;
- request, command, causal, and audit identifiers required by ADR-001; and
- evidence references sufficient to reproduce what was approved.

Approval subjects must identify their complete material content. A subject may be a canonical immutable record, a deterministic manifest, or a cryptographic digest over canonicalized content and referenced artifacts. Human-readable presentation must be bound to the same canonical subject. Approval of a summary does not imply approval of underlying content that is absent from the subject.

Cryptographic signatures should be used when suitable local key storage, canonical serialization, recovery, and usable confirmation are available. Unsigned approvals may be permitted by explicit policy where the authenticated local transaction and retained audit evidence provide proportionate assurance. Signed and unsigned approvals have the same authorization rule: current canonical state determines effectiveness and authority.

## Authentication Model

Authentication establishes that a request controls a credential currently bound to a typed principal. It does not establish authorization, approval, delegation, or execution authority.

Authentication bindings may include operating-system credentials, local IPC credentials, host-backed signing keys, passkeys, service credentials, or future organizational identity-provider subjects. Bindings are versioned, revocable, rotatable, and many-to-one with a stable principal. Replacing or losing a binding does not create a new human identity by default.

Approval policy may require stronger authentication, recent reauthentication, proof of presence, or a signature for higher-risk approval classes. The daemon validates the binding and records the method and assurance evidence without storing unnecessary secret material.

## Authorization Model

Authorization answers whether a principal may perform a particular command in the current project state. It is a deterministic daemon-owned policy evaluation over canonical state, including:

- principal type and current status;
- project or organizational membership and role;
- command and approval class;
- subject identity and version;
- active delegations;
- approval effectiveness;
- warrant state and scope;
- revocation, expiration, supersession, and consumption state;
- task, project, host, and daemon state; and
- applicable policy and human approval gates.

A valid signature is evidence considered during authorization; it never overrides current canonical policy or state. Authorization fails closed when required identity, approval, revocation, subject, delegation, or warrant state cannot be established.

## Delegation Model

Delegation is an explicit canonical grant allowing one principal to exercise a defined subset of another principal's delegable authority. Delegation is not approval and cannot convert a non-human principal into a human approver.

Every delegation must identify its issuer, recipient, project, authority classes, scope, constraints, effective period, revocation state, and parent delegation where applicable. Delegated scope must be equal to or narrower than the delegator's current delegable authority. Delegation chains must prevent amplification and cycles, enforce maximum depth, and preserve complete lineage.

Human approval authority is non-delegable to AI agents, daemons, interfaces, service accounts, or organizations. Policy may allow one eligible human to delegate specified approval authority to another eligible human, but the delegate's resulting approval remains their own attributable human act. Execution authority may be delegated to AI or service principals only through a warrant.

Revocation, expiration, supersession, or loss of authority in a delegation must affect dependent approvals and warrants according to explicit policy. The daemon must reevaluate that dependency before further governed execution.

## Approval Lifecycle

An approval progresses through explicit canonical states and material audit events:

1. **Proposed:** An immutable approval subject and requested approval class are registered.
2. **Presented:** The exact canonical subject or bound human-readable rendering is presented to the human.
3. **Decided:** The human explicitly approves or denies the subject using an authenticated identity; the decision is recorded and signed where practical.
4. **Effective:** Current authorization policy confirms that an approved decision is eligible to satisfy its gate.
5. **Used:** The approval contributes to a warrant decision or another specifically permitted governed transition. Use does not erase the approval.
6. **Completed:** The governed activity reaches its terminal outcome and retains its approval and warrant links.

An approval may instead or subsequently become:

- **Expired** when its time, task-state, revision, use, or other declared validity boundary passes;
- **Superseded** when a later explicit approval replaces it for the same defined purpose;
- **Revoked** through an authorized canonical command;
- **Invalidated** when its subject changes materially or a required dependency ceases to be valid; or
- **Denied**, which is a durable terminal human decision unless a newly proposed subject receives a separate decision.

Expiration is fail-closed and evaluated from canonical validity conditions. Supersession is explicit, preserves both records, and does not rewrite history. Revocation prevents new authority from being derived and triggers policy-defined handling of unstarted or in-flight warrants. It does not erase that the approval once existed or that earlier actions occurred under it.

Material changes always create a new subject identity. They never silently update an approved subject. Reapproval is required even when the change appears favorable unless policy's canonical materiality rules establish that the approved content is unchanged.

## Warrant Relationship

A warrant is the sole bounded execution grant derived from human approval where a human gate applies. Human approval records consent; the warrant represents the daemon's current authorization decision for execution.

A warrant must bind to:

- a task, project, objective, and immutable subject or base revision;
- the effective approval records and policy decision from which it was derived;
- the executing AI, service, or other permitted principal and applicable daemon identity;
- the authorized worktree or execution environment;
- allowed commands, tools, paths, network access, and external effects;
- resource, time, concurrency, and use limits;
- required checkpoints, verification, and evidence;
- explicit prohibitions and stop conditions;
- expiration and revocation state; and
- request, causal, and audit identifiers.

Warrants are capability-like because they confer narrow authority that may be attenuated, consumed, expired, or revoked. They need not be portable bearer tokens. Possession alone is insufficient: the daemon validates the warrant, principal binding, subject, approvals, policy, and current canonical state before each governed boundary.

Approval revocation, expiration, supersession, subject invalidation, policy change, or delegation invalidation must prevent issuance of new warrants and must trigger policy-defined reevaluation of active warrants. A warrant cannot broaden its source approvals or create new approval authority.

## Security Considerations

- A signature proves control of a signing credential over exact bytes. It does not prove comprehension, present authority, identity truth, or current approval effectiveness.
- Canonical serialization and human-readable rendering must be deterministic and bound together to prevent semantic substitution.
- Credentials and signing keys require least-privilege storage, rotation, revocation, recovery, algorithm agility, and protection from interfaces and agents.
- Stable request IDs, idempotent command handling, nonces where appropriate, and canonical use records prevent replay and duplicate approval or warrant issuance.
- Approval and warrant validation must fail closed on unknown principal type, missing subject content, stale version, invalid delegation, unavailable revocation state, or ambiguous recovery state.
- Multi-human policies must preserve individual attribution, dissent, threshold calculations, and the exact common subject approved.
- Interface, daemon, and executing-principal identities must be retained together so accountability cannot collapse into a shared account.
- Service-account credentials must be scoped, rotatable, revocable, and non-transferable where practical.
- Restore and cloning procedures must preserve evidence while preventing restored credentials or consumed warrants from becoming reusable authority on an unintended host.
- Personally identifying and credential data must be minimized; opaque IDs and evidence references should be preferred over duplicating sensitive attributes.
- External effects remain subject to ADR-001's durable intent, attempt, observed-outcome, and compensation model. Neither an approval nor a warrant proves that an external effect occurred exactly once.

## Local-First Behavior

Familiar must support enrollment, authentication, approval, signature verification where used, authorization, revocation, warrant issuance, execution, audit, backup, restore, and recovery without network connectivity.

A local installation maintains canonical principal records and replaceable local authentication bindings. It may rely on operating-system or host-backed mechanisms as authentication evidence, but local machine access alone is not human approval. Network identity providers, remote membership services, cloud key stores, and external timestamp authorities are optional integrations, never availability dependencies for local operation.

The local daemon remains the single writer and current authorization authority. Local clocks may contribute to expiration but cannot alone establish causality, replay protection, or identity; canonical sequence, version, and state relationships must also be used.

## Future Team Behavior

Team operation may add organizational ownership, multiple human principals, role and membership sources, threshold approvals, remote authentication, distributed clients, and federated credential verification. These capabilities must use the same stable principal IDs, typed identities, immutable approval subjects, approval lifecycle, current-state authorization, delegation semantics, and approval-to-warrant separation used locally.

External identity-provider subjects remain replaceable bindings. Organizational roles determine eligibility but do not themselves constitute approval. Multiple-human approval must preserve each human's explicit decision against identical canonical subject content. Membership or role changes must have defined effects on pending approvals, effective approvals, delegations, unissued warrants, and active warrants.

Remote or disconnected operation must define revocation freshness, clock assumptions, issuer and audience, and fail-closed behavior. Distributed validation may cache evidence, but no cache or signature may supersede current canonical authority without a separately accepted architectural decision governing distributed consistency.

## Consequences

### Benefits

- Human intent remains explicit, attributable, durable, and separate from machine action.
- Bounded warrants support least privilege and safe unattended execution.
- Current-state authorization permits immediate policy, role, approval, and delegation revocation.
- Stable principal IDs and replaceable bindings support local evolution and future team identity providers.
- Immutable approval subjects prevent approval from drifting as tasks, diffs, or policies change.
- Optional practical signing improves tamper evidence without making public-key infrastructure mandatory for basic local use.
- Typed principals make agent-neutral accountability possible across interfaces and models.

### Costs and tradeoffs

- The model requires canonical principal, binding, approval, delegation, and warrant records plus lifecycle enforcement.
- Approval presentation and canonicalization become security-critical interfaces.
- Every execution boundary must preserve the distinction between approval eligibility and warrant authority.
- Revocation and supersession dependency graphs increase policy and recovery complexity.
- Cryptographic signing introduces key custody, rotation, recovery, canonical serialization, and algorithm-lifecycle obligations where enabled.
- Future distributed use will require explicit consistency and revocation-freshness decisions; signatures alone do not solve them.
- Legacy approval-like data cannot be treated as equivalent human approval without reliable evidence.

## Verification Requirements

Conforming implementations must provide deterministic evidence that:

1. Only typed, eligible human principals can create records satisfying human approval gates.
2. AI, daemon, interface, organization, and service-account principals are rejected as human approvers.
3. Authentication, approval, authorization, delegation, warrant issuance, and execution produce distinct canonical records and audit evidence.
4. Stable principal identity survives authentication-binding rotation without conflating principals.
5. Approval subject canonicalization is deterministic and material changes produce a different subject identity.
6. Changed, expired, superseded, revoked, denied, or invalidated approvals cannot authorize new warrants.
7. Signatures, where used, verify against the exact canonical approval content and fail after any content alteration.
8. A valid signature from a currently unauthorized or revoked principal does not satisfy authorization.
9. Replayed approval and warrant requests are handled idempotently and cannot duplicate authority.
10. Delegation cannot amplify scope, cross prohibited principal types, form cycles, or survive invalid parent authority contrary to policy.
11. Warrants cannot exceed their approvals, policy, task, project, subject, principal, environment, effect, time, or resource bounds.
12. Service accounts can execute only explicitly delegated warrant authority and cannot originate human approval.
13. Daemon restart and crash recovery preserve approval and warrant state without manufacturing, duplicating, or silently consuming authority.
14. Revocation and supersession have deterministic effects on pending and active warrants.
15. Local-first identity, approval, authorization, and warrant flows work without network services.
16. Audit and consistency checks detect missing, duplicate, impossible, or mismatched principal, approval, delegation, warrant, and execution relationships under ADR-001.
17. Backup and restore preserve the consistency boundary among canonical identity state, approval evidence, warrants, audit history, and referenced artifacts.
18. Every execution outcome remains traceable to the actor, interface, daemon, project, task, approval, authorization decision, warrant, and evidence applicable to it.

## Supersession Conditions

This ADR should be reconsidered only if evidence establishes that one or more of the following is necessary:

- Familiar adopts distributed canonical state in which the local daemon can no longer evaluate current authorization before governed execution.
- Offline execution across independently authoritative nodes requires portable authority with formally bounded stale-state semantics.
- Regulatory, organizational, or threat-model requirements mandate signatures for every approval or prohibit local unsigned approval entirely.
- The stable-principal and replaceable-binding model cannot represent a required identity or accountability regime.
- Capability-like warrants cannot safely express required execution authority, attenuation, revocation, or external-effect boundaries.
- A materially different trust model eliminates a single daemon-owned command boundary.
- Deterministic evidence shows that the approval-to-warrant separation prevents required operation or introduces greater risk than a replacement model.

Any superseding decision must preserve explicit human ownership, immutable approval subjects, durable evidence, typed-principal accountability, current-state revocation semantics, local-first operation, and the distinction between human consent and machine execution unless it explicitly explains why one of those constitutional constraints must change.
