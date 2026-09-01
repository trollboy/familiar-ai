# Backlog Execution Plan — updated 2026-08-31

**Authority:** `docs/north-star.md`; backlog index in `ROADMAP.md`.
**Definition (owner's): a wave is a batch of PRDs runnable simultaneously —
dependency-ready AND mutually scope-disjoint.**
**Policy: bugs preempt the backlog** (owner, 2026-08-31): open
`docs/running_bugs.md` entries outrank all planned work; bug remediation is
the first work of every session; "transferred to PRD-X" is valid only when
PRD-X runs next; new bugs go to the top, never the end.

**Approval:** All 15 pending PRDs (038, 053, 058–061, 063, 071–073,
076–080) are approved for implementation. PRDs 077–080 are the bug-carrier
family created 2026-08-31 from waves 3–4 after-action findings and run
first under the bug policy. Every economy mechanism still defaults off and
promotes only on a recorded PRD-051 measurement. This document is the
owner's standing authorization: workers must not pause for per-PRD plan
sign-off on scope-conformant work.

## Completed

| Wave | PRDs | Outcome |
|------|------|---------|
| 1 | 036, 037, 044, 045, 046, 047 | complete 2026-08-30 — width 1; [after-action](../wave1_afteraction_report.md) |
| gate | 065 | complete 2026-08-30 |
| 2 | 041, 048, 049, 051 | complete 2026-08-30 — width 1, manual landing; [after-action](../wave2_afteraction_report.md) |
| gate | 066, 067, 068 | complete 2026-08-31 |
| 3 | 050, 052, 054, 057, 064, 069, 070, 074, 075 | complete 2026-08-31 — **all nine retained; integrated manually**; [after-action](../wave3_afteraction_report.md) |
| 4 | 032, 055, 056, 062 | complete 2026-08-31 — **cascade-then-manual again**; [after-action](../wave4_afteraction_report.md) |

The waves-3/4 caveat is the plan's dominant fact: Familiar has never
integrated a multi-PRD wave autonomously (FAM-BUG-019/022). The delivered
code is real; the delivery process was the operator. The bug gate below
exists to end that.

## Remaining waves

**GATE — the bug wave (runs before all product work, per policy):**

- **PRD-077 — autonomous wave delivery: COMPLETE 2026-08-31** (implemented
  directly by Claude; bugs 012, 018, 019, 021, 022, the 009 circuit
  breaker). The FAM-BUG-019 closure regression passes: a two-PRD
  shared-scope wave completes end to end through drive alone — clean
  review, merge-queue integration in order, the second PRD provably built
  on the first's integrated base. Live confirmation lands with the M1's
  next real wave.
- **PRD-078 — preflight/verification contract** (bugs 011, 015, 020, 024)
  and **PRD-079 — capability-probed review routing** (bugs 013, 025) run
  as a pair after 077 — their declared files are disjoint.
- **PRD-080 — scope authority refinement** (bug 014; the wave-3 PRD-050
  and wave-4 PRD-055 scope walls) follows.
- Direct fixes **done 2026-08-31**: bug 017 (provider-verify TOML) and
  bug 023 (legacy disabled-delivery deserialization), commit `fcf0aef`.
- New: **bug 027** — `worker_lock` simultaneous-fallback test flakes under
  parallel suite load (passes targeted); a flaky test inside the
  verification gate can halt unattended runs. Owner: PRD-078's
  environment/verification work or a direct fix.

**GATE — PRD-076 scope modularization** (owner-approved 2026-08-30):
still required — the remaining product PRDs (058, 063, 072 especially)
declare whole-crate scopes that would serialize waves 5–6. Regenerates
the rows below as computed true rounds after amending the remaining
PRDs' declarations.

| Wave | PRDs | Graph width | Achievable width |
|------|------|-------------|------------------|
| bug gate | 077 **done** → 078 ∥ 079 (Codex), then 080 | 4 | 2 → 1 |
| gate | 076 | 1 | 1 |
| 5 | 038, 053, 058 | 3 | ~2 (all three are dependency-ready today) |
| 6 | 059, 060, 061, 063, 072 | 5 | ~3–4 post-076 (per-adapter files disjoint) |
| 7 | 071, 073 | 2 | 2 |

## Critical path

**077 → 076 → 058 → {059, 060, 061, 063} → {071, 073}** — 056 landing
moved the control plane off the path; the raw runtime (058) is now the
long pole, and it is ready the moment the gates clear. 038 (multi-repo
acceptance — the forcing function that ends infrastructure work) and 053
are dependency-ready NOW and must not keep slipping: schedule 038 in the
first product session after the gates.

## Scheduling guidance

- One worktree per PRD; candidates land through the session merge queue;
  do not merge worktrees to `main` by hand — if that becomes necessary,
  it is a FAM-BUG-019 recurrence and goes in the bug log, not just the
  terminal history.
- Real concurrency is bounded by the achievable-width column. Authors
  declare file-level `expected_files` and explicit `resources`, never
  whole-crate directories.
- `familiar-ai backlog metadata-check --advisory` is the unattended
  pre-wave gate; `--strict` when legacy migration debt must fail.
- Operator setup reference: `docs/guides/provider-setup.md`.

## Human-approval policy for unattended execution

Approvals are batched, never ad-hoc. Concretely:

1. **Plan approval is pre-granted.** This document is the batch-approval
   gate (PRD-019 model) for every listed PRD. A worker implementing within
   a PRD's declared `expected_files`, `acceptance_criteria`, and
   `risk_classes` proceeds without asking.
2. **Declared manifests are pre-authorized.** A `Cargo.toml`/`Cargo.lock`
   path in a PRD's `expected_files` carries this plan's standing approval;
   PRD-080 makes that a minted hash-bound decision at admission.
3. **Risk acceptance follows declared tiering** (PRD-045): low-risk work
   completes on a clean independent review; high-risk classes get
   independent review, and only an unresolved reviewer objection escalates.
4. **Scope deviation pauses with a decidable finding** (PRD-080), or
   blocks with a recorded reason; the driver moves to the next ready PRD.
5. **Escalations queue; the owner drains the queue at wave boundaries.**
6. **Nothing outside this backlog is authorized.** Work not traceable to a
   listed PRD's scope requires a new PRD and a new approval.
