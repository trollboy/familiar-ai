# Familiar Canonical Query Model

**Status:** Normative  
**Date:** 2026-08-03

This document defines the canonical read model for Familiar. It is the only contract permitted for reading canonical Familiar state. It applies to external callers, presentation surfaces, background workers, and internal subsystems that consume canonical state outside the private evaluation boundary of an already admitted command.

The contract is transport-neutral. Delivery mechanisms may frame, stream, or render queries differently, but they may not change query semantics, authorization, consistency, freshness, status, ordering, or field authority.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described by RFC 2119.

# Goals

The canonical query model exists to:

- provide one protocol-neutral read boundary for canonical Familiar state;
- keep every read scoped to a stable project and accountable principal and interface;
- make read authorization deterministic and current-state aware;
- let callers state their required consistency and freshness explicitly;
- distinguish canonical fields from derived fields in every mixed result;
- preserve provenance, uncertainty, and source authority;
- distinguish unknown, unavailable, unauthorized, stale, partial, and empty results;
- support bounded pagination and streaming without changing result meaning;
- guarantee deterministic ordering for reproducible reads;
- permit safe caching without making a cache authoritative;
- prevent reads from causing hidden canonical mutations or external effects;
- behave consistently across all present and future delivery mechanisms; and
- evolve without coupling canonical semantics to an editor, agent, model, provider, or presentation.

# Non-goals

The query model does not:

- define mutation operations;
- replace the canonical command model;
- define persistence structures or implementation types;
- authorize execution or convey warrant authority;
- create human approval, delegation, policy waiver, or acceptance;
- trigger implicit indexing, reconciliation, verification, review, refresh, or repair;
- make derived summaries, indexes, rankings, caches, or model output canonical;
- make audit evidence the source for reconstructing current operational state;
- guarantee that an external source remains unchanged after a response;
- provide unbounded enumeration or unrestricted cross-project search;
- turn access telemetry into a semantic query side effect;
- hide missing, stale, uncertain, or inaccessible information behind an empty result; or
- permit callers to infer unauthorized target existence from response differences.

# Query Lifecycle

A query has an immutable envelope, a bounded evaluation, and an explicit result state. Query processing is read-only with respect to canonical state.

## 1. Constructed

The caller constructs a complete query envelope with a stable `request_id`, one `project_id`, the intended target, requested consistency, pagination constraints, and versioned query semantics.

Construction does not establish authentication, authorization, target existence, freshness, or result availability.

## 2. Received

The Query Layer receives the immutable envelope through an approved caller boundary. It assigns or validates `query_id`, correlates the request, binds trusted caller context separately from caller claims, and applies structural and resource limits.

Receipt does not mean that the target exists or that the caller may observe it.

## 3. Validated

The Query Layer validates:

- required fields and supported query semantics;
- query and target scope;
- consistency and freshness requirements;
- pagination and filter syntax;
- bounded result and field selection;
- project identity;
- principal and interface bindings; and
- compatibility with the supported query version.

An invalid envelope is a query error, not `Unknown`, `Unavailable`, `Unauthorized`, `Stale`, `Partial`, or `Empty`.

## 4. Authorized

The Query Layer evaluates current read authorization for the authenticated principal, interface, project, query type, target, requested fields, evidence sensitivity, and any applicable privacy or retention policy.

Authorization applies before target details are disclosed. Field-level restrictions may reduce a response only when the query contract explicitly permits field-level authorization and can report the reduction without leaking protected facts. Otherwise, the whole result is `Unauthorized`.

## 5. Consistency Bound

The Query Layer establishes the canonical read boundary required by `consistency`. It identifies the canonical snapshot, causal lower bound, or permitted staleness bound used for evaluation.

All canonical fields in one result or page are evaluated against the same declared canonical boundary unless the query contract explicitly supports independently identified subresults. Derived fields identify their own source and validation boundaries.

## 6. Evaluated

The Query Layer resolves canonical records, authorized evidence, and permitted derived data. It applies filters, deterministic ordering, pagination, provenance, freshness, and uncertainty rules.

