# Wave 3 After-Action Report

**Date:** 2026-08-31  
**Wave:** PRD-050, PRD-052, PRD-054, PRD-057, PRD-064, PRD-069, PRD-070, PRD-074, PRD-075  
**Result:** Complete. All nine retained candidates were recovered, reconciled,
verified, integrated into `main`, and marked complete. The backlog now advances
to PRD-032.  
**Session:** `drive-00001788151041624250-0000057833-000000`  
**Termination:** `budget_prds_exhausted` at 2026-08-31 06:19:14 UTC

The dogfood run exhausted its nine-PRD warrant after attempting every
allowlisted PRD. It produced nine preserved implementation candidates but no
clean terminal result. Manual recovery then reconciled and integrated every
candidate in dependency order because `resume all` was blocked by stale
historical checkpoints.

The dominant workflow failure must not be obscured by the eventual successful
code delivery: the actual process was **`familiar-ai drive` → cascade of nine
retained/failed attempts → individually finish every PRD outside Familiar**.
Familiar integrated zero Wave 3 candidates. The operator and Codex performed
the merge queue, conflict reconciliation, verification, backlog completion,
and release closeout that the product was expected to perform. This is tracked
as FAM-BUG-019 and remains open until a multi-PRD wave completes end to end
using Familiar commands alone.

## Current durable state

The driver admitted all nine requested PRDs but calculated
`achievable_width=1 requested_width=9`, so the wave executed serially. Every
attempt ended retained.

| Sequence | PRD | Durable outcome | Reason |
| --- | --- | --- | --- |
| 1 | PRD-050 | retained | `scope_ambiguous` |
| 2 | PRD-052 | retained | `human_review_required` |
| 3 | PRD-054 | retained | `human_review_required` |
| 4 | PRD-057 | retained | `verification_failed` |
| 5 | PRD-064 | retained | `human_review_required` |
| 6 | PRD-069 | retained | `scope_ambiguous` |
| 7 | PRD-070 | retained | `scope_broadened` |
| 8 | PRD-074 | retained | `human_review_required` |
| 9 | PRD-075 | retained | `verification_failed` |

All retained candidates were recovered from their isolated worktrees. Their
landed commits are:

| PRD | Landed commit | Result |
| --- | --- | --- |
| PRD-050 | `016f641` | Replaceable cloud deploy targets |
| PRD-052 | `7547897` | Authoritative Anthropic billing |
| PRD-054 | `7ed2a31` | OpenAI and Codex accounting |
| PRD-057 | `1186fed` | Complete worker-spec identity |
| PRD-064 | `247168b` | Typed resource reservations |
| PRD-069 | `34a45dc` | Native token compression |
| PRD-070 | `67f703c` | Daemon context service |
| PRD-074 | `7a4642a` | Platform credential stores |
| PRD-075 | `8fac79e` | Audited registry migration |

## Recovery before the active session

The first nine-PRD launch admitted a synthetic Claude model named `claude`.
Every attempt failed immediately because that model was not usable by the
installed Claude client. The claims were released, the Claude worker was
disabled, and the Codex worker was corrected from the synthetic model `codex`
to the locally advertised `gpt-5.6-sol` before relaunching the wave.

This recovered the run operationally, but model registration still lacks a
capability probe strong enough to prevent unusable workers from reaching
execution admission.

## Product defects and friction observed

### 1. The execution plan overstated Wave 3 concurrency

The authored plan expected useful parallelism, but Familiar computed an
achievable width of one because PRD-050's authoritative scope overlaps every
other Wave 3 candidate. The wave therefore runs one PRD at a time despite a
nine-PRD warrant.

**Required correction:** validate claimed wave width against authoritative
mutable scopes when the plan is authored and distinguish dependency-graph
width from actually schedulable width.

### 2. Serialized work still does not integrate before dependent admission

PRD-052 was retained and never integrated, but Familiar subsequently admitted
PRD-054. The PRD-054 worker had to recreate the minimum PRD-052 seam in its own
isolated worktree.

**Impact:** dependency admission is consuming an implementation/review state
instead of an integrated base revision. Later work can compile against a
different architecture from the candidate it depends upon.

**Required correction:** dependencies must consume a durable `integrated`
state and session base revision, never merely an attempted or retained state.

### 3. Manifest scope review remains a terminal wall

PRD-050 was retained as `scope_ambiguous` because its implementation changed
`Cargo.toml` and `Cargo.lock`. The batch's standing approval policy did not
produce an interactive prompt, a hash-bound approval command, or a configured
PoC self-approval path.

