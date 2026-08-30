# Familiar Product Backlog

**Updated:** 2026-08-29
**Authority:** `docs/north-star.md`

This index reconciles the active backlog with current implementation. Location
and explicit status must agree: completed specifications live in `done/`; active
or partial work lives directly in `docs/prds/` until PRD-030 defines the next
structured layout contract.

## Critical path: “have at it”

1. PRD-039 — checkpointed execution and concurrent `resume all`. **Completed.**
2. PRD-030 — structured PRD contract and profile-neutral dependencies. **Completed.**
3. PRD-031 — worker capability registry and deterministic routing. **Completed.**
4. PRD-033 — actionable review decisions and repository delivery policy. **Completed.**
5. PRD-019 — planner and one human batch-approval gate. **Completed.**
6. PRD-020 — reconcile parallel scheduling with the canonical graph. **Completed.**
7. PRD-034 — portable persistent worker. **Completed.**
8. PRD-036 — repository onboarding.
9. PRD-037 — security and recovery burn-in.
10. PRD-038 — multi-repository product acceptance.

## Economy track

- PRD-024 — finish genuinely enforceable execution budgets. **Completed.**
- PRD-028 — cost-tiered review. **Completed.**
- PRD-029 — stable prompt prefixes and cache economics. **Completed.**
- PRD-042 — repository risk vocabulary. **Completed 2026-08-29** — the first
  PRD executed end-to-end by Familiar's own autopilot on this repository
  (sonnet implementation, opus independent review, human risk acceptance).
- PRD-043 — risk-aware registry route rules. **Completed 2026-08-29** by the
  autopilot with reviewer-driven remediation; the remediation also delivered
  per-PRD route context, an advance on PRD-044.
- PRD-044 — per-PRD worker selection; retires `model_routes`. **Approved 2026-08-29.**
- PRD-045 — declared-risk review tiering. **Approved 2026-08-29.**
- PRD-041 — verification-failure escalation. **Approved 2026-08-29.**
- PRD-032 — model probation and empirical routing. Sequenced after PRD-041/044/045:
  its empirical scores require the reconciled registry routing and persisted
  risk inputs those PRDs create. Its cost-per-accepted-PRD scores read the
  PRD-051 usage ledger (today 11 of 15 executions have unknown cost).
- PRD-040 — superseded by PRD-042–045 (owner-approved decomposition, 2026-08-29);
  retained as the design record.

## Supporting product surface

- PRD-026 — complete per-repository execution policy resolution. **Completed.**
- PRD-035 — execution-era API/MCP/dashboard state surface. **Completed 2026-08-30** —
  first fully autonomous finding→remediation→clean-review→completion cycle.

## Provider and delivery track (approved 2026-08-30)

- PRD-047 — provider/model configuration CLI: kind-tagged endpoints,
  BYO-Auth diagnostics, probe-before-persist, comment-preserving edits,
  decision-row audit.
- PRD-048 — internal delivery targets: Familiar as its own CI/CD at
  garage scale; deploy-target providers, recipes, smoke evidence, and the
  first `external_gates` resolver (internal evidence).
- PRD-049 — shareable project configuration: checked-in familiar.toml
  under declare-and-bind with an approved-snapshot authority gate.
- PRD-050 — cloud deploy targets (AWS/GCP/Azure/DO) as replaceable CLIs.

## Usage and cost accounting track (drafted 2026-08-30, awaiting owner approval)

A dependency-ordered decomposition of provider usage, cost ingestion,
billing-source discovery, and reconciliation. Kept as three bounded PRDs
because each has a distinct risk surface and delivery boundary: a
persistence/ledger layer, the only network-facing collector (admin-credential
security), and deterministic reconciliation plus the read surface.

- PRD-051 — billing modes and the usage observation ledger: distinct
  uncached/cache-read/cache-write/output categories, per-model observations,
  sanitized accounting evidence envelopes (never complete terminal
  streams), the minimal ProjectId contract with machine-local issuance,
  exact nanoUSD canonical money, versioned price schedules, closed cost
  provenance, subscription declarations. Depends on PRD-024/029/039/044.
- PRD-052 — authoritative Anthropic organization cost collection:
  `kind = "billing"` provider sources composing with PRD-047
  (probe-before-persist via `/v1/organizations/me`, BYO-Auth, decision rows),
  paginated UTC daily cost-report windows with exact string-decimal money
  and a snapshot-revision model for corrected or restated provider
  reports (dedup by payload hash, superseding revisions, one
  current-effective projection),
  independent multi-organization cursors, duplicate-binding rejection,
  fail-closed individual-account and external-cloud modes. Depends on
  PRD-047/051.
- PRD-053 — cost reconciliation and attributed reporting: append-only
  reconciliation with explicit unattributed/pending/mismatch states,
  per-component warrant reservations, uncached-token/cost warrant
  denominations, authority-labeled cost queries on the PRD-035 surfaces.
  Depends on PRD-035/051/052.