Evaluation does not mutate canonical state or trigger implicit generation. If required information is missing or invalid, the result reports the applicable explicit state.

## 7. Classified

The Query Layer assigns an overall result state and, where the contract permits mixed fields or items, field- or item-level states. `Unknown`, `Unavailable`, `Unauthorized`, `Stale`, `Partial`, and `Empty` retain the distinct meanings defined below.

## 8. Delivered

The response contains the query identity, request correlation, project scope, result state, consistency boundary, ordering, pagination, freshness, provenance, uncertainty, and data permitted by authorization.

Presentation may differ by caller capability. Meaning, omission, ordering, authority labels, and states may not.

## 9. Retried or Continued

A lost response may be requested again with the same `request_id` and the same semantic query fingerprint. A repeated read may return a newer result only when the declared consistency and pagination semantics permit reevaluation. Snapshot-bound continuation returns data from the same logical snapshot or fails explicitly if that snapshot can no longer be honored.

# Query Envelope

Every query contains the following required fields. Together they form the immutable query envelope.

| Field | Meaning | Normative constraints |
|---|---|---|
| `query_id` | Stable identity of this admitted query evaluation. | Unique within Familiar's canonical scope. It binds the evaluation, response, pages, and stream chunks. A caller-proposed value is not trusted until admitted. |
| `request_id` | Stable identity of the caller's logical read request. | Reused only for an exact retry or continuation defined by this contract. Different semantic content requires a new value. |
| `project_id` | Canonical project scope of the query. | Stable opaque identity. A path, name, current working context, remote location, or display label is not a substitute. |
| `principal_id` | Typed principal accountable for the read request. | Stable opaque identity resolved from current authentication bindings. It is distinct from `interface_id`. |
| `interface_id` | Identity of the interface or internal caller boundary through which the query arrived. | Provides attribution and caller-boundary context but never substitutes for principal identity or authorization. |
| `query_type` | Stable semantic name of the requested read. | Protocol-, vendor-, and presentation-neutral and mapped to one registered query contract and version. |
| `target` | Exact project-scoped subject, collection, relation, or evidence set requested. | Type-explicit, bounded, and unambiguous. Material source or revision identity is included when required by the query semantics. |
| `consistency` | Required canonical consistency, causal boundary, and freshness tolerance. | Explicit even when the query uses the contract's default. The Query Layer must either satisfy it or return a non-success state. |
| `pagination` | Requested page boundary, maximum size, cursor, and direction. | Explicitly bounded. A first-page request uses an explicit empty cursor. Pagination does not weaken authorization or consistency. |
| `metadata` | Non-domain context for compatibility, causality, privacy, field selection, and delivery behavior. | Bounded and namespaced. It cannot alter authority or core query meaning unless the query contract explicitly designates a semantic field. |

## Immutable query envelope

The envelope is immutable after receipt. Trusted observations—such as authenticated binding, daemon evaluation time, canonical boundary, authorization decision, cache use, and delivery status—are attached to the result rather than overwriting caller fields.

Changing the target, consistency, filters, ordering, field selection, page size, privacy scope, or other semantic input creates a new logical request with a new `request_id`. A continuation changes only the cursor and any explicitly permitted continuation metadata while remaining bound to the original semantic query fingerprint.

## Query semantic versioning

Each `query_type` identifies a supported semantic version through its registered name or version metadata. The version defines target meaning, filters, ordering, result fields, field authority, states, defaults, and compatibility behavior.

Existing versions retain their meanings and defaults. Adding an optional field is compatible only when its omission preserves prior semantics. Reinterpreting a field, state, default, filter, ordering rule, consistency guarantee, or provenance requirement requires a new version.

Unknown required fields, variants, or semantic versions are rejected. A caller boundary must not approximate an unsupported query using a superficially similar one.

## Target semantics

The `target` identifies one object, bounded collection, relation, or evidence scope within `project_id`. It uses stable canonical identity and, where relevant, immutable source, content, task, revision, or artifact identity.

