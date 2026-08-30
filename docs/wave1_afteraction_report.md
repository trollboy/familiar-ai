# Wave 1 After-Action Report

**Date:** 2026-08-30  
**Wave:** PRD-036, PRD-037, PRD-044, PRD-045, PRD-046, PRD-047  
**Result:** Complete; all six implementations are integrated and the workspace
test suite passes.

## Delivered

- PRD-036: repository onboarding and attributed policy generation.
- PRD-037: adversarial security and failure-injection burn-in.
- PRD-044: per-PRD worker routing with persisted routing inputs.
- PRD-045: declared-risk review tiering.
- PRD-046: dispositioned dogfooding defect ledger.
- PRD-047: provider/model configuration CLI, comment-preserving mutation, and
  durable configuration-decision audit.

## Product defects observed

### 1. Ready work was serialized by dependency-component partitioning

`drive --max-prds 6 --max-parallel-components 6` launched only PRD-036.
The scheduler partitions the whole dependency graph into weakly connected
components and permits only one active PRD per component. Independent ready
siblings therefore serialize merely because they share completed ancestors or
have downstream graph connections.

**Impact:** Wave 1's intended width of six collapsed to one. Concurrency flags
were accepted without reporting that useful parallelism was one.

**Required correction:** schedule from the current ready set. Dependencies are
admission gates, not mutual-exclusion edges. Serialize only overlapping mutable
file scopes or explicit resource conflicts. Persist each selection/defer reason
and test the actual Wave 1 graph at width six.

### 2. The driver crossed the approved wave boundary

After PRD-044 completed, the same `--max-prds 3` session selected PRD-041 from
Wave 2 instead of PRD-045 or PRD-047. The session had to be interrupted and the
PRD-041 claim explicitly released.

**Impact:** A count warrant cannot express an approved PRD set and may execute
work outside an operator's intended wave.

**Required correction:** add an immutable PRD allowlist or approved-wave input
to the session warrant (`--prd ...` and/or `--wave ...`). Selection must never
escape that set, and the resolved set must be recorded durably.

### 3. Isolated worktrees cannot reliably inherit repository policy

Repository configuration is keyed by the main worktree path. A preserved lease
worktree can miss that entry in a later process, making `resume` fail policy
resolution unless a temporary path-specific entry is added.

**Impact:** Manual parallel orchestration was unsafe as a workaround for the
scheduler defect.

**Required correction:** resolve worktrees through Git common-directory
repository identity and pin that policy identity in the checkpoint.

### 4. Headless implementation accepted an empty question-only turn

PRD-036's first implementer stopped after discovery with questions and no
edits. Familiar then spent verification and independent-review resources on
the empty candidate. PRD-046 repeated the pattern until review requested a
non-empty remediation.

**Required correction:** the implementation prompt must state that no human is
present and require bounded assumptions. A successful zero-diff result must
terminalize as `implementation_incomplete` before verification or review,
unless the PRD explicitly permits a no-op result.

### 5. Successful implementation was classified as a token-budget failure

PRD-047 implemented its full surface and passed its tests, but Familiar retained
it because reported implementation usage (14,079,211 tokens) exceeded the
8,000,000-token stage ceiling. Earlier recovered attempts showed the same
failure class, with cache-heavy usage making raw token totals misleading.

**Impact:** completed work skipped review and required `resume all` plus manual
recovery.

**Required correction:** enforce budgets before/during provider execution,
separate fresh/cache-read/cache-write tokens, and checkpoint successful output
before classifying an overage. A post-result overage may block further spend but
must not erase the completed phase.

### 6. Scope review has no interactive approval path

PRD-047's necessary `Cargo.toml` and `Cargo.lock` changes produced
`HumanReviewRequired`, but `resume all` could only preserve the checkpoint.
There is no command to inspect and approve the specific scope findings, despite
the configured proof-of-concept self-approval policy.

**Required correction:** provide an explicit audited command/prompt that shows
the exact findings and accepts or rejects their hashes. Support configured
self-approval for bounded PoC classes and review-gated approval for controlled
environments.

### 7. Recovery commands compose poorly around human completion

After human review, `backlog release` moved PRD-047 to pending, while
`backlog complete` expected an in-progress claim. Committing the reviewed
candidate also made the old checkpoint report `stale_base`.

**Impact:** individually valid recovery operations created a state that could
not be completed through the advertised command sequence.

**Required correction:** add a single transactional `approve-and-complete`
operation for a reviewed checkpoint, binding the approved candidate hash and
commit. Recovery help should print the valid next commands for the current
state.

### 8. A declared output file was treated as a missing input reference

PRD-047 declared `docs/contracts/providers.md` in `expected_files` because it
was authorized to create it. Context compilation failed before claim because
the file did not yet exist. A minimal contract bootstrap commit was required.

**Required correction:** distinguish authoritative input references from
authorized output paths. Nonexistent expected output files must not be loaded
as context; missing explicit input references must still fail closed.

### 9. Review retry exhaustion preserved fixable findings

PRD-037 used all three review attempts. The third review resolved earlier
findings but introduced two new, concrete test gaps, then terminalized as
`HumanReviewRequired` because the retry count was exhausted. Manual recovery
fixed malformed-line redaction and stale coverage identifiers.

**Required correction:** distinguish progress from retry loops. Permit a
bounded continuation when all previous blocking findings are resolved and the
new findings are non-repeated, or checkpoint an actionable remediation-only
state for `resume all`.

### 10. Verification behavior differed across sandbox boundaries

Some agent-run workspace attempts reported daemon rolling-log tests failing
with `Operation not permitted`, while clean reruns in the repository passed.

**Required correction:** verification must use one declared environment or
classify environment-denied checks distinctly from implementation failures.
Preflight should prove required writable paths before a costly stage starts.

### 11. Stale Codex model-cache errors polluted every long run

Repeated `missing field base_instructions` cache errors appeared throughout
successful implementation and review stages.

**Impact:** high-volume false error noise obscured actionable failures.

**Required correction:** invalidate incompatible cache schemas once, emit one
bounded diagnostic, and continue with a refreshed cache.

### 12. Configuration mutation was not rolled back on audit failure

Human review of PRD-047 found that the config file was renamed into place
before the decision row was inserted. A database failure could therefore leave
a successful mutation with no required audit row. The integrated implementation
now restores the prior file when audit insertion fails.

**Follow-up:** add an injected post-rename database-failure test and consider a
small durable mutation journal for crash consistency across the file/SQLite
boundary.

## Recommended immediate backlog action

Before Wave 2, create and prioritize a focused orchestration-reliability PRD
covering defects 1–3 and 7: ready-set scheduling, explicit wave/PRD warrants,
Git-common-dir policy resolution, and transactional reviewed-checkpoint
completion. These are one operational boundary and block trustworthy parallel
execution of every later wave.

The remaining issues should be entered as bounded follow-ups or attached to
their existing owning PRDs (usage accounting/reservations for defect 5,
verification escalation for defects 9–10, and provider/config durability for
defect 12).
