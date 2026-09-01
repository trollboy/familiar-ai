# Wave 4 After-Action Report

**Date:** 2026-08-31  
**Wave:** PRD-032, PRD-055, PRD-056, PRD-062  
**Result:** Complete only after manual recovery. All four implementations were
verified, integrated into `main`, and marked complete, but Familiar delivered
none of them autonomously.

The dominant finding is unambiguous. The actual Wave 4 workflow was:

1. Run `familiar-ai drive` with an allowlist and finite warrant.
2. Encounter a shared preflight, review, scope, checkpoint, or incomplete-
   implementation cascade.
3. Finish each retained PRD individually outside Familiar: inspect its
   worktree, repair it, test it, commit it, cherry-pick it, and manually update
   backlog state.

That is the same systemic failure recorded for Wave 3. Familiar remains an
expensive candidate generator while the operator remains the real scheduler,
circuit breaker, reviewer, merge queue, and completion authority. FAM-BUG-019
remains open until a multi-PRD wave reaches integration and completion using
only Familiar commands.

## Sessions and outcomes

| Session | Result | Material failure |
| --- | --- | --- |
| `drive-00001788204365538829-0000021924-000000` | preflight failed | An unused Unsloth credential blocked every routed worker after a long silent preflight. |
| `drive-00001788205457749646-0000034131-000000` | retained/cascade | PRD-032 review output was malformed; the driver continued toward later work instead of tripping a circuit breaker. |
| `drive-00001788209411219028-0000034357-000000` | retained/cascade | PRD-055 was classified `scope_broadened` for a required MCP integration test, then PRD-056 was admitted on an unintegrated base. |
| `drive-00001788211246708225-0000073195-000000` | retained/cascade | PRD-056 openly remained acceptance-incomplete and the driver proceeded to PRD-062. |
| `drive-00001788227852882169-0000063382-000000` | retained | PRD-062 implemented successfully, then an incompatible Ollama reviewer forced `human_review_required`. |

PRD-062 also required five earlier drive sessions that never reached a claim:
`drive-00001788219360789723-0000099487-000000`,
`drive-00001788220274356121-0000019735-000000`,
`drive-00001788220805342941-0000028460-000000`,
`drive-00001788221441762509-0000036706-000000`, and
`drive-00001788223116868891-0000046930-000000`. Each spent roughly five to ten
silent minutes in workspace-test preflight and returned only exit code 101.

## Landed work

| PRD | Landed result | Recovery reality |
| --- | --- | --- |
| PRD-032 | Empirical worker probation and promotion | Familiar invalidated its own remediated checkpoint; recovered manually. |
| PRD-055 | Capability-scoped Familiar MCP | Correct candidate retained by an over-narrow expected-files judgment; recovered manually. |
| PRD-056 | Daemon-owned control plane | Preserved candidate lacked major acceptance-critical surfaces; two manual audits expanded it to 4,551 changed lines across 32 files. |
| PRD-062 | Local model artifact registry | Familiar implementation passed focused checks; incompatible review retained it; full workspace suite passed during manual recovery. |

## Product failures and friction

### 1. Cascade-then-manual delivery is the standard path

Every productive Wave 4 candidate required operator intervention after
Familiar stopped. The driver did not integrate a single PRD. It repeatedly
admitted later work after earlier candidates were retained or incomplete.

**Required correction:** make integration, not implementation narration, the
dependency boundary. A repeated deterministic fleet failure must stop the
session and emit one executable recovery plan. `resume all` must be capable of
finishing current preserved candidates rather than forcing manual Git work.

### 2. Preflight executes a lossy verification contract

Preflight converts configured review verification into
`PreflightCommandConfig`, dropping the declared environment and timeout. It
also redirects output to null. PRD-062 therefore paid for repeated full-suite
runs that failed without a test name, captured output, or useful remedy.

**Required correction:** execute the exact configured verification spec,
retain bounded redacted output, enforce its timeout, and distinguish test
failure from environment denial. This is FAM-BUG-024.

