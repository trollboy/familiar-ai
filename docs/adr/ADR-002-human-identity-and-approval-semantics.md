# ADR-002 Decision Preparation: Human Identity and Approval Semantics

## Status

Proposed decision preparation. No option has been selected.

This document defines the identity and approval questions that must be resolved before Familiar can treat human approval as durable authority. It is not an implementation plan and does not authorize execution.

## Decision That Must Be Made

Familiar must decide how principals are identified, authenticated, authorized, and held accountable, and how a human approval becomes durable, scoped authority without being confused with authentication, delegation, execution, or an agent assertion.

The selected model must support:

- A single human operating locally and offline.
- Multiple interfaces representing the same or different principals.
- Replaceable AI agents and model providers.
- A daemon that acts as an enforcement point but cannot grant itself human authority.
- Project-scoped policy and ownership.
- Durable approval evidence bound to exact scope and state.
- Replay resistance, expiration, supersession, and revocation.
- Multiple humans and possible future organizational ownership.
- Warrants for bounded unattended execution.
- The canonical state and first-class material-action audit semantics accepted by ADR-001.

The minimum approval models to evaluate are:

- **Option A:** Simple user approval flags.
- **Option B:** Capability-based approvals.
- **Option C:** Explicit signed approval records with durable identities.

The decision may ultimately define a constrained composition, but each model must first be evaluated independently so that its authority and failure modes remain visible.

## Governing Constraints

### Philosophy constraints

- Humans own architecture.
- AI may recommend, critique, propose, and challenge, but may not silently redefine architecture.
- Human approval gates and explicit decision records are engineering invariants.
- Coding agents are replaceable workers, not architectural authorities.
- No single model may both perform work and declare it correct.
- The engineer remains responsible.
- Hidden assumptions, silent drift, and unverifiable success are defects.

### Target architecture constraints

- The central daemon is the sole mutable authority for Familiar operational state.
- Clients are untrusted callers and receive least privilege.
- Agents cannot expand their own authority or waive gates.
- Approvals are explicit domain state, not conversational implication.
- Approvals identify the human, exact scope, revision or artifact, timestamp, and expiration where applicable.
- Broader or later action requires new approval.
- Architecture, policy, privilege expansion, publication, destructive action, risk acceptance, and changes to Familiar policy require configured human gates.
- Project data and authority remain isolated unless explicitly authorized.
- Canonical concepts remain independent of editor, agent, model, and provider identity.

### ADR-001 constraints

- Transactional SQLite tables are authoritative for current operational state.
- Append-oriented audit events are authoritative evidence for defined material actions.
- Approval creation, supersession, revocation, expiration handling, delegation, use, denial, and attempted scope expansion are material actions.
- All canonical mutations pass through daemon-owned command handlers.
- Commands have stable request IDs, idempotency semantics, versions where concurrency is possible, and attributable audit evidence.
- Legacy approval-like records cannot be promoted into historical approvals through fabricated events.
- External effects require durable intent, attempt, observed outcome, and compensation records.

## Principal and Identity Model

Identity answers "which durable principal is this record about?" It does not by itself prove who is currently present, grant authority, or constitute approval.

Every canonical principal identity must have:

- A stable, opaque Familiar identifier that is not a display name.
- A principal type.
- Lifecycle state, including active, disabled, superseded, or otherwise unavailable.
- Provenance describing how the identity was established.
- Zero or more external identity bindings or authentication methods.
- Human-readable labels that may change without changing identity.
- Project or organizational relationships where applicable.
- Audit history for material lifecycle and binding changes.

### What is a human identity?

A human identity is a durable canonical principal representing a natural person who can own decisions, grant approvals within their authority, accept risk, revoke prior grants, and remain accountable across interfaces and sessions.

A human identity is not equivalent to:

- a username string;
- a Unix UID or macOS account by itself;
- a browser session;
- an MCP client connection;
- an API key;
- a signing key;
- a Git author name or email;
- a model's claim about what the human said; or
- an operating-system process running on the human's machine.

Those may authenticate or provide evidence about a human identity, but they are bindings, credentials, or assertions rather than the canonical person.

