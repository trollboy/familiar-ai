# Spectra Autonomous Development Handover

Date: 2026-08-24

## Objective

Make Familiar a reliable unattended engineering steward for
`/Users/trollboy/Projects/spectra`: launch one bounded command, let it process
dependency-ready PRDs with the least capable model that can do the job, run
verification and adversarial review, publish/merge only clean work, deploy
verified batches to DigitalOcean staging, and produce one morning report.

The target is not blind autonomy. Familiar must save human judgment for
explicit gates and stop with an exact, durable reason when a gate, test,
security check, or external dependency fails.

## North-star policy

- Minimize tokens and human touches per accepted PRD.
- Prefer deterministic tools (git, parsers, tests, linters, Docker, hashes)
  over model calls.
- Route discovery, summaries, formatting, and narrow fixes to local Qwen/other
  local models; use Claude/Codex only for broad implementation; use a different
  strong model for adversarial review.
- Never retry an expensive failed approach without new evidence.
- Never claim completion without persisted implementation, verification, and
  review evidence.
- Never merge or deploy a conflicted, untested, security-blocked, or
  incompletely recorded result.

## Current Spectra state

- Repository: `/Users/trollboy/Projects/spectra`.
- There are 73 Markdown PRDs in `docs/prd/todo/` (raw count; dependency-ready
  count is smaller).
- PRD 0136 was decomposed into:
  - `0136a-legacy-gateway-foundation.md`
  - `0136b-legacy-gateway-data-plane.md`
  - `0136c-legacy-gateway-rollout-acceptance.md`
  - parent epic: `docs/prd/0136-legacy-application-access-gateway-epic.md`
- Familiar attempted 0136a/0136b/0136c and retained them without useful
  recorded detail, then started 0139f and was interrupted.
- No PRD implementation from this campaign has been published or merged.
- Dependabot work is separate and has been merged; do not confuse it with PRD
  throughput.
- Existing uncommitted Spectra gateway/attestation scaffolding from an earlier
  interrupted attempt must be reviewed before inclusion; preserve user work.

## Familiar fix already implemented

Commit `d96b81c` on Familiar `main`:

- `DriverRepository::recover_incomplete()` marks abandoned attempts as
  `retained/interrupted` and closes abandoned sessions.
- The drive loop invokes recovery before opening a new session.
- Retained attempts with no detailed classifier receive `run_failed` instead
  of becoming `unrecorded`.
- Added an idempotent storage regression test.

Focused storage and drive-loop tests passed, and the release binary was rebuilt
and installed at `/Users/trollboy/.local/bin/familiar-ai`.

This fixes durable reporting on the next drive restart. It does not yet fix the
underlying interrupted agent, missing toolchain preflight, or parallel
orchestration.

## Known failure modes to fix next

### 1. Preflight before claiming a PRD

Before an attempt is claimed, validate the configured implementation/reviewer
executables and repository toolchain: `go`, `gofmt`, Docker, Node/npm, Claude,
Codex, Ollama, GitHub auth, and staging deployment credentials. Record a
structured failure and leave the PRD pending when a prerequisite is absent.
Do not spend model tokens discovering an unavailable tool.

### 2. Durable result protocol

The coding-agent adapter must always return a structured terminal result:
completed, retained with reason, timed out, interrupted, malformed output,
launch failure, or budget exceeded. Persist the result before the driver can
select another PRD. Add tests for process kill, adapter crash, EOF, malformed
JSON, and partial output.

### 3. Do not silently advance after an unclassified result

The drive loop may continue after a normal retained result, but it must stop on
an unclassified result, storage error, or worker heartbeat loss. The report must
show the exact PRD, attempt, adapter, model, exit/signal, and last durable
phase.

### 4. Dependency-aware parallel worktrees

Build the PRD graph, select only ready nodes, and run independent nodes in
isolated worktrees with bounded concurrency. Serialize migration/shared-file
conflicts. Each worktree needs an ownership record, heartbeat, result journal,
and cleanup/recovery policy. Parallel execution is not yet implemented in the
current Familiar stage.

### 5. Token economy and model routing

Add deterministic preflight/context caching and per-stage budgets. Route by
task complexity and stop on diminishing-return retries. Account for known and
unknown cost honestly; unknown cost must stop a cost-bounded session.

### 6. Publish/merge/staging delivery boundary

Familiar currently does not own push, merge, or deployment. Add an explicit
policy-controlled delivery adapter for this project:

- create/update PR with evidence;
- merge only when clean, reviewed, and all required checks pass;
- deploy staging only (DigitalOcean; no production exists);
- run health/smoke checks and roll back failed batches;
- comment actionable blockers instead of prompting repeatedly.

Keep this behind a finite warrant and an auditable policy. Never bypass a
security or test gate merely to increase throughput.

### 7. Persistent worker

Run the driver under a macOS supervisor (`launchd` preferred) with restart
recovery, logs, heartbeat, and one bounded warrant. A shell `nohup` process is
not sufficient evidence of unattended operation. The worker should survive
the chat session ending and emit a morning report.

## Suggested implementation order

1. Add toolchain/agent/staging preflight and tests.
2. Complete durable adapter terminal-result protocol and stop semantics.
3. Add driver heartbeat/single-worker lock and crash recovery tests.
4. Add dependency graph scheduling and isolated worktrees, initially with
   concurrency 2.
5. Add token-aware model router and cached context metrics.
6. Add policy-controlled PR publication/merge and staging deployment adapters.
7. Install and test a launchd worker on a harmless fixture repository.
8. Run Spectra in small batches; do not start all 73 PRDs as one unbounded
   session.

## Verification expectations

For each Familiar change, use the repository's Make/test conventions where
available and add tests for happy paths, errors, crashes, and edge conditions.
At minimum run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo build --release --no-default-features -p familiar-ai-daemon --bin familiar-ai
```

Before touching Spectra, verify `familiar-ai report`, `history`, and `usage`
show durable records after simulated interruption and restart.

## Do not do

- Do not release/complete the retained Spectra PRDs merely to clear the
  backlog.
- Do not copy API tokens, passwords, or credentials into this document.
- Do not claim all 73 PRDs can safely finish in one session without graph
  scheduling, delivery policy, and recovery evidence.
- Do not use Fable as a substitute for deterministic tests. Use it later as an
  independent adversarial reviewer of the Familiar reliability changes.

## Immediate next session command

Start in `/Users/trollboy/Projects/familiar`, inspect commit `d96b81c`, and
continue with preflight plus durable-result tests before launching another
Spectra drive. Use `/Users/trollboy/Projects/spectra` only after those tests
pass and the worker reports a valid toolchain.