### 3. Long-running preflight remains effectively invisible

The operator received no active check name, elapsed time, test progress, or
bounded output for five-to-ten-minute intervals. The only terminal evidence was
`command exited with code Some(101)`.

**Required correction:** emit heartbeats with the active check, elapsed time,
child identity, and last durable transition. Preserve the terminal evidence in
the session report.

### 4. Review routing ignores demonstrated protocol capability

PRD-062 routed structured review to `llama3:latest`. Ollama explicitly reported
that the model does not support tools. Familiar retried the same deterministic
failure three times, with five reconnects per attempt, then labeled the result
`HumanReviewRequired`.

**Required correction:** structured-output and tool support must be probed and
persisted before selection. Deterministic capability failure must quarantine
the worker for the session and reroute, not masquerade as human judgment. This
is FAM-BUG-025.

### 5. Familiar invalidates work produced by its own remediation

PRD-032 could not resume because the durable checkpoint hash did not advance
after Familiar's remediation modified the candidate.

**Required correction:** successful remediation must atomically advance the
checkpoint manifest and review lineage. This is FAM-BUG-021.

### 6. Scope policy rejects required verification files

PRD-055 was retained because an MCP integration test was outside a literal
expected-files interpretation, despite the PRD requiring deterministic offline
query coverage and declaring the MCP surface.

**Required correction:** scope authority must represent required tests and
declared directory surfaces without treating legitimate coverage as an attack.
Ambiguity must pause with a hash-bound approve/reject action.

### 7. Incomplete implementation narration is not a circuit breaker

The initial PRD-056 worker explicitly named missing daemon transport, detached
lifecycle, MCP isolation, worker adoption, and capability-session work. The
driver retained the candidate and moved on instead of stopping the wave.

**Required correction:** an implementation that admits unmet acceptance
criteria must be terminally classified as incomplete and block dependent
admission. Verification prose cannot substitute for acceptance evidence.

### 8. Patch application remains brittle

PRD-062 hit another exact-context `apply_patch verification failed` while
updating migration-count fixtures. The worker recovered, but this remains noisy
and wastes budget.

**Required correction:** refresh the hunk after the first mismatch, bound the
retry, and record recovered patch misses as telemetry.

### 9. Model-cache and model-discovery protocols remain noisy

The run invalidated Codex's incompatible `models_cache.json`, and Ollama's
OpenAI-compatible model list did not match the Codex models-manager schema.
Both emitted repeated errors during otherwise understandable failures.

**Required correction:** keep runtime discovery schemas adapter-specific,
invalidate cache incompatibility once, and never interpret an Ollama model list
as a Codex-native models response.

### 10. Green migration fixtures did not represent the production database

After PRD-062 passed its candidate workspace suite and was installed,
`familiar-ai next` failed because migration 051 attempted to update an existing
immutable Ollama worker spec. Fresh test databases had no such row, so every
migration test remained green. Release verification, not Familiar review,
caught the blocker.

**Correction landed:** migration 051 now preserves immutable worker history and
creates only the degraded artifact/alias mapping. A populated pre-051 fixture
pins the real upgrade path. This is FAM-BUG-026.

## Verification and closeout

- PRD-062 focused artifact, storage, migration, and configuration tests passed.
- `cargo fmt --all -- --check` passed on the candidate.
- `cargo test --workspace` passed completely during manual recovery.
- The PRD-062 candidate was committed in its preserved worktree, cherry-picked
  to `main`, and manually marked complete because Familiar's reviewer could not
  produce a valid terminal review.
- FAM-BUG-019, 021, 022, 023, 024, and 025 remain open where noted.

Wave 4 delivered useful code, but it did not validate Familiar's autonomous
workflow. It reinforced the opposite conclusion: until the cascade-to-manual
pattern is broken, successful code delivery is evidence of operator recovery,
not product reliability.
