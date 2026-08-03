# ADR-001 Adversarial Architecture Review

## Review Status

Skeptical principal-engineer review of `ADR-001-canonical-state-and-event-semantics.md`.

No option is selected or recommended. The purpose of this review is to attempt to invalidate each option by identifying assumptions that would make it unsafe, incomplete, disproportionate, or operationally misleading.

## Review Standard

Each option must survive more than a normal database-design review. Familiar's target architecture expects the persistence model to support:

- One daemon-owned mutable authority.
- Thin MCP, CLI, local-socket, and HTTP adapters.
- Durable tasks, decisions, findings, verification, handoffs, approvals, warrants, and audit evidence.
- Unattended execution with crash recovery and explicit stop conditions.
- External effects that cannot participate in a SQLite transaction.
- Human-readable auditability and truthful rollback claims.
- Long-lived schema and semantic compatibility.
- Local-first operation on macOS and Linux.

The key adversarial question is not whether an option can be implemented. All three can. The question is whether its failure modes can be made visible, bounded, recoverable, and understandable without creating a second source of truth or an operational system disproportionate to Familiar.

## Option A Review — Conventional Transactional Tables Plus Append-Oriented Audit Events

### Attempted invalidation

Option A may be internally inconsistent with the target requirement for complete, attributable material-action history. It makes audit supporting evidence while permitting current state to exist independently of that evidence. That creates a structurally valid database state that may be historically unverifiable—the exact condition Familiar's philosophy labels a defect.

If audit emission is optional or best-effort, Option A does not merely have an implementation weakness; it encodes the possibility that the system can succeed operationally while failing stewardship. If audit emission becomes mandatory and atomic for material commands, Option A approaches Option C and may cease to be a distinct option.

### Hidden assumptions

- Current rows contain enough information to explain the current state without replaying prior decisions.
- The set of historical questions humans will ask is knowable when table schemas are designed.
- Audit coverage can remain secondary without becoming inconsistent.
- Every mutation passes through application code that knows whether and how to audit it.
- Direct database repair will be rare, controlled, and separately recorded.
- Human operators will trust current rows even when audit evidence is missing or contradictory.
- Existing CRUD-oriented repository APIs can grow into domain-command boundaries without leaving bypass paths.
- The distinction between operational logging and engineering evidence will remain understood over time.
- A local single-user system has low enough concurrency that row-level conflict semantics are sufficient.
- Rollback can be represented as another state mutation without needing exact prior causal history.

### Unstated invariants

- No material mutation may bypass the daemon's governed persistence boundary.
- State transitions must enforce allowed predecessor states, not merely valid column values.
- Every retryable command needs an idempotency identity even if audit is not authoritative.
- Audit payloads must bind actor, project, task, authority, prior version, resulting version, and causal request.
- Current-state rows and audit records need a common version or transaction identity if they are ever correlated reliably.
- A missing audit record must be detectable, not merely theoretically possible.
- Denials, failed validations, interrupted attempts, and ambiguous external effects need records even when no domain row changes.
- Legacy state predating audit must remain explicitly distinguishable from fully evidenced state.
- Backup and restore must preserve current state and audit with one consistent recovery point.

### Operational risks

- Operators may see valid current state with no evidence of who authorized it.
- Audit semantics can differ among subsystems because tables encourage local CRUD ownership.
- Emergency SQL repairs can silently bypass domain invariants and audit.
- Restoring current tables and audit records from different backup points creates plausible but false history.
- Audit retention may remove context required to understand still-current rows.
- A malformed audit payload can be ignored because it does not block the state mutation.
- Operational tooling may report success from rows while an audit writer is degraded.
- The easiest maintenance action—editing a row—becomes the least accountable action.

### Scaling risks

- Query patterns spanning current state and heterogeneous audit payloads may become expensive and difficult to index.
- JSON audit payloads may grow without a disciplined envelope and artifact-reference policy.
- High-frequency process, log, and verification events can turn a supporting audit table into the largest and hottest table.
- A single global audit sequence may become write contention; per-project sequences complicate global chronology.
- Retention and archival can break references from long-lived decisions or handoffs.
- Supporting audit schemas may accumulate subsystem-specific event types without a coherent domain taxonomy.

