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

~~The waves-3/4 caveat: Familiar had never integrated a multi-PRD wave
autonomously (FAM-BUG-019/022).~~ **Ended 2026-09-01 by wave 5:** the
scheduler ran the computed width live (038∥053, 058 admitted on hold
release), 053 and 058 completed hands-off through clean independent
review and merge-queue integration, and 038 landed under the owner's
recorded scope approvals, waiver, and manual completion override
(FAM-BUG-044 tracks the waiver-identity gap that required the override).
Recommendation standing with the owner: close 022; close 019 with the
044 note.

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
- **PRD-080 — scope authority refinement** (bug 014; the wave-3 PRD-050
  and wave-4 PRD-055 scope walls) follows.
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

| Wave | PRDs | Graph width | Achievable width |
|------|------|-------------|------------------|
| bug gate | 077, 078, 079, 080 — **complete** | 4 | done |
| gate | 076 **complete** | 1 | 1 |
| 5 | 038, 053, 058 — **ALL LANDED 2026-09-01** | 3 | 2 — achieved live exactly as computed: 038∥053 in parallel, 058 admitted the moment 053 released the `config/default.toml` hold |
| 6 | 059, 060, 061, 063, 072 | 5 | 3 — `059`/`060`/`061` still share `crates/familiar-ai-core/src/config/providers.rs` (each adds its own `InferenceRuntimeKind` variant to the same closed enum; splitting that enum is a semantic change, out of this PRD's scope) and serialize pairwise; `063` (registry_workers.rs) and `072` (agent_runtime.rs) are disjoint from that trio and from each other, so `{one of 059/060/061, 063, 072}` run together |
| 7 | 071, 073 | 2 | 2 — fully disjoint post-076 (`config/review.rs` vs `config/registry_workers.rs`, `cli/batch_review.rs` vs `cli/model_residency.rs`, distinct repo/test files) |

## Critical path

**{059 | 060 | 061, 063, 072} → {071, 073}** — everything before this is
landed as of 2026-09-01. Wave 6 runs at achievable width 3 with the
059/060/061 providers-enum trio serializing pairwise through the merge
queue; wave 7 is fully disjoint at width 2. Seven PRDs remain in the
entire approved backlog. Per the bug policy, FAM-BUG-044 and frictions
007/008 (delivery-machinery, direct-fix lane) are the first work of the
next session, before wave 6.

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
