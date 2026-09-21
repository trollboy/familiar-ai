# Situation report — 2026-09-21

Written for an agent picking this repository up without the preceding
session's context. It states what is true, what is assumed, and where the
traps are. Everything here is checkable; where it is not, it says so.

---

## Where things stand

`main` is at `e6d9308`. The full workspace suite passes, clippy is clean
under `-D warnings`, and the toolchain is pinned at 1.93.1 by
`rust-toolchain.toml`.

**Verification is local and runs without being asked.** `scripts/gate.sh` is
the single definition of what verification means. A `pre-push` hook runs it
and records the verdict in Familiar's own ledger; `familiar-ai ops gate
status` answers `green`, `red`, `absent` or `unreadable` for any commit,
offline. There is no CI. An earlier cut of PRD-099 ran this in GitHub
Actions; that was removed deliberately — see *Assumptions* below.

**Round 1 of the execution plan is 3 of 4.** PRD-085 and PRD-096 were
implemented, reviewed and integrated by Familiar itself through its merge
queue, which is the first multi-PRD integration recorded on this host.
PRD-090 completed review but had to be landed by hand (FAM-BUG-065).
PRD-100 is still blocked. PRD-092 is blocked with a missing worktree, though
its branch survives locally and on origin.

---

## Outlying problems

**FAM-BUG-019 and FAM-BUG-022 — the oldest open entries.** Neither is a code
defect. They close on evidence: one multi-PRD wave delivered hands-off. The
execution ledger is the arbiter, not a narrative. Do not close them on a
partial result; that has already happened once and was retracted.

**FAM-BUG-061 — no independent review has ever been costed.** Only two sites
write usage observations, and the ledger holds zero `review` rows because
the batch-review path has never run. Every reviewer call in the project's
history is unaccounted, so the lifetime cost figure is an undercount. This
is PRD-086's entire subject; fixing it elsewhere is scope creep.

**FAM-BUG-058 — an unreproduced `security_burn_in` failure.** Its entry says
explicitly: do not fix it before capturing a failure. Three loaded Docker
runs on 2026-09-21 did not reproduce it. `scripts/gate.sh` now records the
names of failing tests, so the next occurrence will identify itself rather
than vanish.

**FAM-BUG-065 — a false completion strands work permanently.** Completion is
immutable (see the contract), so a PRD marked complete whose candidate never
landed is refused by `resume` — "historical evidence, not resumable work" —
and there is no supported path to land it. FAM-BUG-064 stops new ones. This
one needed hands. A supported way to land evidence that is already marked
complete, distinct from resuming it, is a real design question and wants a
PRD.

**PRD-100 and PRD-092 remain blocked.** PRD-100 has 15 files of implemented
work retained in its worktree and every blocker removed as of 2026-09-21;
it has not been re-driven since. PRD-092's checkpoint points at a worktree
that no longer exists; `ops operator rebind` is the tool for that.

**macOS is unverified.** `cargo check --target aarch64-apple-darwin` dies in
`libsqlite3-sys`'s build script for want of an Apple SDK, so this machine
cannot answer the question. The code is structured for it — everything GTK
is behind `cfg(target_os = "linux")` — but `muda`'s `features = ["gtk"]` in
`crates/familiar-ai-tray/Cargo.toml` is declared **unconditionally** while
`gtk` itself is target-gated. That is the first thing to suspect if a macOS
build fails.

**Coverage is ad hoc.** Replacing the compose service's `cargo llvm-cov`
command with the single gate definition means coverage is no longer
produced automatically. The last measured baseline was 78.76% lines, with
`crates/familiar-ai-tray/src/windows.rs` at 0.00%.

---

## Assumptions, and why they are what they are

**Verification is local, not CI.** This is a desktop daemon that deploys
nothing; the most it ever builds is a binary release, made on the developer's
machine. A hosted runner cost eight minutes of cold-cache compilation per
run, fired on every work-in-progress commit, and coupled verification to a
forge whose independence PRD-101 exists to establish. The objective — runs
unasked, recorded, queryable — never required any of it. Do not reintroduce
a workflow without deciding that deliberately.

**Docker is not required to install or run Familiar.** It is absent from
`config/default.toml` and from every install and runtime path. It appears
only in the operator's own `[[review.verification]]` argv, where it
sandboxes model-written code that no human has reviewed yet. The agent's own
tool calls are sandboxed separately by landlock, which does **not** cover
verification. That is the only reason Docker is load-bearing, and it is
easy to mistake for ceremony and remove.

**Every verification check must pass `-p familiar-ai-verify`.** Without a
fixed compose project name, Compose derives one from the worktree directory,
so each PRD gets its own network *and its own cargo-cache volume*. Networks
accumulated until the address pool was exhausted and an entire four-PRD wave
failed with every check reporting the same 189 bytes of Docker error
(FAM-BUG-059). The shared cache also took verification from 1,045–4,292s to
181s.

**Completion is immutable.** `docs/contracts/completion-is-immutable.md`.
There is deliberately no command that moves a PRD out of `completed`. When a
PRD says do A, B and C and only A and B were done, the answer is a successor
PRD citing the findings — not a reversal. PRD-103 is the worked example, for
PRD-090's unmet AC3 and AC5.

**A command the control-plane owner dispatched runs as its delegate.** The
daemon holds the orchestrator lock, and anything it spawns needs the same
lock, so the tray's Start and Re-drive failed on every click for as long as
they existed (FAM-BUG-062). `control_worker` now passes
`FAMILIAR_AI_DELEGATED_BY`, and `WorkerLock` yields to a process naming the
live owner exactly. Exclusion is preserved: a stranger is still refused, a
wrong pid authorises nothing, and two delegates still exclude each other
through their own `O_EXCL` lock. Do not widen this into a general bypass —
two owners of an exclusive lock is what FAM-BUG-027 was, and it cost a wave.

**Migration filenames and versions must agree.** `073_model_residency.sql`
declared version 64, so `ls` lied about the next free number and a naive
reading left a nine-version hole. `migration_allocation.rs` now fails the
build if a filename and its version disagree, if versions collide or go
backwards, if a queued PRD claims a number already applied, or if two queued
PRDs claim the same one. The scheduler deliberately exempts the migrations
directory from overlap detection (PRD-066 allocates those numbers instead),
so nothing else catches it.

**A PRD that declares a migration must also declare
`crates/familiar-ai-storage/src/migrate.rs`.** A migration file does nothing
until it is registered there. Three queued PRDs declared a migration and not
that file, which would have been `scope_broadened` on their first real edit.

**Tests must not mutate process-global environment.** `cargo test` runs
tests as threads in one process. Three tests in `paths.rs` removed and
restored `XDG_*` variables, which flaked the gate twice and never
reproduced — the evidence put itself back. The rules are now pure functions
taking the environment as arguments.

**The bug log is machine-countable.** Every bug gets
`### FAM-BUG-NNN — title` and a `- **Status:**` line whose first word is the
**current** state. `bug_log_contract.rs` fails the build otherwise. Before
this, "how many are open" had a different answer depending on how it was
asked, and an ad-hoc count of mine was wrong twice.