Targets cannot rely solely on mutable labels, host-specific locations, provider sessions, process identifiers, or caller-local context. Repository paths are repository-relative and validated against project identity.

A collection target declares its domain boundary. An omitted filter means “all authorized items within this bounded target,” not “all projects” or an implicit global scope.

## Metadata semantics

Metadata may include:

- query semantic version;
- root causal or trace identifiers;
- requested field set or presentation capability;
- privacy and redaction labels;
- locale;
- cache-control preference that does not weaken `consistency`;
- caller capability declarations; and
- approved extension namespaces.

Metadata must not carry credentials, raw secrets, hidden filters, authority, approval, policy waiver, cross-project scope, or fields that change domain meaning without inclusion in the registered semantic fingerprint.

# Read Consistency

The `consistency` field declares the minimum acceptable canonical read guarantee. It contains one supported mode plus any required boundary values.

## Current canonical

Returns canonical fields from one current committed boundary established after the query is admitted and authorized. It is required for decisions about approval, authorization, warrants, leases, execution admission, revocation, acceptance, and other safety-sensitive state.

“Current” means current at the identified evaluation boundary, not permanently current after delivery.

## Causal

Returns canonical fields from a boundary that includes at least a declared prior command outcome, canonical version, or causal marker. If the service cannot reach or prove that boundary, it returns `Unavailable` rather than an older response.

Causal reads support read-after-write and multi-stage workflows without treating timestamps as ordering authority.

## Snapshot

Returns all canonical fields, pages, and stream chunks from one identified stable logical boundary. Snapshot consistency is required when page-to-page reproducibility matters. If the boundary can no longer be continued, the continuation is `Unavailable`; it does not silently restart against newer state.

## Bounded stale

Permits a canonical read older than the current boundary only when the query type explicitly allows it and the caller declares an exact maximum version lag, maximum age, or both. The result reports the actual boundary and measured lag.

Bounded-stale reads are prohibited for authority, approval effectiveness, warrant eligibility, lease validity, revocation, execution admission, destructive action, external-effect authority, findings disposition, and acceptance.

## Consistency failure

If the requested consistency cannot be proved, the response is `Unavailable` unless valid data exists but fails only the declared freshness requirement, in which case it is `Stale`. The service does not weaken the request automatically.

# Freshness Semantics

Consistency describes the canonical evaluation boundary. Freshness describes how recently canonical or derived information was observed, generated, or validated relative to its authoritative inputs.

Every response identifies, as applicable:

- canonical evaluation boundary and version;
- daemon observation time;
- authoritative source revision or content identity;
- derived artifact generation time;
- last validation time;
- generator and configuration identity;
- invalidation status;
- actual age or version lag; and
- caller-required maximum age or lag.

A recent timestamp does not prove freshness. A derived artifact is fresh only when its authoritative inputs, configuration, generator, and dependency identities still match and no invalidation condition applies.

Canonical state may be current while a derived field is stale. A response must report those conditions independently rather than assign the authority of the canonical container to its derived contents.

# Canonical and Derived Fields

Every response field is classified as one of:

- **Canonical:** Current Familiar operational state whose authority belongs to the canonical state owner.
- **Source-authoritative:** Repository or version-control content and identity read from its authoritative source.
- **Evidence:** Immutable or append-oriented evidence for an action or conclusion; authoritative as evidence, not as current state.
- **Derived:** Reconstructible summaries, indexes, rankings, aggregations, projections, caches, model output, or presentation values.

The classification is part of the query contract and cannot be inferred from field names or nesting. A derived field must not appear indistinguishable from a canonical field. Aggregating canonical records creates a derived view even when every input is canonical.

Derived fields include provenance, freshness, and uncertainty. If derived information conflicts with source or canonical state, source or canonical authority prevails and the conflict is exposed. The query does not repair or regenerate the derived artifact implicitly.

# Provenance

Provenance explains where a returned value came from and why it is eligible for use. Provenance is required for source-authoritative, evidence, and derived fields and for canonical fields whose version or causal context matters.