**Required correction:** pause with an actionable approve/reject operation
bound to candidate and finding hashes. Support explicit PoC auto-approval
without weakening review-gated production policy.

### 4. Independent Ollama review is deterministically incompatible

PRD-052, PRD-054, and PRD-064 reached `human_review_required`. Independent
review was routed to the locally installed Ollama 0.12.3, while the Codex
integration requires Ollama 0.13.4 or newer. Familiar retried the deterministic
failure three times and then represented the result as malformed or incomplete
review instead of an unavailable provider.

**Required correction:** provider preflight must include protocol/version
compatibility. Deterministic incompatibility should disable that worker for the
session, avoid retries, and fall through to another allowed reviewer or an
actionable operator decision.

### 5. Verification cannot reliably reach the configured Unsloth endpoint

Agent verification of the loopback Unsloth endpoint failed with `Operation not
permitted` inside the execution sandbox even though the service is available
to the operator environment.

**Required correction:** record verification-environment identity, preflight
required loopback/network access, and classify environment denial separately
from implementation failure.

### 6. Long-running stages remain effectively silent

The driver emits little useful progress while implementation and full-workspace
verification run. Durable attempt state can remain `preflight` while a worker
is actively editing or testing, leaving the operator unable to distinguish
progress from a hang without inspecting processes and worktrees manually.

**Required correction:** emit and flush bounded heartbeats containing PRD,
current stage, elapsed time, child identity, current check, and last durable
transition.

### 7. Codex model-cache schema errors continue after the planned fix

Runs continue to emit `missing field base_instructions` and cache-refresh
errors. The noise repeats during otherwise productive execution.

**Required correction:** invalidate incompatible cache content once, refresh
atomically, and emit a single bounded diagnostic. Add an installed-binary test,
not only a source-level fixture.

### 8. Patch application is brittle and noisy

Workers repeatedly attempted exact-context patches against text that had
already diverged, producing multiple `apply_patch verification failed`
messages before recovering.

**Required correction:** refresh the target hunk after the first mismatch,
bound retries, and report recovered patch mismatches as telemetry rather than
high-severity tool errors.

### 9. Worker registration accepts labels that are not executable models

Both `claude` and `codex` were initially registered as model names even though
they were adapter/product labels rather than confirmed local model identifiers.
The current run required manual machine-configuration repair.

**Required correction:** registration must distinguish adapter identity from
model identity and prove a worker with a harmless capability request before it
is eligible for scheduling.

### 10. Recovery is blocked by stale checkpoints for integrated predecessors

After the drive ended, `resume all --dry-run` treated obsolete Wave 2
worktrees for PRD-048 and PRD-051 as blocking stale checkpoints. It therefore
blocked the current Wave 3 candidates on predecessors that are already
integrated and durably complete on `main`.

**Required correction:** recovery planning must reconcile historical
checkpoints with current backlog and Git containment before building dependency
waves. A stale candidate for a completed PRD must be suppressed, not allowed to
invalidate its integrated dependency state.

## Positive observations

- Isolated Git worktrees preserved every implementation candidate.
- The immutable nine-PRD allowlist held.
- Familiar honestly reported achievable width one instead of claiming nine
  concurrent workers.
- Durable stewardship queries expose enough session and attempt state to
  reconstruct progress when console output is silent.
- After correcting worker registration, Codex `gpt-5.6-sol` has continued
  through the allowlisted wave without overwriting the main worktree.

## Recovery and closeout

Recovery was completed manually in dependency order. The major reconciliation
points were the provider configuration schema shared by PRD-050 and PRD-052,
the independently introduced Anthropic and OpenAI billing modules, cumulative
accounting fields shared by PRD-054/069/070, migration ordering through 044,
and PRD-075's conversion from the legacy registry shape introduced by PRD-057.

The combined integration gate found three issues that focused candidate tests
could not expose:

1. A legacy configuration sentinel violated the repository identity gate. It
   was renamed to a neutral internal value.
2. A drive-loop test still expected the pre-PRD-057 worker label instead of the
   new content-derived `wspec-sha256` identity. Its assertion now verifies the
   new contract.
3. Five storage migration fixtures were stale after migrations 039 through
   044. Their exact counts were advanced to the integrated schema.

Focused tests for every recovered candidate passed, followed by the complete
workspace test suite on the combined revision. Formatting and diff-integrity
checks also passed. All nine backlog records were transitioned to complete;
`familiar-ai next` now selects PRD-032.