### SQLite-specific concerns

- SQLite serializes writes; current-row mutations plus audit inserts lengthen writer transactions.
- WAL growth can become significant during long verification or evidence transactions if readers remain open.
- `INTEGER PRIMARY KEY` order reflects insertion order, not causal order across pre-transaction work or external effects.
- Foreign keys from immutable audit records to mutable/deletable operational rows can obstruct retention or allow dangling history depending on design.
- SQLite does not prevent an alternate process with file access from bypassing daemon-level audit invariants.
- Backup APIs and file copies must account for WAL state or risk inconsistent snapshots.
- Large audit tables require deliberate indexing, vacuum, and retention behavior; otherwise local storage and startup maintenance degrade.
- A busy timeout is not a concurrency model and can turn conflicts into latency or opaque failure.

### Daemon ownership concerns

- "Daemon-owned" is only a convention unless the database file and repository APIs prevent other writers.
- In-process dashboard, tray, maintenance, and future plugins may call repositories below the audited command layer.
- A daemon crash followed by a compatibility client fallback could reintroduce direct writes.
- Multiple daemon instances need a hard exclusion mechanism; PID files alone are insufficient authority locks.
- Administrative repair paths need a privileged command model or will become unofficial direct writers.
- Audit behavior distributed across repositories weakens the claim that the daemon owns material actions as domain commands.

### Recovery edge cases

- Current state commits and audit append fails when they are not atomic.
- Audit append commits but a response is lost, causing a client retry and duplicate logical action.
- A crash occurs after a state transition but before an external process is started.
- A crash occurs after process start but before attempt identity is persisted.
- A restore rolls current state backward while retaining later audit entries.
- A restore retains current state but loses audit entries that justified it.
- A partially applied schema migration makes old audit payloads unreadable while current rows remain usable.
- A row is repaired manually and later automation assumes its audit history is complete.
- An interrupted cleanup removes artifacts still referenced by audit records.

### External-effect edge cases

- A push, release, process invocation, or filesystem effect succeeds after the database transaction rolls back.
- An effect times out but actually succeeds remotely; a retry repeats it.
- An effect intent is audited but the observed outcome is missing indefinitely.
- Rollback of current state creates the appearance that an external effect was undone when it was not.
- A credential is used successfully but redaction removes enough detail to prevent attribution.
- A worktree is modified outside the daemon after the state row says execution stopped.
- A child process survives daemon termination and continues producing effects.

### Human factors

- Familiar relational tables appear familiar enough that maintainers may underestimate the rigor required for evidence.
- Developers may regard audit insertion as boilerplate and omit it during urgent changes.
- Operators may prefer direct SQL because it is easier than a governed repair command.
- Users may interpret an audit timeline as complete even when the model only promises supporting evidence.
- The distinction between "current truth" and "historical evidence" may be lost in UI wording.
- Human approvers may believe an audit entry proves that approval was valid rather than merely recorded.

### Debugging complexity

- Debugging a discrepancy requires deciding whether rows or audit are wrong, with no reconstruction mechanism to arbitrate.
- Heterogeneous before/after payloads may be impossible to compare after schema evolution.
- Missing audit is absence of evidence, which is hard to distinguish from an action that never occurred.
- Reproducing race conditions requires correlating database transactions, daemon logs, client requests, and external effects manually.
- Direct SQL is easy for current state but poor for causal chains spanning tasks, warrants, processes, verification, review, and handoff.

### Migration hazards

- Legacy rows may be mistaken for fully audited records after adding an audit table.
- Backfilling synthetic events could fabricate history and violate explicit-decision invariants.
- Adding versions to current rows may expose previously tolerated lost-update behavior.
- Deduplicating or normalizing current data before audit begins can erase the evidence needed to explain changes.
- A mixed-version daemon/client period can introduce unaudited writes after the declared audit boundary.
- Existing direct MCP writes can bypass new audit semantics until authority cutover is complete.