Provenance includes, where applicable:

- owning project;
- stable record or artifact identity;
- canonical version or source revision;
- source content hashes;
- parent records and evidence references;
- generator, tool, model, ruleset, configuration, and version;
- generation and validation times;
- dependency identities;
- authorization-aware redaction or omission; and
- invalidation or supersession relationships.

Provenance can be referenced through an immutable manifest when inline detail would exceed a bounded response. The reference itself must be authorized and stable. Missing required provenance makes a derived value `Unknown`, `Stale`, or `Unavailable` according to the facts; it does not remain silently usable.

# Uncertainty

Uncertainty is explicit, attributable, and distinct from availability and freshness. It describes limits in what Familiar can conclude from known evidence.

A field or item that carries uncertainty identifies:

- the uncertain claim or boundary;
- whether uncertainty was reported by a source, deterministic analyzer, model, reviewer, or query composition;
- the evidence supporting and contradicting the claim;
- confidence only when its scale and producer are defined;
- assumptions and unresolved alternatives; and
- what authoritative observation could resolve it.

Uncertainty never upgrades derived information to canonical truth. A low-confidence derived value is not `Unknown` when its value and provenance are known; it is a known derived claim with explicit uncertainty. When no value is recorded, the state is `Unknown`.

# Required Result States

`Unknown`, `Unavailable`, `Unauthorized`, `Stale`, `Partial`, and `Empty` are distinct normative states. They must not be represented by the same status, by a null value without classification, or by an empty collection.

## Unknown

**Meaning:** Familiar can evaluate the query and the caller is authorized, but the requested fact is not recorded, not observed, or not knowable from retained authoritative evidence.

Examples include an explicitly absent historical actor, an unresolved fact that was never observed, or a legacy record whose original provenance is not known.

**Implications:**

- The target or query domain may exist.
- No known value is asserted.
- Retrying without new evidence is not expected to change the result.
- Familiar must not fabricate, infer, or substitute a derived guess.

## Unavailable

**Meaning:** The query cannot currently be completed or its required consistency cannot be proved because a required service, source, artifact, canonical boundary, or dependency is inaccessible or failed.

**Implications:**

- A value may exist and may be known later.
- The result identifies the unavailable dependency at a disclosure-safe level.
- Retry may be appropriate under bounded caller policy.
- Unavailable must not be rendered as empty, unknown, stale, or unauthorized.

## Unauthorized

**Meaning:** The authenticated principal is not permitted to observe the requested target, fields, evidence, or scope under current policy.

**Implications:**

- The response does not reveal whether the protected target exists.
- No protected value, count, freshness, provenance, ordering, or availability detail is disclosed.
- Retry without a relevant authority change is not expected to succeed.
- Authentication failure and authorization denial may use different internal evidence while presenting a disclosure-safe unauthorized result when required.

## Stale

**Meaning:** A known value is available, but its canonical boundary, source identity, validation state, age, or version lag does not satisfy the query's declared freshness or consistency requirement.

**Implications:**

- The stale value may be returned only when the query contract and caller explicitly permit stale payload disclosure.
- The response states the actual boundary, freshness, and invalidation reason.
- Stale data cannot be used for current authorization or other safety-sensitive decisions.
- A refresh requires a separate command or scheduled operation; the query does not trigger one implicitly.

## Partial

**Meaning:** The query validly returns only a proper subset of the requested authorized result because one or more bounded components are unavailable, stale beyond tolerance, unknown, truncated by an explicit limit, or interrupted.

**Implications:**

- Returned items remain valid under the declared consistency and authority labels.
- The response identifies omitted components and gives each an applicable state or disclosure-safe reason.
- Partial is not used for ordinary pagination when a valid continuation cursor represents the complete bounded sequence.
- Partial is not success for a query contract requiring completeness.

## Empty

**Meaning:** The query completed successfully, was authorized, met its consistency and freshness requirements, and the bounded target contains zero matching items or the requested optional relation has no value.

**Implications:**

