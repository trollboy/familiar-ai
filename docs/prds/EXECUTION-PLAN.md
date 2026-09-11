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

**GATE — PRD-076 scope modularization: COMPLETE 2026-09-01.** `config.rs`
is now a `config/` module tree (`providers.rs`, `registry.rs`, `driver.rs`,
`review.rs`, `execution.rs`, `delivery.rs`, `daemon.rs`, `inference.rs`,
and others), re-exported unchanged; `docs/contracts/providers.md` is now
`docs/contracts/providers/` (an index plus `inference-providers.md`,
`credential-authentication.md`, `registry-migration.md`); the CLI binary
is dispatch-only with subcommand bodies under
`crates/familiar-ai-daemon/src/bin/cli/` (that path, not `src/cli/`, because
`src/cli.rs` was already taken by the unrelated `familiar-ai-daemon` binary's
own tiny `Cli` struct; a binary's `mod cli;` resolves inside its own
`src/bin/` directory). Every pending PRD's `expected_files`
is amended below to the narrowed forms this authorizes; the row widths
are the scheduler's own `achievable_width` computation
(`crates/familiar-ai-daemon/src/drive.rs`) against those amended
declarations — not an estimate — pinned in
`crates/familiar-ai-review/tests/scope_narrowing.rs`.

**Wave definition, restated so it cannot drift again:** a row below is one
wave only if every PRD in it is *simultaneously* dependency-ready AND
mutually scope-disjoint under the scheduler's conflict rule (identical
exact files conflict; a directory conflicts with anything nested under
it; `crates/familiar-ai-storage/migrations/` is exempt per PRD-066's
own-numbering safety). "Achievable width" is the largest such
mutually-disjoint subset the scheduler can actually admit at once, not
the count of dependency-ready PRDs (that count is "graph width").

| Wave | PRDs | Graph width | Achievable width |
|------|------|-------------|------------------|
| bug gate | 077, 078, 079 **done** → 080 (last) | 4 | 1 remaining |
| gate | 076 **done 2026-09-01** | 1 | 1 |
| 5 | 038, 053, 058 | 3 | **2** — 038 is disjoint from both 053 and 058; 053 and 058 still collide on `crates/familiar-ai-daemon/src/` (058's own whole-directory declaration) and `config/default.toml` — narrowing those two is 058's own call to make if it wants to co-run with 053, the same standing offer PRD-066 gave 052/054 over `billing.rs` |
| 6 | 059, 060, 061, 063, 072 | 5 | **1** — `docs/contracts/providers.md` and `crates/familiar-ai-core/src/config.rs` no longer collide (each PRD now owns a distinct contract file and, for 072, a distinct config module), but all five still share whole-directory declarations of `crates/familiar-ai-agent/src/` and `crates/familiar-ai-llm/src/`; PRD-076's scope was the four named surfaces (config, contracts, CLI, test/repo dirs), not every crate's `src/`, so this pair remains each PRD's own opportunity to narrow into per-adapter files (`agent/src/anthropic.rs`, `llm/src/backends/anthropic_http.rs`, etc.) if a future session wants this wave wider than 1 |
| 7 | 071, 073 | 2 | **2** — fully disjoint after narrowing (`review.rs`/`accounting.rs`-style repo files, distinct CLI and config modules, distinct test files) |

This is a real reduction (from a ~23-PRD width-one chain to four rounds
with two achieving width 2) but falls short of the PRD-076 objective's
"nine rounds of width two to five" for wave 6 specifically, because that
estimate assumed per-adapter disjointness in `crates/familiar-ai-agent/src/`
and `crates/familiar-ai-llm/src/` that PRD-076's authorized scope does not
reach. Recorded here rather than smoothed over, per the plan's own
"tell the truth" mandate.

## Critical path

**077 → 076 (done) → 058 → {059, 060, 061, 063 — serialized at width 1
until they narrow `agent/src`/`llm/src` themselves} → {071, 073}** — 056
landing moved the control plane off the path; the raw runtime (058) is
now the long pole, and it is ready the moment the gates clear. 038
(multi-repo acceptance — the forcing function that ends infrastructure
work) and 053 are dependency-ready NOW and must not keep slipping:
schedule 038 in the first product session after the gates, alongside one
of {053, 058} at width 2.

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
