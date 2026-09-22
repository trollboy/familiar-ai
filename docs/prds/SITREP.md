# Situation report — 2026-09-22

Written for an agent or reviewer picking this repository up without the
preceding session's context. It states what is true, what is assumed, and
where the traps are. Everything here is checkable; where it is not, it says
so.

---

## Read these first, in this order

1. `docs/north-star.md` — the stated mission.
2. This file.
3. `docs/prds/EXECUTION-PLAN.md` — how rounds are meant to be grouped.
4. `docs/prds/*.md` — the 24 queued PRDs. This directory **is** the todo
   queue. Archiving to `done/` is the owner's call, never done in an
   implementing commit, so the queue contains finished work (see *Traps*).
5. `docs/contracts/*.md` — 20 contract documents. These are the invariants
   the code is supposed to hold.
6. `docs/running_bugs.md` — the bug ledger.

Sample `docs/prds/done/` (97 files, ~125k words) rather than reading it.
Comparing a low-numbered PRD against a recent one shows how the character
of the work changed.

---

## The number that matters

| | |
|---|---|
| PRDs written | ~121 (97 done, 24 queued) |
| Driver attempts recorded (this repo) | 43 |
| Attempts that completed | **2** |
| Commits from Familiar's own merge queue, all-time | **5** |
| Rounds completed unattended | **0** |

Against that, the *refusal* surface, counted from the enums:

| enum | variants |
|---|---|
| `ReviewStopReason` | 18 |
| stall taxonomy classes | 31 |
| `DriveTermination` | 15 |
| `PackageError` | 6 |
| `ReviewValidationError` | 10 |

Roughly 80 named ways to stop, with some overlap. Five lifetime successes.

**No round has ever completed unattended.** Every apparent success involved
a human hand-carrying the candidate past a stall. Treat any claim to the
contrary — including in older documents — as unverified.

---

## Where things stand

`main` is at `846304f`. The full workspace suite passes, clippy is clean
under `-D warnings`, toolchain pinned at 1.93.1.

**Verification is local.** `scripts/gate.sh` is the single definition of
what verification means. A `pre-push` hook runs it and records the verdict
in Familiar's own ledger; `familiar-ai ops gate status` answers `green`,
`red`, `absent` or `unreadable` for any commit, offline. There is no CI,
deliberately.

**Two machines are working this repository.** A macOS session is building
`crates/familiar-ai-desktop`, a cross-platform Tauri shell (PRD-104), and
has been editing `crates/familiar-ai-tray/`. A Linux session has been
working on the daemon, review and storage crates. As of this writing the
file sets do not overlap, but `main` has twice been broken by changes that
only fail on the other platform.

**PRD-100 is open as PR #19** rather than integrated. Its review cycle never
completed — the package is 19 files / ~960KB against a 250KB host cap — so
it passed its gates but was never read by a reviewer. That distinction is
the point of the PR.

---

## What changed on 2026-09-21/22

Six fixes landed, all of them to machinery that was discarding information
or refusing work incorrectly. None of them changed what the system can
build; all of them changed what it can explain.

- `driver_attempts.retained_detail` (migration 070) — a stopped attempt
  records the stopping error, not only a taxonomy token.
- The review coordinator no longer discards the verifier's error with
  `Err(_)`; `stop_detail` rides beside the stop reason through
  `ReviewCycle` → `AttemptTrace` → the ledger.
- A finding with `status: resolved` no longer blocks a landing.
  `is_blocking` consulted category and severity and never status, so a
  finding the reviewer had just marked fixed stopped PRD-100.
- Every *open* finding now carries into the next review's `prior_findings`,
  not only the blocking ones. Three reviews of PRD-100 produced 4, then 2,
  then 5 findings with **no overlap at all**, because non-blocking findings
  were silently dropped between attempts.
- `blocking_category_floor` (default `Medium`) bounds the category rule. A
  `Low`/`SecurityIssue` note no longer stops a landing on category alone.
  Severity still blocks at `High`/`Critical` regardless of category.
- Two of this repository's own contract tests were narrower than the
  contracts they guard and were failing against legitimate work:
  `bug_log_contract` rejected `In progress`, `gate_contract` rejected an
  install command for the second product.

---

## Outlying problems

**1. The remediation stage does not format its own output.** It changes a
signature, call sites reflow, and the `format` gate fails. This killed a
full cycle on 2026-09-21 and will recur on any signature change.