- Empty is a positive, complete result.
- It is not evidence that a protected target does not exist outside the authorized query scope.
- Empty is never used to mask an error, unavailable dependency, stale cache, unknown fact, authorization denial, filter rejection, or interrupted stream.

## State composition

The overall result uses the most informative contract-defined state without obscuring component states:

- A wholly denied query is `Unauthorized`.
- A complete query with no matches is `Empty`.
- A complete query for a single unrecorded fact is `Unknown`.
- A result whose only available value violates the required freshness boundary is `Stale`.
- A query that cannot establish its required read boundary is `Unavailable`.
- A result containing valid data plus explicitly missing components is `Partial`.

Mixed field or item states are allowed only when the query contract defines them and the response preserves each component's authority, provenance, and state.

# Pagination

Pagination is part of query semantics, not a presentation-only concern.

Every collection query defines:

- maximum and default page size;
- deterministic primary ordering and stable tie-breakers;
- cursor direction;
- snapshot or consistency boundary;
- filter and field-selection fingerprint;
- authorization scope binding; and
- cursor expiry or invalidation behavior.

A continuation cursor is opaque to callers and bound to `query_type`, version, project, principal visibility scope, target, filters, ordering, field selection, consistency boundary, and page size rules. It cannot be transferred to another project or authority context.

Items cannot be skipped or duplicated within a valid snapshot continuation. If continuation against the original boundary is impossible, the result is `Unavailable` with a reason safe to disclose. The Query Layer does not silently start a new sequence.

A next-page cursor means more items may remain; it does not make the current page `Partial`. Explicit truncation without a continuation path is `Partial` unless the query contract defines the requested limit as the complete target.

# Filtering

Filters are registered, versioned query semantics. They are applied within the authorized project scope and before pagination. Every filter defines:

- eligible fields and operators;
- normalization and comparison rules;
- handling of unknown or derived values;
- case, locale, and time semantics where relevant;
- interaction with authorization and redaction;
- deterministic ordering effects; and
- compatibility behavior.

Unknown filters or operators are rejected. Unsupported filters are not ignored. Filters cannot reveal protected counts or existence through timing, error differences, or ordering. A filter over a derived field carries that field's provenance and freshness constraints.

# Authorization

Read authorization is evaluated from current canonical identity, project, membership, role, policy, target, field sensitivity, privacy, retention, and evidence state. It is distinct from authentication, approval, delegation, execution authority, and acceptance.

Authorization is checked:

- when the query is admitted;
- when a continuation page or stream segment is produced if policy or authority may have changed;
- before following a reference to more sensitive evidence; and
- before serving cached data.

A cache entry, cursor, stream subscription, prior successful query, or local process identity never preserves authority after revocation. Authorization failure does not reveal target existence or prior visibility.

Queries used by current-state authorization must themselves use current canonical or an explicitly sufficient causal consistency. Bounded-stale reads cannot decide authority.

# Caching

Caching is permitted only as a reconstructible read optimization. A cache entry includes:

- the full semantic query fingerprint;
- project identity;
- authorization visibility scope;
- canonical boundary;
- source and dependency identities;
- derived artifact provenance;
- freshness and invalidation conditions;
- query semantic version; and
- result state and ordering metadata.

Before serving cached data, the Query Layer rechecks current authorization and proves that the requested consistency and freshness still hold. Cache age alone is not proof of validity.

Unauthorized results may be negatively cached only within a safely bounded identity and policy version scope and must not survive relevant authority changes. Protected data must never be shared through a cache key that omits project or visibility scope.

A cache miss has no semantic meaning. It does not imply `Unknown`, `Unavailable`, `Stale`, or `Empty`.

# Invalidation

Cache and derived-result invalidation occurs when any semantic dependency changes, including:

- canonical record version;
- project identity or scope;
- source revision or content hash;
- artifact generator, policy, configuration, or ruleset version;
- principal, membership, role, authorization, privacy, or retention state;
- supersession or revocation;
- field classification or query semantic version; and
- evidence availability or integrity.