Human identity must remain stable across credential rotation and interface changes. A human can have multiple authentication methods, and one authentication method must not silently represent multiple humans unless explicitly modeled as a shared or organizational account with reduced approval semantics.

### What is an AI identity?

An AI identity is a durable or execution-scoped non-human principal representing a specific agent instance, agent installation, model invocation, or provider-backed worker acting in an assigned role.

The identity must distinguish, as required by policy:

- agent product or adapter;
- agent instance or execution attempt;
- provider and model identity;
- assigned role, such as implementer or reviewer;
- parent task, warrant, and daemon session;
- capability and credential context; and
- whether the identity is local, remote, ephemeral, or provider-asserted.

An AI identity may request actions, produce code, findings, reviews, summaries, or evidence, and execute within a warrant. It cannot be treated as a human approver. Model self-identification is not authentication. The daemon or trusted adapter establishes the canonical AI identity and records provider assertions as provenance, not as independent authority.

### What is a daemon identity?

A daemon identity is the durable identity of a Familiar installation or daemon authority instance that receives commands, enforces policy, owns the canonical writer lease, and emits daemon-attributable material-action evidence.

It must distinguish:

- the durable Familiar installation;
- a particular daemon process or boot session;
- the host and operating-system context;
- the software version; and
- the writer-ownership epoch.

The daemon identity authenticates the enforcement point and attributes automated actions. It is not a human identity and cannot originate human approval. When the daemon acts on a human approval or warrant, evidence must retain both daemon/execution identity and the governing human authority.

### What is an interface identity?

An interface identity represents the client or adapter through which a command entered Familiar: MCP, CLI, local socket, loopback HTTP, dashboard, tray, plugin, maintenance tool, background worker, or future integration.

Interface identity answers "through which trusted or untrusted boundary did this request arrive?" It does not answer "which human approved this?"

An interface identity may be bound to:

- a client installation;
- a process or session;
- an authenticated local credential;
- an editor integration;
- an agent adapter; or
- a daemon-internal worker.

Audit evidence must retain interface identity independently from actor identity. A request from the dashboard authenticated as a human still has both a human actor and a dashboard interface. A background worker may have a daemon actor and worker interface without any human currently present.

### What is a project identity?

A project identity is the stable canonical identity of a repository under Familiar stewardship. It scopes policy, decisions, tasks, approvals, warrants, evidence, agents, and access.

Project identity is not merely an absolute filesystem path, mutable repository name, branch, remote URL, or current checkout. It may be bound to canonical repository identity, repository-relative state, remotes, worktrees, and host locations, but it persists across ordinary moves and interface sessions.

A project is not necessarily a principal. It may be an authorization scope and an owned resource. If future organizational operation requires a project to own service credentials or policy, that ownership must be modeled through a human or organizational principal rather than implying that the project can approve actions itself.

## Authentication, Authorization, Approval, Delegation, Execution, and Accountability

These concepts must remain separate in canonical state and audit evidence.

### Authentication

Authentication establishes evidence that a caller, process, key, operating-system session, or provider assertion corresponds to a claimed principal or interface.

Authentication answers: **Who or what is making this request, and how was that claim established?**

Authentication does not grant permission and does not imply informed human consent to a particular action.

### Authorization

Authorization is the deterministic policy decision that an authenticated principal may perform or request a class of action within a scope under current state and policy.

Authorization answers: **Is this principal allowed to perform or request this operation here and now?**

A human may be authorized to approve architectural changes without having approved a particular change. An agent may be authorized to request execution without being authorized to approve it.

### Approval

Approval is an explicit, durable human decision accepting a precisely identified proposal, authority grant, risk, or transition within a bounded scope.

Approval answers: **Did an authorized human knowingly consent to this exact action, artifact, revision, risk, or authority under these conditions?**

Approval must bind to the approved subject and relevant version or content identity. It is not inferred from authentication, authorization, silence, prior similar approval, chat phrasing, or agent-generated summaries.

### Delegation

Delegation is an explicit grant by one principal allowing another principal to exercise some subset of the grantor's delegable authority under specified constraints.

Delegation answers: **May this principal act within authority derived from another principal, and may it delegate further?**

Delegation is not approval of every future action within the delegated scope. Delegation chains must be bounded, attributable, revocable, and policy-limited. Human-only architectural authority may be non-delegable to AI or service principals.

