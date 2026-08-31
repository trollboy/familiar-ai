# Backlog Execution Plan — 2026-08-30

**Authority:** `docs/north-star.md`; backlog index in `ROADMAP.md`.
**Approval:** All 34 pending PRDs (032, 036–038, 041, 044–064, 066–073) are
approved for implementation. The owner-directed economy family
(2026-08-30): PRD-069 native token compression (caveman functionality
replicated first-party, no external dependency), PRD-070 daemon context
service (incremental repo map, prefix-stable serving, per-project
token-sink reports), PRD-071 batch-tier independent review (half-price
async review under the 056 control plane), PRD-072 raw-runtime token
discipline (targeted edits, bounded tool output), PRD-073 warm local model
residency. Every economy mechanism defaults off and promotes only on a
recorded PRD-051 measurement. The 046 and 051–064 family received adversarial
review and sign-off by Sol 5.6; owner batch-approved the full set 2026-08-30. Every
frontmatter PRD is `status: ready`. This document is the owner's standing
authorization: workers must not pause for per-PRD plan sign-off on
scope-conformant work.

## Dependency waves

**Wave 1 completed 2026-08-30.** See
[`../wave1_afteraction_report.md`](../wave1_afteraction_report.md) for delivery
evidence and the orchestration defects observed during execution.

**GATE SATISFIED (2026-08-30): PRD-065 merged; Wave 2 is unblocked.**
Wave 1 exposed four orchestration defects (after-action defects 1–3 and 7);
PRD-065 fixed all four: the scheduler now admits the full ready set
(dependencies are admission gates, not mutual-exclusion edges; two ready
PRDs serialize only on overlapping expected-file scopes, with every
selection/deferral persisted in `driver_selection_decisions`), the session
warrant accepts a repeatable `--prd` allowlist selection can never escape,
worktrees resolve repository policy through Git common-directory identity,
and `backlog approve-and-complete` completes a reviewed checkpoint in one
transaction binding the approved hash and commit. A width-six regression
test on the recorded Wave 1 graph pins the fix.

**Wave 2 completed 2026-08-30.** See
[`../wave2_afteraction_report.md`](../wave2_afteraction_report.md): all four
PRDs integrated, but at achievable width ONE (every wave-2 PRD overlapped in
configuration, storage, or run surfaces), with stale-base composition
defects (duplicate migration numbers, completion before integration) and
substantial manual landing work.

**GATE (2026-08-30): PRDs 066, 067, and 068 block Wave 3 — every wave-2
defect is fixed before new product work runs.**

- **PRD-066** — integration-aware parallel orchestration: the merge queue
  (parallel execution, ordered landing; scope release and completion at
  integration, not review), continuous admission replacing batch lockstep,
  authoring-time achievable-width validation, migration-number allocation,
  hash-bound scope decisions, reconciled recovery, live worker heartbeats.
  Covers defects 1–4 and 13–15. Runs first and alone:
  `familiar-ai drive --max-prds 1 --prd PRD-066`.
- **PRD-067** — durable verification truth: dispositions computed from
  durable check records (narration that contradicts them fails the phase),
  open findings block clean completion absent a recorded human waiver,
  environment-denied checks classified distinctly with preflighted
  writable paths. Covers defects 7, 8, and 17.
- **PRD-068** — driver hygiene: precise context-failure classification
  with continue-past-retained sessions, model-cache invalidate-once,
  metadata-check strict/advisory exit modes. Covers defects 9, 16, 18.

067 and 068 run after 066 lands, through 066's own merge queue, as one
session — their declared scopes are disjoint (review crate + run.rs +
dedicated test file, versus context/agent/drive/bin + dedicated test
file), making them the first honest width-2 test of the new machinery:
`familiar-ai drive --max-prds 2 --prd PRD-067 --prd PRD-068`.

A PRD may start only when its dependencies are complete AND it is inside
the session's approved allowlist — the earlier "wave boundaries are
guidance, not barriers" language is retracted; it authorized the Wave 1
boundary escape (defect 2). Warrant each wave's session with its PRD set.