### Future feature limitations

- Exact time-travel reconstruction is unavailable unless snapshots or change histories are separately added.
- Determining "why" a row has its value depends on audit completeness and payload quality.
- New workflows needing causal replay may require bespoke history tables.
- Cross-domain state-machine analysis is harder when only current rows are authoritative.
- Offline conflict resolution and branching workflow state have no natural representation.
- Proving that every material action was audited requires continuous coverage tooling rather than following from the model.

## Option B Review — Full Event Sourcing with Projections

### Attempted invalidation

Option B may solve a problem Familiar does not have at a cost Familiar cannot safely absorb. Full event sourcing makes event design, replay, projection correctness, upcasting, and aggregate boundaries permanent parts of every domain feature. That specialized machinery can become a second full-time project, conflicting with Familiar's stated simplicity and local-first goals.

It also risks confusing event-stream authority with complete auditability. State-changing domain events do not automatically record denied commands, reads, tool output, process behavior, external effects, credential disclosures, or human observations. Full event sourcing can therefore impose maximum complexity while still requiring a separate operational audit model.

### Hidden assumptions

- Familiar benefits materially from exact state reconstruction and temporal projections.
- Domain boundaries are understood well enough now to publish durable event contracts.
- All current operational state can be derived deterministically from events.
- Projection lag is acceptable or can be eliminated without losing the benefits of separation.
- Maintainers will understand event sourcing, aggregate design, replay, snapshotting, and upcasting.
- Existing SQLite rows can be converted into a trustworthy baseline without real historical events.
- Event volume remains manageable on user machines over the product's lifetime.
- Every command can be assigned cleanly to one aggregate or coordinated safely across several.
- External processes can be represented by events without events being mistaken for effects.
- The debugging value of history exceeds the cognitive and operational cost of indirection.

### Unstated invariants

- Events are immutable, uniquely identified, versioned, attributable, and durably ordered.
- Every projection is deterministic for a fixed event stream and projector version.
- Projector side effects are forbidden or idempotent.
- Stream expected-version checks are mandatory for every aggregate mutation.
- Event serialization preserves semantic meaning across application versions.
- Snapshots are verified accelerators, never independent authority.
- Projection checkpoints advance only after corresponding writes commit.
- Rebuilds cannot publish partially rebuilt projections as current.
- Event redaction cannot change domain meaning or invalidate replay.
- Aggregate invariants survive command retries, daemon restarts, and cross-aggregate workflows.
- An event store corruption boundary is detectable and recoverable.

### Operational risks

- A projection bug can make all visible current state wrong while the event store remains internally valid.
- A corrected projector can reinterpret years of history and abruptly change current state.
- Event replay may make the daemon unavailable after upgrade or corruption.
- Snapshotting and compaction can quietly weaken the promised reconstructibility.
- Repair requires specialized event surgery or compensating events, even for obvious local mistakes.
- Operators may not know whether to trust events, snapshots, or projections during an incident.
- Asynchronous projections can return stale authorization, warrant, or task state after a successful command.
- Synchronous projections enlarge append transactions and partially recreate ordinary relational updates with more machinery.

### Scaling risks

- Event streams, projections, snapshots, indexes, logs, and artifacts multiply storage.
- Replay time grows with history and number of projectors.
- Adding a new projection can require scanning the entire retained event history.
- Global ordering creates a hot append path; only per-stream ordering complicates cross-domain chronology.
- Snapshot frequency trades storage for replay cost and adds invalidation/versioning obligations.
- High-frequency execution telemetry does not belong naturally in aggregate streams but can overwhelm them if included.
- Large binary or textual evidence cannot reasonably live in events and introduces artifact-store consistency problems.

### SQLite-specific concerns