Invalidation may remove a cached result or mark a derived value stale. It cannot mutate canonical domain state, fabricate replacement data, or trigger regeneration as an implicit query side effect.

When dependency validity cannot be established, the cache is not served. The response becomes `Unavailable` or `Stale` according to whether a known value exists and disclosure is permitted.

# Streaming

Streaming is incremental delivery of one query evaluation, not a separate consistency model and not a subscription to authority by possession.

Every stream:

- retains one `query_id` and semantic query fingerprint;
- declares whether it is snapshot-bound or represents explicitly versioned successive observations;
- numbers chunks deterministically;
- preserves item ordering and page boundaries;
- attaches canonical boundary, provenance, and state to each relevant chunk;
- rechecks authorization at defined continuation boundaries;
- applies backpressure and bounded buffering; and
- ends with an explicit complete, partial, unavailable, stale, unauthorized, cancelled, or interrupted terminal result.

A snapshot stream does not mix canonical boundaries. A successive-observation stream labels every change with its own boundary and must not represent itself as one snapshot.

Dropped chunks, lost ordering, revoked authority, or inability to preserve the requested boundary terminates the stream explicitly. Reconnection uses a valid cursor or begins a new query; it does not guess the missing sequence.

# Partial Results

Partial results are permitted only when the query contract declares component independence and the caller has not required all-or-nothing completeness.

A partial result contains:

- the valid returned subset;
- the canonical and source boundaries that apply;
- a deterministic inventory of omitted components;
- component states and disclosure-safe reasons;
- whether continuation or retry can recover more data;
- whether the subset is safe for the caller's declared purpose; and
- a completeness indicator that cannot be mistaken for success.

Items returned in a partial result must not be weakened merely because another component failed. Conversely, the valid subset must not be used as evidence that omitted components passed, were empty, or do not exist.

# Unavailable Results

An unavailable result identifies the failed dependency class and retry characteristics without leaking protected state. It distinguishes transient service failure, unavailable authoritative source, lost snapshot boundary, corrupt evidence, unsupported continuation, and consistency proof failure where policy permits those details.

Unavailable results contain no guessed substitute. A derived cache may be disclosed as `Stale` only when the caller explicitly permitted stale disclosure and the authorization check still succeeds.

Repeated unavailability is an operational health fact, not permission to weaken consistency automatically.

# Stale Data

Stale data remains known data with a failed freshness or validity condition. It is never relabeled current because it is the best available value.

When stale disclosure is permitted, every stale value includes:

- its authority classification;
- the boundary at which it was valid or observed;
- the required and actual freshness;
- source and dependency identity;
- invalidation reason;
- explicit prohibition on safety-sensitive use where applicable; and
- the separate action, if any, that could refresh it.

Canonical current-state fields used for authority and safety decisions cannot be served stale. Derived summaries may be served stale for diagnostic purposes only when clearly labeled and source fallback remains available.

# Deterministic Ordering

Every collection query defines a total order. The order includes a stable, unique tie-breaker and is evaluated under the query's declared consistency boundary.

Ordering rules specify:

- field precedence;
- ascending or descending direction;
- null, unknown, and unavailable placement;
- text normalization and locale behavior;
- timestamp interpretation;
- derived-score precision and tie-breaking; and
- behavior across semantic versions.

Implicit storage order, filesystem enumeration order, provider response order, map iteration order, and nondeterministic model ranking are prohibited as canonical query ordering.

When a derived relevance score participates in ordering, the generator, inputs, score version, and deterministic tie-breaker are part of provenance. Repeated evaluation against identical inputs and versions must produce the same order or the response must identify the ranking as nondeterministic and ineligible for snapshot pagination.

# Query Invariants