The width columns are honest per wave-2 defect 1: **graph width** is what
the dependency graph permits; **achievable width** is what the declared
expected-file scopes and resources permit under the PRD-065 conflict rules.
PRD-066 plan validation computes achievable width with the scheduler's scope
and resource conflict rules. Historical measured values remain labelled;
future authored waves must validate their claimed width before admission.
Narrowing a wave's `expected_files` raises its achievable width; coarse
whole-crate declarations forfeit concurrency by design.

| Wave | PRDs | Graph width | Achievable width |
|------|------|-------------|------------------|
| 1 | 036, 037, 044, 045, 046, 047 — **completed 2026-08-30** | 6 | 1 (measured) |
| gate | 065 — orchestration reliability — **completed 2026-08-30** | 1 | 1 |
| 2 | 041, 048, 049, 051 — **completed 2026-08-30** | 4 | 1 (measured) |
| gate | 066 — integration-aware parallel orchestration | 1 | 1 |
| gate | 067, 068 — verification truth, driver hygiene | 2 | 2 (disjoint by construction) |
| 3 | 050, 052, 054, 057, 064, 069, 070 | 7 | ~3–4 (069/070's new crates are disjoint) |
| 4 | 032, 055, 056, 062 | 4 | ~2 |
| 5 | 038, 053, 058 | 3 | ~2 |
| 6 | 059, 060, 061, 063, 072 | 5 | ~3–4 (per-adapter files disjoint; 072 shares 058's surfaces) |
| 7 | 071, 073 | 2 | ~2 (batch review vs model residency are disjoint) |

## Critical path

**066 → 064 → 056 → 058 → {059, 060, 061, 063}** (044, 051 completed) —
five PRDs deep with the gate; no amount of parallelism shortens it. Within
any wave, schedule the critical-path member ahead of its siblings.

Secondary chains: 032 → 038 (have-at-it acceptance; 041 completed) and
057 → 062 → 063 (local-model execution).

## Scheduling guidance

- Use one worktree per PRD. After PRD-066, candidates land through the
  session merge queue in review order and successors branch from the
  session integration revision; do not merge worktrees to `main` by hand
  mid-session.
- Real concurrency is bounded by the achievable-width column, not the graph
  column. Authors who want width declare file-level `expected_files` and
  explicit `resources`, not whole-crate directories.
- Heavy items that will pace their waves: 038 (multi-repo acceptance),
  058 (raw runtime), 056 (control-plane migration).
- Run `familiar-ai backlog metadata-check --advisory` as the unattended
  pre-wave gate. It reports the 41 legacy PRDs as migration debt without a
  failing exit; every structured-v1 diagnostic remains blocking. Use
  `--strict` when legacy migration debt must also fail the check.

## Human-approval policy for unattended execution

Approvals are batched, never ad-hoc. Concretely:

1. **Plan approval is pre-granted.** This document is the batch-approval
   gate (PRD-019 model) for every listed PRD. A worker implementing within
   a PRD's declared `expected_files`, `acceptance_criteria`, and
   `risk_classes` proceeds without asking.
2. **Risk acceptance follows declared tiering.** PRD-045 (wave 1) routes
   review by declared risk class: low-risk work completes on a clean
   independent review with no human in the loop; high-risk classes get
   independent review, and only a reviewer-flagged unresolved objection
   escalates to the owner.
3. **Scope deviation blocks, it does not ask.** New dependency edges,
   changes to expected files or acceptance criteria, failed external
   gates, or anything that fails closed marks the PRD `blocked` with a
   recorded reason, and the driver moves to the next ready PRD. The graph
   is wide enough (~4 ready on average) that one blocked item never idles
   the fleet.
4. **Escalations queue; the owner drains the queue at wave boundaries.**
   PRD-041 (wave 2, prioritize early) is the mechanism that turns
   verification failures into recorded escalations instead of silent
   retries or mid-run prompts. The owner reviews the blocked/escalation
   queue once per wave (or daily), approves or amends in one sitting, and
   unblocked PRDs rejoin the ready set.
5. **Nothing outside this backlog is authorized.** Work not traceable to a
   listed PRD's scope requires a new PRD and a new approval, not an
   in-flight judgment call.