- SQLite's single-writer model limits concurrent append and synchronous projection updates.
- Asynchronous projection workers still contend for the same database writer unless separated, introducing cross-database consistency problems if separated.
- Long replay reads can hold snapshots that prevent WAL checkpoint progress and grow WAL files.
- Rebuilding projections atomically may require shadow tables and swaps, increasing disk usage.
- A global event sequence is easy to implement but becomes central contention and can be confused with causality.
- Foreign keys are awkward when canonical identities exist in events but projections are rebuildable.
- SQLite file corruption affects both event authority and projections unless backups are independently validated.
- VACUUM, compaction, and retention interact badly with an immutable-history promise.
- JSON payload querying and upcasting inside SQLite are limited compared with dedicated event tooling, pushing complexity into application code.

### Daemon ownership concerns

- The daemon becomes responsible for event-store integrity, aggregate loading, command handling, projection scheduling, replay, snapshots, and schema compatibility.
- Thin clients may observe accepted commands before projections expose their results.
- Multiple daemon instances can race on stream versions unless hard single-writer exclusion is enforced.
- Background projection failures can degrade policy queries while command append remains available.
- Administrative repair cannot safely edit projections or events without domain-specific tooling.
- A plugin or subsystem that emits an invalid event can permanently contaminate canonical history.
- Restart behavior becomes coupled to every projector, not merely canonical database accessibility.

### Recovery edge cases

- Event append commits but required synchronous projection update does not.
- A projector writes results but fails before advancing its checkpoint, causing duplicate application.
- A checkpoint advances before all projection changes are durable, causing permanent omission.
- An upcaster crashes on one historical event and blocks all later replay.
- A snapshot is accepted for the wrong aggregate/event version.
- A new projector rebuild observes event types it cannot interpret.
- Events are intact but the event-index or sequence metadata is corrupted.
- A partial restore combines an old event store with newer projections or vice versa.
- The daemon crashes during a projection-table swap.
- Replaying nondeterministic historical code produces different state.

### External-effect edge cases

- An event such as `PushRequested` is mistaken for proof that a push occurred.
- An effect occurs, but the outcome event is never appended because the daemon crashes.
- An outcome event is appended from an ambiguous timeout and records the wrong reality.
- Replay accidentally re-executes an external effect if projector/event-handler boundaries are not pure.
- Compensating events alter projected state but cannot undo the effect.
- A remote system changes independently after an outcome event is recorded.
- Exactly-once external delivery is assumed from at-least-once event handling.
- Credential or network policy changes between intent and handler execution.

### Human factors

- Event sourcing terminology can create false confidence that every action is audited.
- Maintainers may design events around UI actions or database diffs rather than durable domain facts.
- Operators lose the ability to fix simple state with comprehensible SQL.
- Event histories can be overwhelming without high-quality tooling and summarization.
- Developers may avoid changing incorrect event schemas because of compatibility burden.
- Human reviewers may struggle to understand projected state without replay-aware diagnostics.
- Snapshot and upcaster behavior may be understood by only one maintainer, creating knowledge concentration.

### Debugging complexity

- A wrong current value can originate from an invalid event, wrong event order, upcaster, projector, checkpoint, snapshot, or query projection.
- Reproduction requires exact historical projector and upcaster versions unless semantics are stable forever.
- Causal chains spanning aggregates are not obvious from per-stream order.
- Time-travel state may differ depending on which projector version is used.
- Event replay can reproduce the bug rather than explain it.
- Logs and external evidence still require separate correlation with event IDs.

### Migration hazards

- Synthetic baseline events may look indistinguishable from observed historical events.
- Per-row import events may encode current schema rather than durable domain meaning.
- Missing prior transitions make stream versions historically arbitrary.
- Cutover requires preventing old direct writers while establishing stream authority atomically.
- Projection parity may pass for current fixtures but fail on unusual legacy records.
- Rollback after accepting new events requires either translating them back to row mutations or retaining both architectures.
- An event model chosen before task/warrant/verification domains are mature can fossilize incorrect boundaries.

### Future feature limitations

- Every new domain capability must integrate with event versioning and projection infrastructure.
- Cross-aggregate invariants remain difficult and may lead to eventual consistency where policy needs immediate certainty.
- Data minimization or deletion requests conflict with immutable event history.
- Sensitive payload mistakes are hard to erase without breaking integrity.
- Provider/model artifacts may be too large or unstable for event payloads, requiring a separate artifact authority.
- Offline branch-and-merge of event streams is not automatically safe despite event history.
- A future move away from event sourcing becomes a semantic extraction project, not a database migration.