### Execution

Execution is the actual attempt to perform an operation under authorization, approval, delegation, and warrant constraints.

Execution answers: **What attempted or completed action occurred?**

Execution does not retroactively establish approval. A successfully executed operation may still have been unauthorized or unapproved and must remain an accountable defect.

### Accountability

Accountability is the durable ability to attribute requests, decisions, grants, actions, failures, and outcomes to the relevant human, AI, daemon, interface, project, authority, and causal chain.

Accountability answers: **Who decided, who authorized, who executed, through what interface, under which authority, and what happened?**

Accountability cannot be reduced to a single actor field. Human approver, delegated principal, executing AI, enforcing daemon, originating interface, and affected project may all differ.

## Common Approval Semantics

Regardless of the chosen approval model, an approval must be a canonical, project-scoped record rather than a mutable UI property or conversational assertion.

### Approval subject

An approval binds to one precisely identified subject, such as:

- an architectural decision version;
- a task objective, scope, base revision, and acceptance criteria;
- a context or warrant proposal;
- a specific source revision, diff, artifact, verification bundle, finding disposition, or acceptance record;
- an external-effect intent;
- a defined policy change; or
- a bounded delegation.

If the subject changes materially, the approval does not follow automatically.

### Approval evidence

Approval evidence must retain, as applicable:

- approval ID and version;
- human approver identity;
- authentication evidence or method used when approving;
- interface and daemon identities;
- project and organizational scope;
- exact subject type, identity, version, revision, or content hash;
- authority under which the human was allowed to approve;
- explicit decision, such as approved, denied, or risk accepted;
- conditions, constraints, and exclusions;
- issuance and effective timestamps;
- expiration or no-expiration policy;
- supersession and revocation references;
- delegation chain where permitted;
- stable request and causal identifiers; and
- the canonical audit event required by ADR-001.

Evidence must permit later verification that the subject considered by the human is the subject acted upon. A UI display or prose summary may support the human decision but cannot replace the canonical subject identity.

## Approval Lifecycle

The lifecycle must distinguish at least:

1. **Proposed:** An approval request exists but grants no authority.
2. **Approved or denied:** An authorized human has explicitly decided the exact proposal.
3. **Effective:** All activation conditions are satisfied. Approval may be approved but not yet effective.
4. **Consumed or partially consumed:** A one-time or bounded approval has authorized the permitted action or portion.
5. **Expired:** Its validity period ended. Expiration does not erase prior valid use.
6. **Superseded:** A newer approval or denial replaces it for future decisions under explicit precedence rules.
7. **Revoked:** Future use is prohibited by an authorized revocation. Revocation cannot undo actions already performed.
8. **Completed or closed:** The approved subject reached a terminal state and no further use is allowed.

Lifecycle transitions are material actions under ADR-001. Approval state uses versions where concurrent change is possible and stable request IDs for idempotency.

### Approval expiration

Expiration limits future use based on a declared time, task state, revision change, warrant state, number of uses, or other deterministic condition.

The decision must define:

- whether approvals default to expiration;
- which approvals may be non-expiring;
- which clock and monotonic evidence are trusted;
- what happens when a daemon is offline across expiration;
- whether approval must still be valid at intent, attempt, and external effect; and
- whether a long-running execution may continue after expiration.

Expiration never erases evidence that approval existed or was used.

### Approval supersession

Supersession replaces an approval's future applicability with an explicitly related record. The superseding record must identify what it replaces and whether already-issued warrants or in-flight executions remain valid.

Changing a proposal normally requires a new approval rather than mutation of the approved record. Supersession preserves both records and their causal relationship.

### Approval revocation

Revocation is an explicit material action by a principal authorized to revoke the approval. It must define its effective point and its consequences for pending commands, unconsumed warrants, in-flight execution, external-effect intents, and already-completed actions.

Revocation is prospective unless a separate compensation or rollback action is authorized. It does not rewrite history or convert earlier authorized execution into unauthorized execution retroactively.

### Nested approvals

Nested approvals occur when one approval is a prerequisite for another, such as organizational policy approval followed by project approval, or warrant approval followed by external-effect approval.

The model must define:

