# Backlog Execution Plan — updated 2026-09-18

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

## 2026-09-22 — release 1.0 definition and queue audit

**Definition (owner, 2026-09-22). Familiar 1.0:**

1. takes a collection of PRDs — the planner is not on the 1.0 path;
2. parallelizes and implements them at the width the scope graph allows;
3. optimizes each stream against token spend;
4. optimizes the architecture against token spend, with multi-model
   routing that is cheap-first and empirical — *"use the local Ollama
   model if you can get away with it, then Claude for the heavy lifting,
   Codex for tests, or whichever you have the best luck with"* — and with
   Familiar's own loop as a selectable peer of the vendor CLIs, on both
   product and cost grounds;
5. implements every PRD with testing as far as possible, stopping only at
   a genuine user-permission gate, which it passes through to the user.

The bar: **faster and cheaper than five people using Claude.** Fire and
forget, but not at the cost of the runway. PRD-098 now carries the
baseline as a declared number the report divides by.

Under that definition a stop is legitimate only if it is a decision that
is the user's to make. Every other stop in the ledger — infrastructure,
adapter bug, package cap, checkpoint bug, reviewer incapacity — is a
defect by definition, not a refusal to preserve.

**What the audit found and changed.** Details in each PRD's dated
section; the summary:

| PRD | Finding | Action |
|---|---|---|
| 082, 085, 090, 095, 096, 099, 102 | landed, still in the queue | not edited (completion is immutable); archive is the owner's act. 102's six criteria should be checked against the tray crate's tests first |
| 097 | SITREP said landed; merge `deec7f8` carried only the document | corrected; stays `draft`, 1.x |
| 105 | successor to a PRD that has not integrated (PR #19) | `blocked`; findings belong in PR #19 |
| 088 | motivating bugs fixed; AC3 duplicated 098; AC5 contradicted 099 | `draft`, 1.x; AC3 struck, AC5 restated |
| 089 | its exclusion was removed 2026-09-21 | `draft`, 1.x; AC5 struck; `scripts/gate.sh` declared |
| 092 | gate now exists; `scripts/` was a bare directory claim | depends on 099; declares `scripts/gate.sh`; AC1 restated against `gate.sh` |
| 093 | `blocked` already honoured, `draft` not; FAM-BUG-051's admission half unowned | AC7 narrowed to `draft`; AC8 added |
| 094 | overlapped 100 on the local path | depends on 100; local resolves through the owned loop |
| 086 | review stage never costed (FAM-BUG-061); local basis unorderable | two criteria added; framed as 1.0 |
| 098 | its ledger table mixed two repositories; five merge commits read as two | corrected in place; repository scope, `integrated_at` on every landing path, and the human baseline added |
| 103 | assumed 090 unmerged | corrected; named as the first hands-off run |
| 104 | 14 criteria, 40 files, in progress on macOS, outside the batch grant | not edited; the owner's call |
| 100, 101 | accurate as written | unchanged |
| **106** (new, draft) | the hub files bound width and no PRD split them | migrations self-register, defaults per feature, reference out of README, width measured after |
| **107** (new, draft) | the 1.0 routing sentence had no PRD; every piece exists unwired | per-job-class ladder, one bounded escalation, probation live, **live-run acceptance** |

**The ledger the previous sections quote is wrong.** 13 of the 43
`driver_attempts` rows belong to `~/Projects/spectra` (ids `PRD-177a`,
`PRD 0177f`, August dates). The two largest rows of the histogram below,
"no Acceptance Criteria section" ×6 and "no reason recorded" ×6, are all
spectra rows, and round 1 was sequenced on them. On this repository: 30
attempts, 2 `completed`, 2 `integrated_at`, and 5 merge-queue commits on
`main` — the "resumed candidate" landing path never writes
`integrated_at`. Corrected table:

| Retained reason, this repository | Count |
|---|---|
| `verification_failed` (4 of 6 = FAM-BUG-059 Docker subnets) | 6 |
| `scope_broadened` | 5 |
| `scope_ambiguous` | 4 |
| `unclassified_result` | 3 |
| `interrupted` | 3 |
| `human_review_required` | 3 |
| `malformed_output` (FAM-BUG-034/038) | 2 |
| `integration_failed` | 1 |
| `checkpoint_failed` (FAM-BUG-037) | 1 |

**Bug work that precedes the queue, per the standing policy:** the
resume-landing path must write `integrated_at`; attempt queries must
filter by `driver_sessions.repository_key`; `scripts/gate.sh` still runs
`--no-default-features` so the tray is untested in the gate.

**1.0 order, dependency-ordered.** Widths are not restated here: the
rounds below were computed by `achievable_width()` against declarations
this audit changed, and `familiar-ai operator width` refuses while the
daemon holds the control-plane claim. Regenerate after archiving.

1. **PRD-103, hands-off, no-touch rule.** Two files, no dependencies,
   no hub overlap. Every stop becomes a bug entry naming the variant;
   nothing is fixed mid-run. This is the firing table, not a PRD.
2. **PRD-106 alone**, once approved — it conflicts with most of the
   queue by construction, as PRD-076 did.
3. ~~**PRD-100**~~ — landed by hand 2026-09-22 (`e16fc73`) after an
   independent review and four fixes on the branch; PRD-105 is `ready`
   with the review's remaining findings.
4. **086 ‖ 092 ‖ 093** — disjoint once 106 removes `migrate.rs` from
   086 and 093.
5. **094, 101** after 100; **098** after 086.
6. **PRD-107** after 086, 094 and 100 — its fifth criterion is the live
   run.

1.x: 088, 089, 097, 105 (until 100 lands), and 104 at the owner's
discretion.

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
Totals **on the Linux host**: 39 attempts, **2** completed, **2**
integrated (PRD-53 on 09-01, PRD-81 on 09-04), in separate single-PRD
sessions. This is one machine's slice, corrected 2026-09-17 — the M1 ran
the early work and every Codex execution against its own store, which
this box cannot read, and `driver_sessions` has no host column. There is
no project-wide aggregate, and that absence is itself the finding.

What genuinely worked, and is worth keeping, is the *designed pause*:
`scope_ambiguous` pausing a candidate, freeing its slot so siblings
continue, and resuming on a recorded owner decision. That is PRD-080
working as specified, and the 038 description above — landed under
recorded scope approvals, a waiver, and a manual completion override —
was accurate about 038. What was never true is the step after the pause:
no resumed candidate has ever reached integration through Familiar.

**No multi-PRD wave integration is recorded on this host; the largest
wave integrated here is one PRD.** Whether that holds for the M1 is
unknown and is the open question — if the Mac holds an integrated
multi-PRD wave, the original closure was right. FAM-BUG-019 and
FAM-BUG-022 are reopened as of 2026-09-16 pending that evidence; 019's
exit criterion is unchanged and unmet by anything visible here. PRD-098 makes
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
exhausted. **The rounds below are regenerated for the current queue and are
measured, not authored** — `achievable_width()` was called directly against
the live backlog on 2026-09-17 (`familiar-ai operator width` refuses while
the control-plane owner is live, so the same function was called through a
one-off harness; the numbers are the scheduler's own).

## Queue state, 2026-09-17 (superseded by the 2026-09-22 audit above)

Sixteen PRDs sit in `docs/prds/`. One of them is not remaining work:

- **PRD-095 — declared git and forge identity: IMPLEMENTED** in `833aa02`
  (config, delivery, `delivery_identity.rs`, the credential contract). It
  stays in `docs/prds/` only because archiving is the owner's separate act
  (`7ef6d26`). It is excluded from the rounds below.
- **PRD-096 and PRD-097 are authored, not implemented.** Their merges
  (`476ad81`, `deec7f8`) carried the PRD documents and nothing else —
  `crates/familiar-ai-daemon/tests/forge_adapters.rs` and
  `docs/contracts/forge-adapters.md` do not exist on any branch. Both are
  full work items in the rounds below.

That leaves **fifteen PRDs of remaining work**: 082, 085, 086, 088, 089,
090, 092, 093, 094, 096, 097, 098, 099, 100, 101.

Every one of them is dependency-satisfiable — every declared dependency is
either in `docs/prds/done/` already or is itself in this queue. Only three
carry an in-queue dependency edge: **089 → 088**, **098 → {085, 086}**,
**101 → 100**.

## Measured width

```
graph_width=16  achievable_width=6
```

Six is the ceiling for the whole active set, and no round below reaches it,
because the largest mutually-disjoint set is not also dependency-ready. The
binding constraint is not the dependency graph — it is four hub files:

| Contended file | PRDs declaring it |
|---|---|
| `config/default.toml` | 088, 089, 094, 097, 100, 101 |
| `README.md` | 090, 092, 098, 099, 101 |
| `crates/familiar-ai-core/src/config/registry_workers.rs` | 086, 094, 100, 101 |
| `crates/familiar-ai-daemon/src/run.rs` | 086, 088, 094, 100 |

Thirty-seven conflict edges exist across fifteen PRDs. **Twenty-two of them
are one of those four files.** PRD-076 split the previous generation of hot
surfaces per feature and the width went up; these four are the next
generation and nobody has split them. `README.md` in particular is a
documentation file serializing five product PRDs — splitting the operator
surface out of it, or declaring per-section ownership, is the cheapest
width purchase available and is worth its own PRD.

## Phase 0 — the gate's preconditions, COMPLETE 2026-09-18

No PRD covers this work, which is why it sat red. A required gate cannot be
landed onto a tree that fails its own checks: it would refuse every merge on
day one and the first thing anyone would do is switch it off. Four chores,
all verified inside the verification image rather than only on a
workstation:

- **rustfmt** (`170c420`) — 16 files, mostly import ordering left by the
  PRD-084 operator work.
- **clippy** (`1425e5d`) — `--workspace --all-targets -- -D warnings` now
  exits 0. It took four fixes, not one: clippy stops at the first failing
  crate, so each fix exposed the next. The last was a dead-code error that
  turned out to matter — `enqueue_initial_scan` was a leftover of the
  refactor to the durable `run_repository_scan`, and the only caller left
  was the sole test asserting that `node_modules/` and `target/` are
  skipped. That assertion was guarding code the product no longer runs. The
  function is gone and the test now covers the live path.
- **Toolchain pinned** (`b865530`) — `rust-toolchain.toml` at 1.93.1, both
  Dockerfile stages matched. Local ran 1.93 while the image pinned 1.88, so
  `clippy -D warnings` was red on one and green on the other. **This
  delivers PRD-099's sixth acceptance criterion already** — what
  verification means is now changeable only by a reviewed change to this
  repository. Whoever implements 099 should not write it twice.
- **Verification image rebuilt** — and with `familiar-ai-tray` now a
  workspace member (`e31dc53`), the image needed GTK and the Ayatana
  indicator. Confirmed: the tray compiles in Docker and appears in the
  coverage table.

**The suite was already green.** 1427 passed / 0 failed / 2 ignored across
90 targets locally; 1411 / 0 across 74 in the image, the gap being
`--no-default-features` dropping the daemon's tray tests. Roughly 1,400
tests and ~97 archived PRDs, and the only thing wrong was that nothing had
ever run them in one command. Coverage baseline: **78.76% lines, 71.89%
functions**, with `familiar-ai-tray/src/windows.rs` at **0.00%** — 2,252
lines of GTK verified by nobody but a human looking at it.

Every command in this phase was run by hand. That is the point of PRD-099
and the reason it is the next thing.

## Rounds

A round is the owner's definition applied literally: dependency-ready and
mutually scope-disjoint under `scope_overlap` on the amended
`expected_files`.

**PRD-099 is landed first, alone, by a human — it is not a round.** An
unverified system cannot be asked to build its own verifier and then be
believed about the result. It is also the precondition for the honesty of
everything after it: nine queued PRDs write their criteria in the language
of automatic verification (*"proven by a gate job"*, *"a check scans and
fails"*) and `.github` has never existed on any branch, so every round
below is unfalsifiable until it lands. Land it advisory, watch a few
merges, then turn on the merge refusal.

**This costs one sequential step and the cost is stated rather than
hidden:** six rounds is the floor for the remaining fourteen PRDs under
every ordering tested, so the queue is 099 plus six rounds — seven phases,
where the 2026-09-17 cut claimed six by putting 099 inside round 1.

| Round | Width | PRDs | What it buys |
|---|---|---|---|
| 1 | 4 | **100**, 085, 090, 096 | Familiar's own loop is a selectable worker; the stall taxonomy starts recording; CLI surface pass; a failing required check becomes work, not a wall |
| 2 | 3 | **101**, 082, 093 | no vendor CLI required; tool-output retention hardened; PRD admission quality |
| 3 | 3 | 086, 092, 097 | cost measurement; the gate builds what users install; forge-agnostic delivery |
| 4 | 2 | 088, 098 | load-faithful verification; delivery claims computed from the ledger |
| 5 | 1 | 089 | no holes in required gates |
| 6 | 1 | 094 | local workers reachable from dispatch |

The width-1 tail is structural, not sloppy scheduling: 089 depends on 088
and conflicts with 094, so nothing can share either round.

**Amended 2026-09-19: 093 moved from round 1 to round 2.** A pre-flight of
round 1's manifests found that PRD-085 and PRD-093 each declare a migration
but neither declared `crates/familiar-ai-storage/src/migrate.rs` — and a
migration that is not registered in that file's `MIGRATIONS` array never
runs. Both would have hit `scope_broadened` on their first real edit, which
is the single largest cause of retained attempts in the ledger and is
exactly what stopped PRD-099's implementation until the manifest was
widened. Declaring `migrate.rs` fixes that, and makes 085 and 093 conflict
with each other, so they can no longer share a round.

That is a real bottleneck worth naming: **every migration-bearing PRD must
touch `migrate.rs`, so no two of them can ever run in the same round.** 085,
086 and 093 are now mutually serialized for that reason alone. It is the
same hub-file problem as `README.md` and `config/default.toml`, and a
registration mechanism that did not require editing a shared array — a
build script, or `include_dir!` over the migrations directory — would return
that width.

**Why this ordering and not the widest-first one.** Greedy
maximum-independent-set scheduling also finishes in six rounds, but it puts
PRD-100 in round 5 and PRD-101 alone in round 6 — because 101 conflicts
with nine of the other fourteen and can only run after 100. That schedule
costs exactly the same and delivers the **vendor-CLI independence pair four
rounds later**. Since the round count is identical, ordering is free, and
it should be spent on the elephant: Familiar must not require `codex` or
`claude` to run. Rounds 1 and 2 deliver that.

## Critical path

**099 → {100 → 101} and {085 → 086 → 098}.**

099 gates both. After it, two chains run concurrently. The left chain is
goal 2, vendor independence, and is two rounds long. The right chain is
the measurement spine — 085 records why unattended runs stall, 086 makes `CostUnmeasured`
the exception, and 098 turns both into a shipped query so the next status
claim is decided by the table rather than by narrative. 098 cannot start
before both of its inputs land, and 085 and 086 conflict on
`crates/familiar-ai-daemon/src/report.rs`, so the spine is four rounds long
no matter how it is scheduled. It is the longest path in the queue and it
sets the six-round floor for the rounds.

The ledger's own ranking of what stops delivery, this host — **corrected
2026-09-22; the table that stood here counted 12 rows from another
repository's fixtures as this project's stops:**

| Retained reason, this repository only | Count |
|---|---|
| `verification_failed` (4 of 6 infrastructure, FAM-BUG-059) | 6 |
| `scope_broadened` | 5 |
| `scope_ambiguous` | 4 |
| `unclassified_result` | 3 |
| `interrupted` | 3 |
| `human_review_required` | 3 |
| `malformed_output` | 2 |
| `integration_failed` | 1 |
| `checkpoint_failed` | 1 |

Two observations, one of which survives the correction:

1. **Scope authorization refuses nine attempts** — the single largest
   cause, and PRD-080 already narrowed it once. The remaining
   `scope_broadened` cases are post-080 (PRD-63, 72, 87, 92 on 09-03 and
   09-05), so 080 did not close the class. The hub-file table above is the
   same finding seen from the other side: PRDs are declared against files
   that other PRDs also need. All nine were later landed by hand.
2. ~~Six attempts record no reason at all and six die on a review
   precondition before a model is ever called.~~ Both sets are spectra
   fixture rows. PRD-085 and PRD-093 stand on their own merits, not on
   this histogram; the round-1 placement argued from it is withdrawn.

Per the bug policy, FAM-BUG-019 and FAM-BUG-022 are reopened and outrank
the product queue. Their exit criterion is unchanged: one multi-PRD wave
delivered using only Familiar commands. **Round 1 is the natural test** —
a four-PRD wave, run once 099 makes its result verifiable, and if it has to
be landed by hand that is the recurrence, not a footnote. Phase 0 and 099
are both deliberately hand-run and neither counts as evidence either way.

## Approval status

**BATCH APPROVED — by the owner, 2026-09-19.**

The 082+ queue is approved for unattended execution over the decomposition
in this document: PRDs **082, 085, 086, 088, 089, 090, 092, 093, 094, 096,
097, 098, 100 and 101**, in the six rounds above, under the human-approval
policy at the end of this file. This is the PRD-019 batch gate opening over
a decomposition for the first time since the 2026-08-31 authorization was
exhausted.

Workers implementing within a PRD's declared `expected_files`,
`acceptance_criteria` and `risk_classes` proceed without pausing for
per-PRD sign-off. That is the point of the grant: until now these PRDs ran
one at a time under ordinary approval, which made a multi-PRD wave — and
therefore the FAM-BUG-019 exit criterion — impossible to even attempt.

**PRD-099 is not in the grant.** It lands by hand, alone, ahead of the
rounds: an unverified system cannot be asked to build its own verifier and
then be believed about the result. *(Landed 2026-09-18, PR #16.)*

**Amendments from the 2026-09-22 audit.** 088 and 089 are `draft` and
leave the grant until the owner returns them to `ready`. 105 is `blocked`
on 100. 106 and 107 are `draft` and await the owner's approval; nothing in
this document authorizes them yet. The grant is otherwise unchanged.

What this grant does **not** authorize, unchanged from the standing policy
below:

- Work outside a listed PRD's declared scope. A scope deviation still
  pauses with a decidable finding (PRD-080) or blocks with a recorded
  reason; it does not proceed on the strength of this approval.
- A migration number not allocated in that PRD's `expected_files`.
- Any new PRD. Work not traceable to a listed PRD needs its own approval.
- Merging to `main` by hand. If that becomes necessary it is a FAM-BUG-019
  recurrence and goes in the bug log.

### Preconditions cleared before the grant

- **Phase 0** — the gate's own checks are green and the toolchain is
  pinned, so a round's failure is the round's and not the environment's.
- **Migration allocation deconflicted, 2026-09-19.** PRD-093 declared
  `063_admission_quality.sql` while `063_local_worker_telemetry.sql`
  already existed — a flat collision inside round 1. PRD-085 and PRD-086
  declared 059 and 060, both gaps in the applied sequence, which would have
  ordered differently on a fresh database than on an upgraded one. These
  are now 068, 066 and 067. `073_model_residency.sql` was renamed to 064 to
  match the version it actually declares, and
  `crates/familiar-ai-daemon/tests/migration_allocation.rs` fails the build
  if a filename and its version disagree, if versions collide or go
  backwards, or if a queued PRD declares a number already applied.
- **The tray is pinned to keep shipping.**
  `crates/familiar-ai-daemon/tests/tray_ships.rs` fails if it returns to the
  workspace `exclude` list, leaves the daemon's default feature set, or
  grows its own lockfile again.

### Open, and not blockers

- Branch protection is off. The gate is advisory until it is on, which is
  deliberate ordering rather than an oversight.
- Nothing calls `familiar-ai gate require` yet, so the delivery path can
  still integrate a commit whose gate is red or absent.

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