---

## Things that are true and easy to disbelieve

- **Three of the bugs closed on 2026-09-21 had already been fixed** and
  nobody had closed the entries: FAM-BUG-054 by PRD-096, FAM-BUG-051's root
  cause by the containment rewrite in `scope_entries_overlap`, FAM-BUG-006
  by PRD-075. The log was overstating the real problem by about a third.
  Check before rebuilding something.
- **The tray's Release and Force-complete buttons never worked**, from the
  day they shipped until 2026-09-20. They passed empty actor and reason, and
  `validate_recovery_attribution` rejects both. Nothing reported it because
  dispatched commands ran with stdout and stderr set to null (FAM-BUG-063).
- **`familiar-ai` and `familiar-ai-daemon` in `~/.local/bin` can be stale.**
  They are installed by hand from `target/release`, so the installed binary
  and the tree routinely disagree. While fact-checking this document, the
  installed `familiar-ai` did not have `ops gate status` — because it
  predates PRD-090's namespacing, which landed minutes earlier. The daemon
  prints its commit in its first log line; check it before concluding a
  command is missing. Rebuild and reinstall both binaries after anything
  that changes the CLI surface.

---

## What to do next, in order

1. **Re-drive PRD-100.** 15 files of implemented work are retained, and
   every blocker is gone: Re-drive can fire (062), the reviewer gets the
   authorised scope (060), completion follows landing (064), and a failure
   will say why (063). If it lands, round 1 is 4 of 4 and FAM-BUG-019 and
   022 close on real evidence for the first time since August.
2. **Rebind PRD-092** with `ops operator rebind`; its branch survives.
3. **PRD-103**, which finishes PRD-090's unmet criteria.
4. Rounds 2–6 of `EXECUTION-PLAN.md`, which carries a batch approval granted
   2026-09-19.

The execution plan's rounds were computed by calling the scheduler's own
`achievable_width()` against the live backlog rather than by authoring a
width. Recompute rather than edit by hand if PRD manifests change.