- whether all parents must remain effective;
- whether a child snapshots or dynamically depends on parent authority;
- how parent expiration, supersession, or revocation affects children;
- maximum depth and cycle prevention;
- whether approval composition is conjunctive, alternative, threshold-based, or ordered; and
- how a human sees the complete authority chain before approving.

Nested approvals cannot conceal expanded authority. The effective scope is never broader than the intersection permitted by all governing records and policy.

## Option A — Simple User Approval Flags

### Model

Canonical domain records contain simple approval state such as approved/not approved, optionally with approver identity, timestamp, and a small set of status fields. Approval is evaluated by reading the current flag on the subject record.

### Security

Security depends heavily on daemon-only mutation and authorization of the flag transition. A boolean or enum cannot by itself bind consent to a specific artifact, scope, revision, authentication event, or authority chain. Stronger metadata can reduce this weakness but moves the option toward an explicit approval-record model.

Compromise of a mutation-capable human session can approve any subject permitted by current authorization. Shared local accounts make attribution weak.

### Ergonomics

The model is easy to understand and supports low-friction local use. UI presentation and queries are simple. It can become misleading when one flag must represent conditional, partial, multi-human, expiring, or nested approval.

### Offline operation

Strong for a single local human. Approval can be recorded entirely in SQLite without network access. Authentication may rely on the local operating-system session or another locally available method.

### Auditability

ADR-001 audit events can record each flag transition. Current approval state is easy to query, but the subject and conditions must be preserved elsewhere to prove what was approved. Mutating the subject after approval can invalidate the meaning of the flag unless versions or immutable subject references are required.

### Replay protection

Stable request IDs prevent duplicate flag transitions, but the approval itself may be replayed against changed state unless bound to subject version, revision, and use semantics. A reusable `approved=true` flag is naturally prone to over-reuse.

### Revocation

Revocation is a transition to revoked/not approved and can be audited. The model needs separate history to distinguish never approved, denied, expired, superseded, consumed, and revoked; otherwise distinct accountability meanings collapse.

### Delegation

Poor fit. A flag can record that delegation is approved, but it cannot naturally express scope attenuation, delegation chains, non-delegable authority, maximum depth, or downstream revocation without additional records.

### Multiple humans

Limited. One approver field loses multiple decisions. A separate join table can support multiple humans, thresholds, and dissent, but that becomes a distinct approval-record system rather than a simple flag.

### Multiple agents

Agents can inspect the flag through core queries, but the flag does not specify which agent, role, attempt, or capability may use it. Binding those dimensions adds compound scope metadata or separate warrant records.

### Nested approvals

Weak. Multiple flags can represent prerequisites, but precedence, parent-child dependency, cycle prevention, and revocation propagation become application-specific logic distributed across domain records.

### Warrant compatibility

Adequate only if warrants independently contain complete scope, authority, use, expiration, and subject bindings. The flag then states that a warrant proposal is approved but carries little approval semantics itself.

### Crash recovery

Straightforward for committed flag state. Crashes between approval and warrant creation require deterministic recovery and request idempotency. If approval consumption is not separately recorded atomically, a one-use approval may be reused after restart.

### Migration complexity

Low. Additive fields or a small approval table fit the current relational schema. Legacy records can remain unapproved or pre-approval-boundary. Expanding later to richer records may require interpreting ambiguous flags conservatively.

### Future distributed operation

Weak without substantial extension. Local booleans do not prove which human approved remotely, resist cross-device replay, support key rotation, or reconcile concurrent approvals and revocations. Central-server serialization can retain the model but places all trust in server authentication and availability.

### Failure modes and irreversible consequences

- A subject changes after approval while the flag remains true.
- One approval is unintentionally reused for multiple executions.
- A shared local account makes human accountability impossible.
- Denial, expiration, revocation, and absence are collapsed.
- A later migration cannot reconstruct scope or conditions never recorded.
- UI convenience normalizes broad, persistent approval.

## Option B — Capability-Based Approvals

### Model

A human approval issues or activates a bounded capability representing authority to perform a specified action against a specified scope under constraints. Possession and validation of the capability permits use subject to policy. Capabilities may be opaque references to canonical records or self-describing tokens backed by canonical state.