## Option C Review — Transactional Canonical State with First-Class Material-Action Audit Events

### Attempted invalidation

Option C may be Option A with stronger language rather than a genuinely different architecture. "Material action" is an escape hatch: any action omitted from the definition is unaudited by design, and any action included creates permanent storage, redaction, and compatibility obligations. Unless there is a mechanically enforceable boundary proving that every governed mutation emits exactly one sufficient event, Option C can degrade silently into Option A.

It also claims dual authority—tables for current truth and audit events for historical truth—without defining how contradictions are resolved. Atomic writes prevent some divergence at commit time, but later schema migrations, retention, repair, restore, and operator actions can still make the two narratives disagree.

### Hidden assumptions

- Material actions can be exhaustively and stably defined.
- A core command boundary can prevent all unaudited mutations.
- One audit event can represent a material action with sufficient fidelity.
- Atomic state/event writes do not make transactions too broad or slow.
- Current state never needs to be reconstructed solely from audit history.
- Operators can distinguish evidence authority from current-state authority.
- Existing repositories can be refactored behind commands without persistent bypasses.
- Causal IDs and row versions are sufficient to explain multi-step workflows.
- Audit schema evolution can remain inspectable without replay-capable upcasters.
- External effects can be modeled through durable intent and observation without exactly-once guarantees.

### Unstated invariants

- Every state-changing command maps to one declared material-action class.
- Canonical mutation and required audit append share the same SQLite transaction.
- No repository method capable of mutation is callable outside the command boundary.
- Every event includes precondition version, result version, actor, authority, request identity, and outcome.
- Denied, failed, and no-state-change actions have explicit audit rules.
- Idempotency keys cover command retries and external-effect attempts.
- Material-action taxonomy is versioned and reviewed as architecture.
- Restores and repairs preserve or explicitly reestablish state/audit consistency.
- Audit retention cannot remove evidence referenced by canonical rows.
- External effect intents and outcomes are separate, durable states.
- A consistency checker can detect missing, duplicate, or impossible state/event pairs.

### Operational risks

- Developers believe atomic append guarantees completeness while bypass paths remain.
- Audit event volume expands as "material" grows, eventually including noisy operational telemetry.
- Event payloads become informal copies of row state and drift independently.
- Current state and audit history can disagree after manual repair or partial restore.
- A failure to serialize audit payloads can block otherwise valid operational work.
- Strict auditing can make the whole daemon unavailable when audit storage is unhealthy.
- Relaxing auditing during an incident creates an undocumented historical gap.
- Retention policy can undermine the claim that audit is authoritative evidence.

### Scaling risks

- Every material write increases transaction duration and index maintenance.
- Causal indexes across actor, task, warrant, attempt, event type, and project can make audit writes increasingly expensive.
- Verification and execution produce many actions; classifying each as material may overwhelm a single table.
- Querying a long causal chain across typed relational rows and versioned event payloads may require complex joins plus JSON parsing.
- Archival of old audit partitions is awkward in SQLite without table rotation or database attachment strategies.
- A single global sequence becomes hot; per-project sequences complicate cross-project operator timelines.

### SQLite-specific concerns

- Atomic state/audit transactions fit SQLite, but write amplification and index count directly affect the single writer.
- Audit append failure causes the entire state mutation to fail, coupling availability to evidence storage.
- SQLite has no native table partitioning for long-lived audit history.
- Large append tables increase backup, restore, integrity-check, and vacuum time.
- WAL checkpoints can be delayed by long-running readers of audit history.
- Trigger-based enforcement could improve coverage but risks hiding domain behavior and cannot capture rich command context safely.
- Application-only enforcement remains bypassable by alternate writers with file access.
- Transaction IDs are not a first-class SQLite concept exposed for durable correlation; the application must define them correctly.

### Daemon ownership concerns