1. Every read of canonical Familiar state outside an admitted command's private evaluation boundary uses the Query Layer and this contract.
2. A query is read-only with respect to canonical state and external reality.
3. Every query is scoped to exactly one canonical project.
4. Every query identifies distinct principal and interface identities.
5. Every query has one immutable envelope and a bounded target.
6. Consistency and freshness requirements are explicit and never silently weakened.
7. Canonical, source-authoritative, evidence, and derived fields remain distinguishable.
8. Derived values never replace canonical or source authority.
9. Every non-canonical value carries sufficient provenance, freshness, and uncertainty.
10. Unknown, unavailable, unauthorized, stale, partial, and empty remain distinct.
11. Every collection has deterministic total ordering and bounded pagination.
12. A query cannot trigger mutation, execution, refresh, reconciliation, verification, review, or acceptance.
13. A query result is not approval, authorization for mutation, execution authority, verification, review, or acceptance.
14. Audit evidence supports explanation but does not reconstruct current state.
15. Cross-project queries are outside this contract and prohibited.

# Security Invariants

1. Every caller field is untrusted until validated against authenticated context.
2. Read authorization is evaluated against current canonical state before protected facts are disclosed.
3. Principal identity is stable and distinct from interface identity.
4. Authentication, local presence, prior access, cursor possession, cache possession, and stream possession do not imply current authorization.
5. Unauthorized responses do not disclose target existence, count, freshness, provenance, prior visibility, or dependency health.
6. Field-level authorization cannot create inference channels through omission, ordering, timing, pagination, or totals.
7. Project identity and visibility scope are included in every cache, cursor, stream, and derived-artifact boundary.
8. Secrets and protected evidence are redacted or omitted according to policy; redaction state is explicit where disclosure is permitted.
9. Query metadata cannot smuggle cross-project scope, hidden authority, credentials, or policy overrides.
10. Plugins, agents, models, presentation surfaces, and background workers receive no direct canonical read handle outside this contract.
11. Authorization is rechecked before serving cached results and at required continuation boundaries.
12. Errors do not reveal another project's existence or protected target details.

# Compatibility Rules

- Query semantics are transport-, editor-, agent-, model-, provider-, and presentation-neutral.
- All caller boundaries map the same logical read to the same `query_type`, semantic version, target, consistency, filters, ordering, states, and authority labels.
- Existing query versions retain field meaning, classification, defaults, ordering, filtering, consistency, provenance, and status semantics.
- New optional fields may be added only when their omission preserves the existing result meaning and security behavior.
- A change that reclassifies canonical versus derived data, weakens consistency, changes ordering, alters state meaning, or changes authorization requires a new query version.
- Unsupported versions or required fields are rejected rather than approximated.
- Unknown optional metadata may be ignored only when declared non-semantic and safe; otherwise the query is rejected.
- Cursors and stream continuation tokens are version-bound and cannot be reinterpreted after incompatible change.
- Result states and error categories remain stable across delivery mechanisms even when rendering differs.
- Future distributed readers use the same canonical boundaries and state semantics; they do not create alternate read authority or canonical writers.

# Extension Rules

A new query type or version may be added only through the registered Query Layer extension contract.

Each extension must define:

- one approved, bounded read purpose;
- project, target, principal, and interface scope;
- field authority classifications;
- supported consistency and freshness guarantees;
- provenance and uncertainty requirements;
- authorization and redaction behavior;
- filters and deterministic total ordering;
- pagination and streaming behavior;
- cache identity and invalidation dependencies;
- all result and error states;
- partial-result and stale-disclosure policy;
- semantic version and compatibility behavior;
- deterministic conformance evidence; and
- resource, size, and time bounds.

Extensions must not:

- create a mutation or external effect;
- bypass current-state authorization;
- expose direct canonical storage access;
- add cross-project reads without a separately accepted architectural decision;
- make a cache, index, summary, model result, or audit stream canonical;
- hide unavailable or unauthorized data as empty;
- use nondeterministic ordering for snapshot pagination;
- make a plugin or provider mandatory for canonical reads; or
- change query semantics through caller-specific presentation logic.

# Normative Requirements