Capabilities must distinguish authority from the human approval evidence that created them. A capability is an exercisable grant; it is not necessarily the complete record of informed human consent.

### Security

Capabilities can express least privilege, attenuation, scope, allowed operations, expiration, use count, agent binding, and non-transferability. Bearer capabilities are dangerous if copied or leaked. Proof-of-possession or daemon-held opaque references reduce theft risk but add credential and lifecycle machinery.

Validation must include canonical current state so revoked or superseded capabilities do not remain valid merely because their token is structurally valid.

### Ergonomics

Good when approval maps directly to a bounded action or warrant. Humans can approve a precise capability proposal. Poorly designed capability displays can be incomprehensible, especially with compound scopes, nested grants, or implicit attenuation.

### Offline operation

Opaque capabilities stored and validated by the local daemon work offline. Self-contained signed capabilities also work offline if keys and revocation state are locally available. Cross-device offline use creates reconciliation and revocation ambiguity.

### Auditability

Capability issuance, attenuation, transfer where allowed, use, denial, expiration, revocation, and consumption can be material audit events. The audit must preserve the originating human approval and not merely the capability identifier.

### Replay protection

Capabilities can include nonce, use count, request binding, expiration, task/revision scope, and consumption state. One-time consumption must be atomic with the governed command. Self-contained tokens without canonical consumption state are vulnerable to replay until expiration.

### Revocation

Opaque canonical capabilities are straightforward to revoke centrally. Self-contained capabilities require online revocation checks, short lifetimes, revocation lists, key rotation, or acceptance of delayed revocation. Parent revocation must have defined effects on attenuated children.

### Delegation

Strong fit if explicit attenuation is supported. A delegating principal can grant a strict subset of operations, scope, duration, and use count. The system must prevent amplification, cycles, hidden transitive authority, and delegation to prohibited principal types.

### Multiple humans

Threshold or conjunctive approval can issue a capability only after all required human decisions exist. A capability may hide individual dissent unless the underlying approval records remain separately inspectable.

### Multiple agents

Capabilities can bind to an AI identity, role, task, attempt, daemon epoch, or adapter. Agent replacement requires either a new capability or a deliberately transferable role capability. Transferability expands risk.

### Nested approvals

Capabilities naturally support derivation and attenuation, but approval nesting and capability delegation are not the same. Organizational, project, and effect-specific human approvals may jointly authorize capability issuance. Parent-child graphs require cycle detection, validity propagation, and understandable visualization.

### Warrant compatibility

Strong. A warrant can be modeled as or backed by a capability authorizing bounded execution. The warrant still needs richer execution, verification, stop, and external-effect semantics than a generic capability.

### Crash recovery

Canonical capability state and atomic consumption support deterministic restart. Ambiguity remains when a capability authorizes an external effect and the daemon crashes after effect execution but before consumption/outcome recording. ADR-001 external-effect recovery still applies.

### Migration complexity

Moderate. New capability, grant, derivation, consumption, and revocation records are required. Existing approval-like data cannot safely become capabilities without new explicit human approval. Service and agent bindings need stable principal identities.

### Future distributed operation

Potentially strong. Capabilities can cross service boundaries if validation, audience, issuer, key, revocation, clock, and replay semantics are defined. Distributed validation substantially increases security and operational complexity.

### Failure modes and irreversible consequences

- A leaked bearer capability grants authority until consumed, expired, or revoked.
- Attenuation logic accidentally amplifies scope.
- Human approval evidence becomes obscured behind a token.
- Nested capability graphs become impossible for humans to reason about.
- Self-contained offline tokens remain usable after revocation.
- Capability formats and scope semantics become long-lived compatibility contracts.

## Option C — Explicit Signed Approval Records with Durable Identities

### Model

A human creates an explicit canonical approval record bound to durable human identity, exact subject identity and version, scope, conditions, and lifecycle. The approval includes a cryptographic signature or equivalent durable proof produced by a credential bound to the human identity. Canonical state tracks current validity, supersession, revocation, and use.

The signature proves possession of an approval credential over exact approval content. It does not prove that the human understood the content, had organizational authority, or that the approved action remains safe at execution time.

### Security