- The daemon must be the only writer in practice, not merely by convention.
- Shared repository code must not expose public mutation methods below the audited command boundary.
- Maintenance, migration, repair, and background reconciliation all need explicit material-action rules.
- Thin adapters must propagate authenticated actor and request identity without being trusted to define authority.
- Multiple daemon instances can both pass application checks unless a hard ownership lock exists.
- An audit-heavy command layer can become a monolith through which every subsystem must pass.
- Background deterministic processes may have ambiguous actor identity and authority provenance.

### Recovery edge cases

- A state/event transaction commits but the client receives no response and retries.
- A migration changes current rows without emitting ordinary material-action events; audit history no longer narrates current state.
- A backup restore moves state and audit backward while external effects remain later.
- State is restored from one snapshot and artifacts/audit from another.
- A repair command corrects current state but cannot reconstruct the missing historical cause.
- Audit payload deserialization fails after upgrade, blocking operator inspection but not current-state reads.
- Idempotency records are retained for less time than clients may retry.
- An interrupted external-effect reconciliation writes a second contradictory outcome.
- Clock rollback affects expiration while sequence ordering remains valid.

### External-effect edge cases

- Intent commits but the daemon never attempts the effect.
- Effect succeeds but no observed-outcome transaction commits.
- Effect fails locally after succeeding remotely.
- Outcome observation is based on a non-authoritative or eventually consistent remote API.
- Compensating action fails, leaving canonical state and external reality permanently divergent.
- The same warrant authorizes a retry after authority should have been consumed.
- A child process performs undeclared effects that the audit taxonomy never sees.
- Worktree or repository state changes between authorization and effect execution.
- An external actor changes the remote resource between observation and rollback.

### Human factors

- "First-class audit" may be interpreted as replayable history when it is not.
- "Material" invites subjective categorization and political pressure to reduce noise or storage.
- Developers must understand two linked schemas for every important mutation.
- Operators may see a clean audit sequence and overlook missing categories of action.
- Direct repair becomes culturally prohibited even when the governed repair tool is incomplete.
- Human approvers may not understand whether an audit event records intent, authorization, attempt, observation, or completion.
- The boundary between event payload and referenced artifact may make evidence appear complete when an artifact has expired.

### Debugging complexity

- Diagnosis requires joining current versions, audit sequence, causal IDs, daemon logs, artifacts, and external observations.
- Atomic commit proves co-occurrence, not correctness of the event's semantic description.
- Contradictions lack an automatic arbiter because audit cannot necessarily rebuild state.
- Historical payload versions complicate tooling even without projection replay.
- A missing event can be detected only if current rows retain enough version/transaction linkage.
- Multi-command workflows can have individually valid events but an invalid overall sequence.

### Migration hazards

- Declaring an audit-era boundary can imply more historical completeness than legacy data supports.
- Existing direct MCP or repository writers may remain active during cutover and create unaudited state.
- A state backfill performed as a migration may not emit events, producing immediate mismatch with strict coverage assertions.
- Emitting synthetic migration events can be mistaken for observed actions.
- Adding transaction/version identities to existing rows requires conservative baseline semantics.
- Rolling back application code after new event versions exist can make audit records unreadable.
- A partial rollout can create both Option A and Option C semantics in the same database.

### Future feature limitations

- Exact reconstruction and temporal queries remain unavailable unless more history is added.
- Consumers may gradually demand replay from events not designed to support it.
- Material-action contracts become durable public semantics even though they are not domain-event authority.
- High-volume telemetry must live elsewhere, creating another correlation boundary.
- Cross-database or cloud synchronization cannot assume audit events are complete state replication.
- Branching hypothetical workflow state is awkward because current rows are singular.
- Strong atomic audit can constrain future storage separation or sharding.
- Data deletion and redaction can weaken historical evidence or require cryptographic erasure patterns.

## Comparative Risk Classification

### Fundamental risks

These risks arise from the problem domain and remain under all three options:

