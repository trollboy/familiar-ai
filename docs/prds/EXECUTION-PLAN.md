# Backlog Execution Plan — updated 2026-09-01 (PRD-076)

**Authority:** `docs/north-star.md`; backlog index in `ROADMAP.md`.
**Definition (owner's): a wave (equivalently, a round) is a batch of PRDs
runnable simultaneously — dependency-ready AND mutually scope-disjoint.**
Scope-disjoint is not a judgment call: it is the scheduler's own
`achievable_width` partition (`crates/familiar-ai-daemon/src/drive.rs`) —
two PRDs conflict exactly when a declared expected-file scope entry overlaps
another's (exact-file equality, or directory-prefix containment either way),
except `crates/familiar-ai-storage/migrations/`, which is always exempt
(PRD-066 owns per-PRD migration numbering). The rows below are that
function's actual output against this document's declared `expected_files`,
not an estimate.
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
sign-off on scope-conformant work. **PRD-076's approval is also this
document's standing authorization to narrow the `expected_files` of every
PRD listed below** to the per-feature module/contract/CLI files the config,
provider-contract, and CLI-binary splits created; each amended PRD's new
content hash reconciles as a normal document update.

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
became `crates/familiar-ai-core/src/config/` (per-feature modules,
re-exported, proven behavior-preserving by a pinned-fixture equivalence
test); `docs/contracts/providers.md` became an index over
`providers-{inference,billing,deploy-targets,credentials,registry-migration}.md`;
`bin/familiar-ai.rs` thinned to dispatch, with `billing`, `config`, and
`worker` subcommand implementations moved to `crates/familiar-ai-daemon/src/cli/`.
Every pending PRD below declares the narrowed per-feature file it actually
needs instead of the four shared surfaces. The rows below are
`achievable_width`'s computed output against those narrowed declarations —
not the pre-076 estimate.

| Round | PRDs (dependency-ready) | Graph width | Achievable width | Conflict driving the gap |
|-------|--------------------------|-------------|-------------------|---------------------------|
| bug gate | 077, 078, 079 **done** → 080 folded into round 1 below | — | — | — |
| 1 | 038, 053, 058, 080 | 4 | **2** | 053, 058, 080 pairwise conflict on `crates/familiar-ai-daemon/src/` (058's whole-directory scope), `crates/familiar-ai-storage/src/repos/`, and `crates/familiar-ai-storage/src/repos/orchestration.rs`; 038 conflicts with none. Run `{038, 058}` first — it unlocks the most follow-on work. |
| 2 | 053, 059, 060, 061, 063, 072, 080 | 7 | **2** | 059/060/061/063/072 form a five-way clique on `crates/familiar-ai-agent/src/`, `crates/familiar-ai-llm/src/`, and (for the four adapters) `docs/contracts/providers-inference.md` + `crates/familiar-ai-core/src/config/providers.rs` + `config/default.toml` — none of those are surfaces PRD-076 split. 053 also conflicts with the four adapters via `config/default.toml`. Run `{059, 080}` — 059 unlocks 071. |
| 3 | 053, 060, 061, 063, 071, 072 | 6 | **3** | Same agent/llm clique among 060/061/063/072. 071 and 053 are disjoint from everything else in this round. Run `{053, 071, 072}`. |
| 4 | 060, 061, 063 | 3 | **1** | Full clique (agent/src, llm/src, providers-inference.md, config/providers.rs, config/default.toml) — this is the real remaining serialization point, and it is **out of PRD-076's declared scope**: narrowing `crates/familiar-ai-agent/src/` and `crates/familiar-ai-llm/src/` per adapter would be a PRD-059/060/061/063-owned follow-on, mirroring how PRD-076 left `billing.rs`'s per-provider split to PRD-052/054. Run `{060}`. |
| 5 | 061, 063 | 2 | **1** | Same clique, one member down. Run `{061}`. |
| 6 | 063 | 1 | **1** | Last clique member. Completing it unlocks 073. |
| 7 | 073 | 1 | **1** | Only PRD left; nothing to conflict with. |

Rounds 4–7 are genuinely width-1 — not because PRD-076 failed to recover
concurrency, but because the four raw-inference adapters share mutable code
(`crates/familiar-ai-agent/src/`, `crates/familiar-ai-llm/src/`) that this
PRD's scope explicitly does not touch (see Scope: "no new features, no
semantic changes"). Rounds 1–3 are the actual recovery this PRD delivers:
three real rounds of width 2–3 replace what was previously a flat
twenty-three-round, width-one chain end to end.

## Critical path

**077 → 076 → {038, 058} → {059, 080} → {053, 071, 072} → 060 → 061 → 063 → 073**
— 056 landing moved the control plane off the path; the raw runtime (058)
is still the long pole, and it is now ready. 038 (multi-repo acceptance —
the forcing function that ends infrastructure work) is dependency-ready now
and must not keep slipping: it runs in round 1, not deferred behind the
adapter family.

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