Signatures provide tamper evidence and durable proof independent of the interface that transported the approval. Security depends on key generation, protection, binding, rotation, recovery, revocation, algorithm agility, and trustworthy presentation of the signed subject.

A compromised signing key can create apparently valid approvals. A valid signature from an unauthorized human must still fail policy. Signing prose summaries rather than canonical subject hashes creates semantic substitution risk.

### Ergonomics

Potentially higher friction. Secure key provisioning, confirmation, recovery, and explicit signing can burden a single local user. Host-backed signing and well-designed canonical previews can reduce friction. Invisible automatic signing weakens the meaning of explicit approval.

### Offline operation

Strong if keys and identity bindings are available locally. Verification is deterministic and offline. Revocation and organizational membership may be stale when disconnected unless local canonical state is authoritative for the operation.

### Auditability

Strong. The signed record preserves exact approval content, durable identity, credential binding, and time/context evidence. Audit events record proposal, signing, validation, use, denial, supersession, and revocation. Signature validity does not replace audit of use.

### Replay protection

Approval content can bind nonce, project, task, subject hash, command class, validity interval, audience, daemon identity, and allowed use count. Canonical consumption and request idempotency remain necessary; a signature alone does not prevent replay.

### Revocation

Canonical revocation is straightforward while all validation passes through the daemon. Offline or distributed validators require revocation state, short lifetimes, status proofs, or accepted delay. Revoking a key affects all approvals signed by it unless approval validity is evaluated at signing time under a preserved trust policy.

### Delegation

Signed delegation records can provide explicit chains and scope. Every link requires authority, attenuation, validity, principal-type restrictions, and revocation semantics. Long chains increase validation and human-comprehension cost.

### Multiple humans

Strong. Separate signed records can support unanimous, threshold, role-based, or ordered approval without losing individual attribution or dissent. The policy must define whether signatures cover identical canonical content and how changes invalidate collected approvals.

### Multiple agents

Approvals can bind to a specific AI identity, role, adapter, task, or warrant proposal. Agent replacement requires explicit semantics: either the approval is agent-specific or it approves a neutral role/capability and policy assigns an agent later.

### Nested approvals

Signed records can reference parent approvals and preserve a verifiable graph. They do not simplify graph semantics; cycles, dynamic parent validity, supersession, threshold rules, and human-readable presentation remain necessary.

### Warrant compatibility

Strong. A signed approval can authorize issuance of a separate warrant bound to its exact proposal. Approval and warrant should remain distinct: approval is human consent; warrant is the daemon-enforced execution authority derived from it.

### Crash recovery

Signed approval records already committed remain verifiable after restart. Approval use and warrant issuance still require atomic canonical transitions and idempotency. A signature created client-side but not committed is not an effective approval; the client may retry with the same request and signed content.

### Migration complexity

High relative to the other options. Durable human identities, credential bindings, key lifecycle, signature envelopes, canonical serialization, algorithm/version metadata, revocation, and recovery are required. Legacy decisions cannot be retroactively signed without creating a new present-day approval.

### Future distributed operation

Strongest intrinsic portability if issuer, audience, trust roots, organizational membership, time, revocation, and canonical serialization are standardized. Distribution also magnifies key compromise, trust federation, clock, and revocation complexity.

### Failure modes and irreversible consequences

- A human signs a misleading rendering that does not match canonical content.
- Key compromise creates durable fraudulent approvals.
- Lost keys make future approval impossible or complicate identity continuity.
- Algorithm or serialization choices become long-lived verification contracts.
- Automatic signing reduces approval to authenticated button state with cryptographic decoration.
- Distributed revocation cannot reliably reach offline validators.

## Human and Organizational Ownership

### Human ownership

Human ownership means a natural person remains accountable for decisions and approvals within their authority. Familiar may enforce policy and preserve evidence, but it cannot convert daemon, AI, interface, or organizational identity into a substitute human decision where policy requires a human.

A project may have one or more human owners. Ownership grants eligibility to approve specified classes; it does not constitute standing approval of every action.

### Organizational ownership

Future team mode may introduce an organization as a canonical principal or ownership scope. Organizational policy may define roles, membership, thresholds, escalation, and revocation. An organization cannot literally express human judgment; human or explicitly permitted service principals act under organizational authority.