**2. A hand repair invalidates the checkpoint.** `diff_hash` is bound to the
candidate, so any manual fix requires `ops operator rebind` with an explicit
human actor. Correct as integrity, but it means no repair is ever free.

**3. A flaky check escalates to a human instead of being retried.**
`tests-green-crates` failed once and passed on rerun, costing a full cycle.
Worse, the operator was told *"This check passed on an earlier attempt — it
may be flaky"* for `cargo fmt`, which cannot flake — `FailureShape::Varying`
treats variation as evidence of flakiness for every check, including
deterministic ones.

**4. The review package has no back-pressure.** Remediation grows the
candidate every round — PRD-100 went 15 → 21 → 36 → 42 changed paths — until
it cannot be assembled. Nothing warns before it is too large.

**5. PRD size is unbounded at admission.** PRD-100 has 9 acceptance
criteria and failed four times. PRD-104 has **14** and is in progress. The
PRDs that landed cleanly had 3–6.

**6. The stale blocking display.** When a cycle dies before a second review,
the operator is shown the *previous* review's findings as though they were
current — including ones the remediation already fixed.

**7. Review non-determinism.** See the 4/2/5-with-no-overlap figure above.
At least two findings across those reviews were factually wrong about the
code (one invented a schema field name that `args_schema` contradicts).

---

## Traps

- **`gh` account.** Two accounts are configured; the active one reverts to
  `mbowerhouse`, which has read access only and fails PR creation with
  `must be a collaborator`. Run `gh auth switch -u trollboy` immediately
  before any `gh` write, not once per session.
- **The queue overstates itself.** PRD-085, 090, 095, 096, 097 and 099 are
  landed but still in `docs/prds/`. The ledger disagrees with both the
  filesystem and GitHub, in different directions.
- **`crates/familiar-ai-desktop/gen/`** is generated by `tauri-build` and is
  not gitignored, so *running the gate* dirties the worktree and blocks a
  subsequent rebase with `could not detach HEAD`.
- **Two package limits, not one.** `max_package_bytes` and
  `max_package_tokens` are checked in the same expression and
  `estimate_tokens` is `bytes / 4`. Raising bytes alone does nothing.
- **`scripts/gate.sh` runs `cargo test --workspace --no-default-features`**,
  which disables the `tray` feature. The tray compiles under clippy but its
  tests never execute in the gate — which is what PRD-092 exists to prevent.
- **Building on Linux now requires `libwebkit2gtk-4.1-dev`** because
  `familiar-ai-desktop` is a workspace member. The failure is a pkg-config
  error inside a build script, not a named diagnostic.
- **`git checkout --theirs` during a rebase means the commit being
  replayed**, not the upstream side. This silently clobbered another
  session's PRD in this repository once already.

---

## Assumptions, and why they are what they are

- **No CI, deliberately.** This is app development, not web deployment.
  Verification runs locally, on the machine doing the work. An earlier cut
  of PRD-099 ran the gate in GitHub Actions; it was removed.
- **Docker is optional.** Hosting the daemon in a container is a supported
  choice, not a requirement for a normal install.
- **Completion is immutable.** A completed PRD is never edited after the
  fact; unmet criteria become a new PRD. See
  `docs/contracts/completion-is-immutable.md`, and PRD-103/PRD-105 as
  worked examples.
- **`docs/prds` is the queue.** Archiving is the owner's decision.

---

## The open question

The corpus has added a mechanism per PRD for roughly 100 PRDs — scope
authorization, budget reservations, checkpoint binding, worker locks,
delivery policy, blocking policy, verification gates, escalation surfaces.
Each is individually defensible. Each acquired a veto. No PRD was ever
written that says *the loop completes unattended, end to end*, so the
composition has never been tested.

Three courses have been proposed and none chosen:

1. **Freeze new mechanism PRDs** until one round completes unattended.
2. **Make each refusal justify itself against the ledger** — most of the ~80
   classes have never fired; a refusal that has never correctly stopped a bad
   candidate is dead weight with a veto.
3. **Run the smallest possible round hands-off and let it fail**, to get an
   honest count of which refusals actually fire. Candidates: PRD-098 (3
   criteria), PRD-103 (4), PRD-105 (3).

The reflex this repository needs to resist is treating each stop as a bug to
fix individually. Eighty of them is not a bug; it is the design.