- **Database versus external reality:** SQLite cannot atomically commit Git, filesystem, process, credential, network, or provider effects.
- **Intent versus outcome:** Recording authorization or intent never proves an effect occurred, failed, or was reversed.
- **At-least-once uncertainty:** Lost responses and ambiguous timeouts require idempotency and reconciliation regardless of persistence style.
- **Human authority:** A technically valid record does not prove that approval was informed, appropriate, or legitimately granted.
- **Repository authority:** Familiar state can never replace source and Git history; cross-system divergence must be detected.
- **Historical boundary:** Existing data lacks the complete causal history desired by the target architecture. No migration can recover facts never recorded.
- **Sensitive evidence:** Complete auditability conflicts with data minimization and secret redaction.
- **Schema semantics:** Once historical records are relied upon, their meaning becomes a long-term compatibility obligation.
- **Operator repair:** Any system needs a governed way to repair corruption or mistaken state without pretending the original history was different.
- **Single-daemon failure domain:** Central authority simplifies semantics but makes daemon availability and ownership correctness critical.
- **Rollback limits:** Compensating state cannot guarantee reversal of repository or external effects.
- **Audit taxonomy:** The system must decide which failures, denials, reads, attempts, and outcomes are material; no storage model makes that decision automatically.

### Risks that are implementation mistakes

These are avoidable defects rather than necessary consequences of an option:

- Non-atomic state and required-audit writes where atomicity is promised.
- Missing idempotency keys for retryable commands and effects.
- Using timestamps as the sole ordering or causality mechanism.
- Allowing direct client writes after daemon authority is established.
- Failing to enforce single-daemon ownership.
- Advancing projection checkpoints before durable projection writes.
- Re-executing external effects during event replay.
- Publishing partially rebuilt projections.
- Omitting actor, project, task, approval, warrant, or causal identity from required records.
- Treating intent events as outcome evidence.
- Restoring state and history from inconsistent backup points.
- Logging secrets or retaining unredacted provider payloads.
- Making legacy baseline records indistinguishable from observed post-boundary history.
- Deleting audit/artifacts that are still referenced by canonical decisions or handoffs.
- Letting application versions write event formats older readers cannot tolerate during rollback.
- Returning a successful command response before the option's promised consistency level is durable.

The frequency and detectability of these mistakes differ by option. Classification as an implementation mistake does not imply low risk.

### Risks that disappear or materially change if assumptions change

| Assumption change | Risks reduced or removed | Risks introduced or increased |
|---|---|---|
| Familiar never performs unattended execution or external effects | Warrant consumption, process recovery, ambiguous remote outcome, and effect rollback risks shrink substantially | Target architecture is no longer satisfied |
| Audit needs only best-effort diagnostics | Option A audit-completeness pressure and atomic-append requirements largely disappear | Engineering evidence, accountability, and philosophy invariants weaken |
| Exact historical reconstruction is mandatory | Ambiguity over replay value disappears; Option B's core premise becomes necessary to evaluate directly | Event compatibility, replay, projection, and migration burdens become unavoidable |
| Exact reconstruction is explicitly unnecessary | Several Option B benefits disappear; Options A/C avoid replay obligations | Root-cause and temporal analysis depend on audit payload quality and snapshots |
| All projections are synchronous | Option B stale-read and read-after-write ambiguity decreases | Write latency, lock duration, and SQLite contention increase |
| All adapters tolerate eventual consistency | Option B projection lag becomes less operationally disruptive | Approval, warrant, and status queries may still be unsafe if stale |
| Audit retention is short and bounded | Storage, backup, and long-query risks decrease for A/C | Durable project history and decision evidence may be violated |
| Audit retention is permanent | Historical gaps from deletion decrease | Storage, redaction, schema compatibility, and privacy risks increase |
| Only the daemon process can physically access the database | Direct-writer bypass risk decreases for A/C and projection corruption risk decreases for B | Backup, diagnostics, and repair workflows become more dependent on daemon tooling |
| Material actions are few, stable, and centrally defined | Option C taxonomy and write-amplification risks decrease | Future expansion can invalidate the assumption |
| Domain aggregates are mature and stable | Option B event/aggregate fossilization risk decreases | Migration and specialized operational complexity remain |
| Data volume remains small | SQLite write contention, replay time, archival, and storage risks decrease | Semantic and correctness risks remain unchanged |
| Data volume or concurrency grows sharply | None of the models becomes automatically invalid | SQLite single-writer contention and maintenance costs become first-order constraints for all options |
| Direct SQL repair is forbidden and complete repair commands exist | Audit bypass and projection corruption risks decrease | Operator recovery depends on the completeness and availability of repair tooling |
| External systems provide idempotency and authoritative status APIs | Ambiguous effect and duplicate-delivery risks decrease | Provider-specific semantics and dependency risks increase |
| Legacy history can be discarded explicitly | Migration complexity and false historical reconstruction decrease | Durable-memory expectations and user trust may be violated |