The model must distinguish:

- organization identity;
- human membership and role;
- authority derived from role;
- project ownership by the organization;
- individual approval evidence; and
- organizational policy satisfaction.

Changes in membership or role must define their effect on prior approvals, pending approvals, delegations, warrants, and in-flight execution.

### Service accounts

A service account is a non-human principal used for deterministic automation or integration. It may authenticate, request operations, execute permitted commands, or exercise explicitly delegated machine authority.

A service account may not satisfy a human approval gate merely because a human originally configured it. If policy permits service approval for a particular low-risk class, that is machine authorization under explicit policy, not human approval, and must be labeled accordingly.

Service credentials require rotation, revocation, least privilege, non-interactive authentication, and explicit project/audience scope. Shared service accounts weaken accountability and should retain executing daemon/interface identity.

### Local-only mode

Local-only mode must work without network identity providers, remote clocks, cloud key services, or organizational directories.

Viable local identity roots include operating-system identity, locally provisioned credentials, host-backed signing keys, or daemon-managed identity records. Each has different guarantees. Local possession of the machine is not automatically informed human approval.

Local-only mode must define:

- initial human identity enrollment;
- recovery after credential or key loss;
- protection against another local process or user;
- whether multiple operating-system users share a Familiar database;
- daemon/client authentication;
- offline expiration and revocation; and
- backup/restore of identity and approval evidence.

### Future team mode

Future team mode introduces remote membership, concurrent humans, organizational ownership, distributed clients, clock and revocation propagation, and possibly replicated approval evidence.

The initial decision must avoid making team mode impossible, but it need not implement distributed identity now. Stable opaque principal IDs, typed principals, explicit credential bindings, project-scoped authority, versioned approval subjects, and preserved evidence are prerequisites under every option.

Future team operation must not reinterpret a local username or operating-system UID as a globally unique human identity.

## Cross-Option Requirements

Regardless of approval model:

- AI, daemon, interface, project, organization, service account, and human identities remain typed and distinguishable.
- Only an eligible human identity satisfies a human-required approval gate.
- Authentication never implies approval.
- Authorization never implies approval of a specific subject.
- Approval never performs execution by itself.
- Execution records retain the governing approval, warrant, policy, actor, daemon, interface, and project identities.
- Approval subjects are immutable or versioned; material changes require new approval.
- Approval state, use, revocation, supersession, expiration, denial, and failed use are governed material actions under ADR-001.
- A daemon cannot manufacture human approval, including during recovery or migration.
- An AI-generated summary cannot be the canonical object approved unless policy explicitly defines that exact immutable artifact as the subject and source evidence remains accessible.
- Shared accounts and service identities never silently become individual human identities.
- Approval evidence is preserved while referenced by decisions, findings, verification, warrants, handoffs, or acceptance.
- Clock time alone does not establish identity, causality, validity, or replay protection.

## Open Questions

1. What establishes the first trusted human identity in a new local installation?
2. Is operating-system user identity sufficient authentication for any approval class, and which classes require stronger proof?
3. Can one local installation support multiple human identities under the same operating-system account?
4. Which approval classes require explicit reauthentication or proof of presence?
5. Must architecture, policy, external-effect, destructive-action, and risk-acceptance approvals use the same model?
6. Are simple approval flags acceptable for any low-risk workflow, or would mixed models create ambiguous semantics?
7. Is a capability the approval itself, an authority derived from approval, or only a warrant mechanism?
8. If capabilities are used, are they opaque daemon references, bearer tokens, or proof-of-possession credentials?
9. If signatures are used, which keys, canonical serialization, algorithms, host stores, rotation, recovery, and revocation rules apply?
10. What exact content must the human see before approving, and how is that presentation bound to canonical subject data?
11. Which approvals expire by time, task state, revision change, use count, or warrant lifecycle?
12. At which external-effect stage must approval still be effective: intent, attempt, observed outcome, compensation, or all applicable stages?
13. Does revocation stop an in-flight execution, prevent only future effects, or trigger a policy-specific stop condition?
14. How do parent revocation and supersession affect nested child approvals and issued warrants?
15. Are delegation chains permitted, which authority is non-delegable, and may any chain include AI or service principals?
16. What maximum nesting depth and approval graph forms are understandable and supportable?
17. How are multiple humans combined: unanimous, threshold, ordered, role-based, or policy-dependent?
18. How is dissent represented when a threshold approval succeeds?
19. Does prior approval remain valid after a human leaves an organization or loses a role?
20. Can an approval bind to a neutral agent role, or must it identify the exact agent instance?
21. How are ephemeral AI/model invocations linked to durable agent identities without making provider assertions authoritative?
22. How are daemon installation identity, process identity, writer epoch, and restored identity distinguished?
23. How are identity and approval records restored without enabling replay on a cloned host?
24. What identity evidence may be retained without exposing personal or credential data?
25. What is the fail-closed behavior when authentication, revocation, organization membership, or clock evidence is unavailable?
26. Which service-account actions are permitted without contemporaneous human approval?
27. What compatibility promises are required for future team mode and distributed validation?

