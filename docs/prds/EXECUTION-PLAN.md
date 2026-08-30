# Backlog Execution Plan — 2026-08-30

**Authority:** `docs/north-star.md`; backlog index in `ROADMAP.md`.
**Approval:** All 26 pending PRDs (032, 036–038, 041, 044–064) are approved
for implementation. The 046 and 051–064 family received adversarial review
and sign-off by Sol 5.6; owner batch-approved the full set 2026-08-30. Every
frontmatter PRD is `status: ready`. This document is the owner's standing
authorization: workers must not pause for per-PRD plan sign-off on
scope-conformant work.

## Dependency waves

**Wave 1 completed 2026-08-30.** See
[`../wave1_afteraction_report.md`](../wave1_afteraction_report.md) for delivery
evidence and the orchestration defects observed during execution.

**HARD GATE (2026-08-30): PRD-065 blocks Wave 2.** Wave 1 exposed four
orchestration defects (after-action defects 1–3 and 7) that make parallel
execution untrustworthy: the scheduler serializes to width one, the warrant
cannot confine a session to an approved PRD set, lease worktrees lose
policy resolution, and reviewed checkpoints cannot complete
transactionally. PRD-065 (ready-set scheduling and session warrant
integrity) must be implemented, reviewed, and merged before any Wave 2
PRD is claimed. No other work runs concurrently with it.

The remaining graph resolves into the waves below. A PRD may start only
when its dependencies are complete AND it is inside the session's approved
allowlist — the earlier "wave boundaries are guidance, not barriers"
language is retracted; it authorized the Wave 1 boundary escape (defect 2).
Until PRD-065's `--prd` allowlist exists, each drive session must be
warranted for exactly one wave's PRD set.

| Wave | PRDs | Width |
|------|------|-------|
| 1 | 036, 037, 044, 045, 046, 047 — **completed 2026-08-30** | 6 |
| gate | 065 — orchestration reliability, runs alone | 1 |
| 2 | 041, 048, 049, 051 | 4 |
| 3 | 050, 052, 054, 057, 064 | 5 |
| 4 | 032, 055, 056, 062 | 4 |
| 5 | 038, 053, 058 | 3 |
| 6 | 059, 060, 061, 063 | 4 |

## Critical path

**065 → 051 → 064 → 056 → 058 → {059, 060, 061, 063}** (044 completed) —
still six PRDs deep with the gate; no
amount of parallelism shortens it. PRD-044 starts first and PRD-051 is the
widest gate (ten downstream PRDs). Within any wave, schedule the
critical-path member ahead of its siblings.

Secondary chains: 044 → 041 → 032 → 038 (have-at-it acceptance) and
047 → 057 → 062 → 063 (local-model execution).

## Scheduling guidance

- Peak useful concurrency is 6 workers (wave 1); average ready-set width is
  ~4. Use one worktree per PRD; merge back to `main` on completion so
  downstream waves build on integrated state.
- Estimated duration at full parallelism: 6–8 working days of
  implementation; ~10–12 calendar working days including review,
  remediation, and merge friction. Serial execution would be 4–6 weeks.
- Heavy items that will pace their waves: 037 (burn-in), 038 (multi-repo
  acceptance), 058 (raw runtime).
- Run `familiar-ai backlog metadata-check` before starting a wave; the
  frontmatter is authoritative for status and dependencies.

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