1. Every read of canonical Familiar state outside an admitted command's private evaluation boundary **MUST** use this query contract.
2. A query **MUST NOT** mutate canonical state, initiate an external effect, or trigger hidden refresh, indexing, reconciliation, verification, review, or execution.
3. Every query **MUST** contain `query_id`, `request_id`, `project_id`, `principal_id`, `interface_id`, `query_type`, `target`, `consistency`, `pagination`, and `metadata`.
4. The admitted query envelope **MUST** be immutable.
5. Every query **MUST** be scoped to exactly one canonical project.
6. Every query type **MUST** have explicit semantic versioning and bounded target semantics.
7. The Query Layer **MUST** validate the envelope and authenticate and authorize caller context before disclosing protected facts.
8. Query authorization **MUST** use current canonical state and **MUST NOT** be inferred from prior access, cursor possession, cache possession, stream possession, or local presence.
9. An unauthorized response **MUST NOT** reveal whether a protected target exists.
10. The requested consistency **MUST** be satisfied or the result **MUST** explicitly report `Stale` or `Unavailable` according to this contract.
11. Consistency and freshness requirements **MUST NOT** be silently weakened.
12. Safety-sensitive authority and acceptance reads **MUST** use current canonical or an explicitly sufficient causal consistency and **MUST NOT** use bounded-stale state.
13. Every response **MUST** identify its canonical boundary and applicable source or derived freshness.
14. Canonical, source-authoritative, evidence, and derived fields **MUST** be distinguishable by contract.
15. Derived fields **MUST** carry provenance, validation, freshness, and uncertainty sufficient for their declared use.
16. Derived data **MUST NOT** replace repository source, version-control history, or canonical operational state as authority.
17. `Unknown`, `Unavailable`, `Unauthorized`, `Stale`, `Partial`, and `Empty` **MUST** remain distinct machine-readable states.
18. A null value or empty collection **MUST NOT** substitute for any required result state.
19. `Empty` **MUST** mean an authorized, complete, sufficiently fresh query with no matching data.
20. `Unknown` **MUST** mean that no requested fact is recorded or established from retained authoritative evidence.
21. `Unavailable` **MUST** mean the query or required consistency cannot currently be completed or proved.
22. `Unauthorized` **MUST** disclose no protected target, count, freshness, provenance, or availability facts.
23. `Stale` **MUST** identify the failed freshness or validity boundary and **MUST NOT** be used for current authority decisions.
24. `Partial` **MUST** identify every omitted bounded component and **MUST NOT** imply completeness.
25. Every collection query **MUST** define a deterministic total order with a stable unique tie-breaker.
26. Implicit enumeration or storage order **MUST NOT** define result order.
27. Pagination cursors **MUST** bind query semantics, project, visibility scope, filters, ordering, and consistency boundary.
28. Valid snapshot pagination **MUST NOT** skip or duplicate items, and failure to continue the snapshot **MUST** be explicit.
29. Ordinary pagination with a continuation cursor **MUST NOT** be classified as `Partial` solely because more pages remain.
30. Filters **MUST** be registered and versioned; unknown filters **MUST NOT** be silently ignored.
31. Cached results **MUST** be project- and authorization-scoped and **MUST** satisfy current authorization, consistency, freshness, and invalidation checks before use.
32. A cache miss **MUST NOT** be assigned a domain result state.
33. Invalidation **MUST NOT** mutate canonical domain state or implicitly regenerate derived data.
34. Streams **MUST** preserve declared consistency, deterministic order, chunk identity, authorization, and an explicit terminal state.
35. A stream **MUST NOT** mix canonical boundaries while claiming snapshot semantics.
36. Partial results **MAY** be returned only when the query contract permits component independence and the caller has not required completeness.
37. Stale values **MAY** be disclosed only when the query contract and caller permit it and current authorization succeeds.
38. Query retries and continuations **MUST** preserve the original semantic fingerprint unless they begin a new logical request.
39. Query results **MUST NOT** imply approval, mutation authorization, execution authority, verification, review, acceptance, or completion.
40. Extensions **MUST** satisfy the extension rules and **MUST NOT** introduce alternate read authority, mutation paths, or cross-project access.