## Decision Matrix

The matrix is qualitative and does not rank or recommend an option.

| Criterion | Option A: Simple flags | Option B: Capability-based | Option C: Signed durable approvals |
|---|---|---|---|
| Canonical representation | State on or adjacent to approved subject | Canonical grant/capability and its lifecycle | Canonical signed approval record and lifecycle |
| Human identity strength | Depends on approver field and authentication | Depends on issuance evidence and principal binding | Durable identity plus credential/signature binding |
| Subject binding | Weak unless version/hash fields are added | Strong when capability scope is explicit | Strong when canonical subject content is signed |
| Security model | Central daemon and authorization controls | Least-privilege grant possession and validation | Signature integrity plus daemon policy validation |
| Local ergonomics | Simplest | Moderate | Potentially highest friction |
| Offline operation | Strong | Strong for local opaque grants; conditional for distributed tokens | Strong with locally available keys and trust state |
| Auditability | Transition history; subject details may be external | Strong grant/use history if approval origin is preserved | Strong explicit content and signer evidence |
| Replay protection | Request IDs and separate consumption state required | Natural nonce/use constraints, but canonical consumption still required | Signed nonce/scope plus canonical consumption required |
| Revocation | Simple current-state transition | Straightforward for opaque grants; harder for self-contained tokens | Canonical revocation; distributed/offline propagation remains hard |
| Delegation | Poor without additional graph records | Strong attenuation model; theft/amplification risks | Explicit signed chains; validation and comprehension costs |
| Multiple humans | Awkward without separate records | Threshold issuance possible | Separate attributable approvals naturally supported |
| Multiple agents | Requires added binding fields | Natural agent/role/audience scope | Exact agent or neutral role can be signed |
| Nested approvals | Application-specific flags and precedence | Grant derivation plus separate approval composition | Explicit reference graph with signed nodes |
| Warrant compatibility | Warrant must carry most semantics | Strong; warrant may be capability-backed | Strong; signed approval authorizes distinct warrant issuance |
| Crash recovery | Simple state; consumption ambiguity must be modeled | Strong with canonical issue/use/consume records | Strong committed records; use and issuance still transactional |
| Migration from current state | Low | Moderate | High |
| Key-management burden | None beyond authentication method | Conditional: low for opaque grants, high for signed/self-contained tokens | High and unavoidable |
| Risk of authority leakage | Broad persistent flag or mutable subject | Leaked or transferable capability | Compromised signing credential or misleading signed rendering |
| Human comprehensibility | High for simple cases, low for compound policy | Depends on scope visualization | Depends on canonical approval presentation and key UX |
| Future team mode | Requires substantial extension | Potentially strong with issuer/audience/revocation design | Strong attribution potential with federation and revocation complexity |
| Future distributed mode | Weak unless centralized validation remains mandatory | Strongest for portable capabilities, with added security burden | Strong portable proof, but membership/revocation/time remain external |
| Long-term compatibility burden | Approval-state semantics | Capability format, attenuation, audience, and validation | Signature envelope, serialization, identity binding, and algorithm agility |
| Primary irreversible risk | Missing approval scope/history cannot be reconstructed | Leaked or overbroad grants and entrenched capability semantics | Durable fraudulent approvals after key compromise and permanent signature contracts |