## Cross-Option Comparative Observations

### Authority ambiguity

- Option A has one clear current-state authority but weak historical authority unless audit rules are strengthened.
- Option B has one historical authority but introduces projection authority questions during lag, corruption, or rebuild.
- Option C deliberately has current-state authority and historical-evidence authority, requiring explicit contradiction and repair semantics.

### Completeness claims

- Option A is easiest to operate while making the weakest inherent completeness claim.
- Option B makes state-transition completeness structurally plausible but does not guarantee operational-audit completeness.
- Option C makes material-action completeness an invariant, but the material-action taxonomy can hide omissions.

### Irreversibility

- Option A's principal irreversible loss is history never recorded.
- Option B's principal irreversible commitment is durable event semantics and replay infrastructure.
- Option C incurs both a risk of unrecorded categories and durable audit-contract obligations, without guaranteed replay.

### Failure visibility

- Option A can fail silently through absent audit.
- Option B can fail visibly but opaquely through projection/replay disagreement.
- Option C can detect some state/event mismatches but can still fail silently when a bypass or taxonomy omission is outside its checks.

### Complexity placement

- Option A places complexity in discipline, coverage, and later forensic correlation.
- Option B places complexity in the persistence model, event evolution, projections, and operations.
- Option C places complexity in command-boundary enforcement, dual-schema semantics, causal audit, and external-effect recovery.

### SQLite pressure

- Option A minimizes mandatory write amplification but can accumulate a large secondary audit table.
- Option B adds event history, projections, snapshots, and replay workloads to the single-writer/local-file model.
- Option C makes audit write amplification mandatory for material actions and ties operational availability to audit persistence.

## Evidence Gaps Exposed by This Review

The ADR cannot distinguish hypothetical from material risks without evidence in these areas:

- Expected event/action frequency during indexing, verification, review, and unattended execution.
- Maximum acceptable command latency and daemon restart time.
- Whether exact historical state reconstruction is a real requirement.
- A complete draft taxonomy of material actions, denials, failures, attempts, and outcomes.
- Proposed aggregate boundaries for Option B.
- A mechanically enforceable command/mutation boundary for Options A and C.
- A crash matrix spanning database commit, process start, effect execution, observation, and response delivery.
- Long-lived SQLite measurements for WAL, audit growth, projection replay, backup, restore, and integrity checking.
- A legacy migration rehearsal demonstrating the historical boundary without fabricated causality.
- Operator exercises for repair, reconciliation, audit explanation, and rollback.
- Data retention and redaction constraints for logs, model outputs, credentials, and external-effect evidence.
- A definition of read-after-write consistency required by each thin adapter.

## Review Conclusion

This review does not select or recommend an option.

Each option can satisfy portions of the target architecture only if additional invariants are made explicit:

- Option A must explain how incomplete supporting audit can satisfy durable engineering accountability, or define stronger coverage that may collapse its distinction from Option C.
- Option B must justify permanent event/projection complexity and separately account for operational actions that are not state-changing domain events.
- Option C must prove that material-action coverage is mechanically enforceable and that dual current/history authority can be repaired without pretending audit is replayable.

The remaining decision is not reducible to "simple tables versus events." It depends on the required strength of historical reconstruction, acceptable operational complexity, enforceable audit coverage, external-effect recovery, and the evidence that Familiar's actual workloads justify those costs.
