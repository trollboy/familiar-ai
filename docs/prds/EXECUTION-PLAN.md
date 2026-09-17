# Backlog Execution Plan — updated 2026-09-16

**Authority:** `docs/north-star.md`; backlog index in `ROADMAP.md`.
**Definition (owner's): a wave is a batch of PRDs runnable simultaneously —
dependency-ready AND mutually scope-disjoint.**
**Policy: bugs preempt the backlog** (owner, 2026-08-31): open
`docs/running_bugs.md` entries outrank all planned work; bug remediation is
the first work of every session; "transferred to PRD-X" is valid only when
PRD-X runs next; new bugs go to the top, never the end.

**Approval:** the 2026-08-31 batch (038, 053, 058–061, 063, 071–073,
076–080) was approved for implementation and is now **fully landed and
exhausted**. PRDs 077–080 were the bug-carrier family created from waves
3–4 after-action findings. Every economy mechanism still defaults off and
promotes only on a recorded PRD-051 measurement. This document remains the
owner's standing authorization for that batch: workers must not pause for
per-PRD plan sign-off on scope-conformant work within it.

**The 082+ queue carries no batch approval yet.** It was not part of the
2026-08-31 authorization, and this document does not extend approval to it
— that is the owner's gate to open, and PRD-019's model says it opens over
a *decomposition*, which for 082+ has not been reviewed as a batch. Until
then the 082+ PRDs are executed one at a time under ordinary per-PRD
approval, not under standing authorization.

## Completed

| Wave | PRDs | Outcome |
|------|------|---------|
| 1 | 036, 037, 044, 045, 046, 047 | complete 2026-08-30 — width 1; [after-action](../wave1_afteraction_report.md) |
| gate | 065 | complete 2026-08-30 |
| 2 | 041, 048, 049, 051 | complete 2026-08-30 — width 1, manual landing; [after-action](../wave2_afteraction_report.md) |
| gate | 066, 067, 068 | complete 2026-08-31 |
| 3 | 050, 052, 054, 057, 064, 069, 070, 074, 075 | complete 2026-08-31 — **all nine retained; integrated manually**; [after-action](../wave3_afteraction_report.md) |
| 4 | 032, 055, 056, 062 | complete 2026-08-31 — **cascade-then-manual again**; [after-action](../wave4_afteraction_report.md) |
| 5 | 038, 053, 058 | complete 2026-09-01 — **1 of 3 integrated by Familiar** (053); 038 and 058 retained `scope_ambiguous` and landed by hand |
| 6 | 059, 060, 061, 063, 072 | complete 2026-09-03 — **0 of 5 integrated**; all retained (`scope_broadened`/`scope_ambiguous`) and landed by hand |
| 7 | 071, 073 | complete — **0 of 2 integrated**; 071 retained `human_review_required`, 073 never attempted by the driver at all |
| — | 081, 083, 084, 087, 091 | complete 2026-09-05..09 — **1 of 5 integrated** (081); 083/087/091 retained, 084 never attempted |

**The waves-3/4 caveat stands. It was retired on 2026-09-01 and reinstated
on 2026-09-16 against the execution ledger.**

The 2026-09-01 entry claimed wave 5 ended it: "053 and 058 completed
hands-off through clean independent review and merge-queue integration."
`~/.local/share/familiar-ai/familiar.db` records one attempt row per PRD
in that wave and says otherwise — **053 completed and integrated; 038
and 058 were both retained `scope_ambiguous` and never carry an
`integrated_at`.** Wave 6 (059/060/061) integrated nothing at all.
Lifetime totals across the whole project: 39 attempts, **2** completed,
**2** integrated (PRD-53 on 09-01, PRD-81 on 09-04), in separate
single-PRD sessions.

What genuinely worked, and is worth keeping, is the *designed pause*:
`scope_ambiguous` pausing a candidate, freeing its slot so siblings
continue, and resuming on a recorded owner decision. That is PRD-080
working as specified, and the 038 description above — landed under
recorded scope approvals, a waiver, and a manual completion override —
was accurate about 038. What was never true is the step after the pause:
no resumed candidate has ever reached integration through Familiar.

**Familiar has never integrated a multi-PRD wave. The largest wave it has
ever integrated is one PRD.** FAM-BUG-019 and FAM-BUG-022 are reopened as
of 2026-09-16; 019's exit criterion is unchanged and unmet. PRD-098 makes
this class of claim a shipped query instead of an audit note, so the next
closure is decided by the table rather than by the narrative.

## Remaining waves

**GATE — the bug wave (runs before all product work, per policy):**

- **PRD-077 — autonomous wave delivery: COMPLETE 2026-08-31** (implemented
  directly by Claude; bugs 012, 018, 019, 021, 022, the 009 circuit
  breaker). The FAM-BUG-019 closure regression passes: a two-PRD
  shared-scope wave completes end to end through drive alone — clean
  review, merge-queue integration in order, the second PRD provably built
  on the first's integrated base. Live confirmation lands with the M1's
  next real wave.
- **PRD-078 — preflight/verification contract: COMPLETE 2026-08-31** and
  **PRD-079 — capability-probed review routing: COMPLETE 2026-08-31** —
  implemented in parallel by Codex on the M1 while Claude implemented 077
  (the bug wave genuinely ran at width 3 across two machines, human-
  orchestrated). Bugs 011/015/020/024 and 013/025 fixed; 024/025
  live-verified 2026-09-01. The predicted migration-052 collision happened
  and was repaired (079 → migration 053, collision ledger in
  running_bugs).
- **PRD-080 — scope authority refinement: COMPLETE 2026-09-02** (bug 014;
  the wave-3 PRD-050 and wave-4 PRD-055 scope walls). All four acceptance
  criteria verified against the code and proven live in wave 6: declared
  manifests mint standing approval (`declared_expected_file`), declared
  test surfaces are in-scope (`declared_surface_coverage`), a genuine
  ambiguity pauses and frees the slot (PRD-60 and 61 continued while 59
  was paused) and deciding it resumes the workflow, and all 17 recorded
  decisions are durable with actor and reason beside a paste-runnable
  deciding command. Archived to `docs/prds/done/`. FAM-BUG-014 stays
  open, narrowed to the UNDECLARED manifest case.
- Direct fixes **done 2026-08-31**: bug 017 (provider-verify TOML) and
  bug 023 (legacy disabled-delivery deserialization), commit `fcf0aef`.
- New: **bug 027** — `worker_lock` simultaneous-fallback test flakes under
  parallel suite load (passes targeted); a flaky test inside the
  verification gate can halt unattended runs. Owner: PRD-078's
  environment/verification work or a direct fix.

**GATE — PRD-076 scope modularization: COMPLETE** — the four hot shared
surfaces (`config.rs`, `providers.md`, the CLI binary source, whole shared
test/source directories) are split into per-feature files and every
remaining pending PRD's `expected_files` is amended to a narrowed,
per-feature form. The rows below are regenerated as **true rounds**: a
round is the owner's wave definition applied literally — a set of PRDs
that is simultaneously (a) dependency-ready (every dependency is
`completed`) and (b) mutually scope-disjoint under the scheduler's own
conflict rules (`achievable_width` in `crates/familiar-ai-daemon/src/drive.rs`:
exact-file/directory-prefix overlap on the amended `expected_files`, with
`crates/familiar-ai-storage/migrations/` exempted per PRD-066's allocation).
Graph width is the round's PRD count; achievable width is
`achievable_width()` computed against the amended declarations below —
this is the same computation `familiar-ai backlog metadata-check` and
authoring-time plan validation use, not an estimate.

Every wave listed above is landed. The approved 038–073 backlog is
exhausted, and the queue is now PRDs 082, 085, 086, 088, 089, 090, 092,
093, 094, 095, 096 in `docs/prds/`, indexed in `ROADMAP.md`.

**These rounds are not regenerated here yet.** Recomputing achievable
width for the new queue is real work and would be the third consecutive
plan written against an unmeasured system. PRD-098 lands first and alone:
it is cheap, it makes both north-star metrics a shipped query, and it
reports the retained-reason histogram — so the next round table is
computed from what actually blocks delivery rather than from file-overlap
alone. Round regeneration for 082+ happens after 096 reports.

## Critical path

**096 → {083-class delivery blockers, chosen by the 096 histogram} → the
082+ queue.**

The ledger's own ranking of what stops delivery, lifetime:

| Retained reason | Count |
|---|---|
| Review precondition: PRD lacks an explicit Acceptance Criteria section | 6 |
| *(no reason recorded)* | 6 |
| `scope_broadened` | 5 |
| `scope_ambiguous` | 4 |
| `unclassified_result` | 3 |
| `interrupted` | 3 |
| `human_review_required` | 3 |

Two observations that should drive the next round rather than the
file-overlap width computation:

1. **Scope authorization refuses nine attempts** — the single largest
   cause, and PRD-080 already narrowed it once. The remaining
   `scope_broadened` cases are post-080 (PRD-63, 72, 87, 92 on 09-03 and
   09-05), so 080 did not close the class.
2. **Six attempts record no reason at all** and six die on a review
   precondition before a model is ever called. Neither is a hard problem;
   both are invisible without the histogram, which is why nobody has
   fixed them.

Per the bug policy, FAM-BUG-019 and FAM-BUG-022 are reopened and outrank
the product queue. Their exit criterion is unchanged: one multi-PRD wave
delivered using only Familiar commands.

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