- PRD-054 — OpenAI Platform and Codex usage accounting (drafted
  2026-08-30): the second provider behind the same interfaces —
  Codex terminal telemetry with the reasoning-output category, sanitized
  evidence envelopes, and exactly-once terminal persistence; authentication-mode
  classification (ChatGPT plan / API key / enterprise access token)
  before any monetary interpretation; OpenAI organization Costs/Usage
  collection as `kind = "billing"` sources with org/project scope and
  duplicate-collector rejection; ChatGPT plan credits as a typed unit
  that never converts to dollars. Depends on PRD-047/051; reconciles
  through PRD-053's engine. OpenAI was added as a provider-adapter PRD
  (PRD-051 extended only for the neutral generalizations: the
  reasoning-output category and typed credit units) because PRD-052 is
  deliberately Anthropic-bounded and a merged collector PRD would couple
  two vendors' API risk into one delivery boundary.
- PRD-055 — project attribution and historical usage series (drafted
  2026-08-30): durable Familiar project identity above the path-bound
  layers (wave-one `projects.repo_root`, `RepositoryIdentity.key`,
  `execution_history.repository`), worktree rollup with explicit
  degraded/fork states; project↔provider attribution bindings with the
  attributed-plus-unattributed-equals-total invariant; the
  provider-neutral `usage_series` query contract (arbitrary half-open
  UTC ranges, hour/day/week/month buckets, sparse and dense series,
  drill-down to observations); rebuildable rollups with indefinite raw
  retention; provider capability matrix; future-server export
  compatibility without any server in scope. PRD-051 owns the minimal
  ProjectId contract (globally unique id, degraded classification,
  machine-local issuance behind a stable resolver boundary — its
  acceptance never requires PRD-055) plus the
  period/observed/ingested time envelope and discrete-never-cumulative
  facts; PRD-055 implements full registry resolution behind that
  boundary. PRD-053 gained the reservation lifecycle
  (acquire/commit/release/expire/crash-recover). Depends on
  PRD-051/053. Boundary: PRD-053 keeps source-centric billing views
  (month-to-date per source, variance); PRD-055 adds the
  project-centric time axis over the same rows, counted once.

Ledger defect ownership: PRD-053 owns B1 and, with PRD-051, B8; PRD-051
enables but does not fix B5, B6, and B13, whose stop-reason,
review-checkpoint, and finalize-retry fixes remain execution/recovery work.
PRD-051 supplies the trustworthy cost observations PRD-032 requires.

## Control plane track (drafted 2026-08-30, awaiting owner approval)

- PRD-056 — daemon-owned multi-project control plane: one deterministic
  application-service layer (routing, policy, warrants, transitions,
  accounting, authorization, queries) hosted by a persistent daemon
  that owns projects, per-project durable queues, scheduling, detached
  executions, and recovery; CLI/MCP/dashboard (and a later tray client)
  become adapters over a same-user Unix-socket protocol with versioning,
  idempotent submission, and reconnect cursors; capability-scoped MCP
  sessions with a least-authority matrix; phased migration
  (extract services → host behind socket → daemon-owned execution →
  MCP re-homed) with exactly one mutating orchestrator at all times.
  Retires the multi-writer SQLite contention class (B13's environment)
  and absorbs the wave-one daemon; the PRD-034 supervisor shifts to
  keeping the daemon alive. Depends on PRD-034/035/039/044/051.

Supporting work may proceed in parallel but does not replace the critical path.
PRD-035 becomes acceptance-critical only for facts PRD-038 requires clients to
query consistently.

## Delivery assurance policy

- Default: reviewed PR, manual merge and deployment authority.
- PoC: explicit, finite, visibly low-assurance self-approval warrant.
- Governed: independent review and separate approval evidence suitable for
  later control mapping; Familiar does not claim compliance certification.
- No policy: no merge or deployment.

## Reconciliation record

- PRDs 001–031, 033–034, and 039 are implemented and archived.
- PRDs 032, 035–038, 041, and 042–045 are active; checkpointed recovery
  is now available for their execution. PRD-040 is superseded by 042–045.
  PRDs 042–045 are this backlog's first contract-v1 front-matter documents.
- PRDs 028 and 029, and the completion intent for PRD-027, were recovered from
  `origin/prd-023-location-is-truth`; divergent old implementation commits were
  not merged.
- Historical wave-one documents in `done/` retain their legacy naming.
- PRDs 051–056 were drafted 2026-08-30 by autonomous specification runs
  as `status: draft`; owner approval flips them to `ready`. The provisional
  PRD-051+ numbering inside `docs/architecture/delivery-backlog.md` is that
  document's own superseded scheme, not the canonical sequence.

No specification is deleted merely because it is old. A future deletion must
name the duplicate or superseding PRD and preserve the decision in Git.
