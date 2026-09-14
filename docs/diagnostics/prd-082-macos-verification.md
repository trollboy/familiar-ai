# PRD-082 on macOS — handoff

Written on Linux, 2026-09-13, for whoever picks this up on the Mac (Claude
session or Familiar). Everything below was verified on Linux only.

## Where the work is

- Branch `prd-082-tool-output-retention-hardening`, commit `4fe6bb0`.
- PR **#8**, based on PR **#7** (`prd-081-resolved-path-worktree-containment`),
  *not* on `main`. PRD-081 is not merged; 082's defect 4 is about code that
  only exists on that branch. Merging #8 means merging #7 too.
- PRD file stays at `docs/prds/PRD-082.md`. `docs/prds` is the todo queue; a
  PRD moves to `done/` only when the owner declares the work complete, as its
  own `chore: archive …` commit. Do not archive it as part of implementing it.

## What shipped, and what was already fixed before it

Three of PRD-082's four defects were **already closed in the tree** when the
work started — the wip commit `790da40` had moved retention out of the worktree
into daemon-owned storage created `0700` and written `0600`, made the retention
write's error inspected, and made a failed write fall back rather than advertise
a handle. The PRD's narrative describes the pre-move code. Do not "re-fix" those.

What this branch actually contains:

1. **Defect 4 — one canonical root.** `SandboxedToolExecutor::new` canonicalizes
   the worktree root once; the field is private so no second form can exist.
   Every containment comparison, every prefix-relative computation,
   `run-command`'s default cwd, and the retention key now read that one value.
   All ten construction sites moved from struct literals to the constructor.
2. **Store-root hardening (beyond the PRD's list).** The directory the
   per-execution retention dirs are created inside was made at the ambient umask
   and never inspected — a local user could pre-create it and own the parent of
   every retention directory. It now gets the same create / verify-owner /
   `chmod 0700` treatment and is keyed by effective uid.
3. **Retention lifecycle**, stated in `docs/contracts/agent-loop.md`, plus
   `SandboxedToolExecutor::discard_retained_tool_output`.
4. **The two acceptance tests** run for real — `#[ignore]` removed, every
   assertion byte-identical. Only their *setups* changed: both had pointed at
   `.familiar/tool-output/` inside the worktree, which post-`790da40` is only the
   shape of the opaque handle string, not a real path. They now point at the
   actual store via `tool_output_retention_dir()`.

## Why macOS specifically

PRD-082's defect 4 is the case that **does not reproduce on Linux and does on
macOS**. `std::env::temp_dir()` on macOS is `$TMPDIR` under `/var/folders/...`,
and `/var` is a symlink to `/private/var`, so a bare `tempfile::tempdir()` root
canonicalizes to a path with a different prefix than the one the test holds. On
Linux a tempfile root canonicalizes to itself, which is why the bug was latent
here.

Consequence: on macOS **every test in `runtime_path_containment.rs` exercises
the fix implicitly**, because `new()` stores `/private/var/...` while the test
still holds `temp.path()`. The dedicated regression
(`a_symlinked_root_spelling_is_indistinguishable_from_its_canonical_one`) plants
a symlink explicitly so it covers the case on both platforms, but it is not the
only coverage there.

## What to run

```
cargo test -p familiar-ai-daemon --test runtime_path_containment
cargo test -p familiar-ai-daemon --test runtime_token_discipline
cargo test --workspace
cargo clippy --workspace --all-targets
```

Linux baseline on `4fe6bb0`:

- `runtime_path_containment`: **14 passed, 0 failed, 0 ignored**
- `runtime_token_discipline`: 9 passed
- `cargo test --workspace`: **1322 tests, 86 binaries, 0 failed, 0 ignored**
- clippy: no new warnings (two pre-existing ones remain, in
  `crates/familiar-ai-core/src/bootstrap.rs` and
  `crates/familiar-ai-daemon/src/summary_worker.rs`)

Two gotchas that are not failures:

- `cargo fmt --all` cannot run in this workspace — the tray crate is
  workspace-excluded and `cargo metadata` errors on it. Use
  `rustfmt --edition 2021 --check <files>` on changed files instead.
- `cargo test --workspace` prints `error: agent did not produce a valid terminal
  result: EOF before turn.completed`. That is fixture output from
  `cli_run.rs::run_records_malformed_output_without_completing`, which
  deliberately feeds a fake Codex that dies mid-stream. That binary reports
  `2 passed; 0 failed`.

## If something fails on macOS

The failure class this change could newly introduce is **a comparison between an
executor-produced path and a raw `temp.path()`**, which now differ because the
executor canonicalizes. I found no such comparison — the containment tests assert
on worktree-*relative* output and the retention tests locate files through
`tool_output_retention_dir()` — but that is the first thing to look at.

Triage rule: decide whether the failure is the **invariant** breaking or the
**test setup** being stale, and say which in the report. That distinction is the
whole story of this PRD — its two acceptance tests failed for setup reasons, not
because the invariants were broken.

Do **not**:

- weaken or delete any assertion in `runtime_path_containment.rs` (acceptance
  criterion 6 is that it passes in full with nothing relaxed);
- re-add `#[ignore]` to the two retention tests;
- move retention back inside the worktree — it is outside deliberately, because
  untracked bytes in the worktree would need write-scope authorization no
  PRD-013 Expected Files entry could cover and would pollute the diff evidence
  the change under review is judged against.

If a macOS-only fix is needed, put it on this branch (PR #8 is still open) rather
than a new one, so the fix and the change it corrects land together.

## Two things that cost time here

- **`gh`'s active account.** On the Linux box the active account is
  `mbowerhouse`, and `gh pr create` against `trollboy/familiar-ai` fails with an
  opaque `GraphQL: Something went wrong` 500 (and the REST fallback returns
  `unexpected end of JSON input`) — never a 403. Fix is
  `gh auth switch -u trollboy`, create, then switch back. Check
  `gh auth status` on the Mac before assuming a push/PR problem is real.
- **Stale PRD narratives.** The wip commit `790da40` imported a large
  unreviewed working tree, so PRDs written before it may describe code that no
  longer exists — PRD-082 was three-of-four already fixed. For the rest of the
  queue (073, 085–094), read the code a PRD quotes before planning, and treat a
  failing acceptance test as a question rather than as the defect itself.

## There is no CI

The repo has no `.github/workflows/`; PR #8 reports zero checks. `MERGEABLE` /
`CLEAN` on the PR is about merge conflicts only, not tests. Local runs are the
only evidence that exists. Adding a workflow that runs `cargo test --workspace`
plus clippy was offered and not yet done — it should go up as its own PR against
`main`, not folded into this stack.
