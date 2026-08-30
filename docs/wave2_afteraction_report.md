# Wave 2 After-Action Report

**Date:** 2026-08-30  
**Wave:** PRD-041, PRD-048, PRD-049, PRD-051  
**Result:** Complete; all four implementations are integrated, durable backlog
state advances to PRD-050, formatting is clean, and the full workspace test
suite passes.

## Delivered

- PRD-041: one bounded stronger-worker retry after a pre-review verification
  failure, with durable linkage, warrant enforcement, and reporting.
- PRD-048: internally recorded SSH deploy targets, assurance tiers, delivery
  recipes, smoke evidence, and repository-scoped internal gates.
- PRD-049: explicitly approved and drift-sensitive `familiar.toml`, machine
  bindings, effective-configuration provenance, and project status.
- PRD-051: an append-only usage observation ledger with distinct token
  categories, exact nanoUSD normalization, project identity, sanitized
  evidence, and idempotent accounting recovery.

Integrated commits:

- `eba7c6c` — PRD-041
- `66cf861` — PRD-048
- `19c90d4` — planned-output context correction required by PRD-049
- `b223e0c` — PRD-049
- `c14a2a5` — PRD-051

## What worked

- The immutable allowlists held. Neither drive session attempted a PRD outside
  its approved set; PRD-050 was explicitly recorded as `excluded_allowlist`.
- Dependency admission and deterministic ordering were correct. `next` now
  reports PRD-050, proving all Wave 2 dependencies are durably complete.
- Scope-overlap decisions were explicit and durable, and the driver reported
  `achievable_width=1 requested_width=4` rather than pretending useful
  concurrency existed.
- Independent review caught material defects in PRD-041 and PRD-049 and drove
  bounded remediation.
- Preserved worktrees retained every candidate needed for manual recovery.
- Focused tests plus independent full-workspace reruns prevented several unsafe
  implementations from being landed.

## Product defects and friction observed

### 1. The execution plan overstated Wave 2 concurrency

All four PRDs overlap in configuration, daemon tests, storage repositories, or
`run.rs`. The corrected scheduler honestly reduced the requested width of four
to one. `EXECUTION-PLAN.md` still describes width four without distinguishing
the graph width from the achievable mutable-scope width.

**Required correction:** calculate and record achievable width from real
authoritative scopes when a wave is authored, and fail plan validation when a
claimed width cannot be reached.

### 2. Serialized execution did not serialize integration

Familiar marked PRD-041 complete and immediately started PRD-048 without
integrating PRD-041 into the base revision. Both independently created
migration 027. The same failure repeated after PRD-049: PRD-051 started from
the pre-049 base and both independently created migration 029.

**Impact:** work was serialized for file safety but still built against stale
state. Manual rebases and migration renumbering to 028 and 030 were required.

**Required correction:** a selected PRD must not release overlapping scopes or
admit a later overlapping PRD until its reviewed candidate is integrated into
the session base. Persist and advance a session integration revision.

### 3. “Completed” meant backlog-complete, not integrated

PRD-041 and PRD-049 transitioned to completed while their implementations
existed only as dirty isolated worktrees. Familiar neither committed nor merged
them, yet scheduled subsequent work as though completion were available.

**Required correction:** separate `review_complete`, `integration_pending`, and
`integrated` states. Dependencies and scope release must consume `integrated`,
not merely a clean review disposition.

### 4. Dependency-manifest review still has no approval interaction

PRD-048 and PRD-051 necessarily changed `Cargo.toml` and `Cargo.lock`. Scope
classification stopped before substantive independent review, and `resume`
only printed `HumanReviewRequired` before preserving the checkpoint because
stdin was non-interactive. No hash-bound approve/reject command or prompt was
offered.

**Impact:** the operator had to review, correct, test, commit, rebase, cherry-
pick, and use `backlog complete` manually. Checkpoint hashes then became stale,
making `approve-and-complete` unusable.

**Required correction:** expose an interactive and scriptable scope-decision
command bound to the finding and candidate hashes. Permit narrowly configured
PoC self-approval and retain review-gated policy for controlled environments.

### 5. Scope classification masked substantive defects

Because manifest/lockfile ambiguity stops before independent review, PRD-048's
candidate never received substantive agent review. Manual review found that a
verification gate could be satisfied by evidence from another repository.

**Integrated correction:** verification evidence is now selected by exact
check id and durably joined through driver session repository identity, with a
cross-repository rejection test.

**Required product correction:** human-only file classes should pause the
authority decision without suppressing read-only substantive review.

### 6. PRD-041 initially disabled escalation under every finite duration warrant

The first implementation rejected all escalation whenever
`max_duration_ms != 0`, ignoring elapsed and remaining time. Independent review
caught it; remediation added an explicit reservation and a remaining-duration
execution cap.

### 7. PRD-041 completed with an unresolved end-to-end test gap

The second review recorded that unit tests covered worker selection, warrant
arithmetic, and the uniqueness constraint separately but did not exercise one
cheap failure followed by exactly one stronger attempt across crash recovery.
The finding remained open while the disposition was `ReadyForHumanApproval`
and Familiar automatically marked the PRD completed.

**Required correction:** a clean terminal disposition must not coexist with an
open acceptance-criterion test gap unless an explicit human waiver is recorded.

### 8. PRD-048's reported verification did not match reality

The worker said focused migration tests passed and attributed the only
workspace failures to sandboxed log creation. An independent run found six
real migration fixture failures: migration 028 had been registered without
updating version/count assertions.

**Integrated correction:** all migration fixtures were updated and the full
workspace suite passed after rebase.

**Required correction:** final summaries must be generated from durable check
exit records, not agent narration. Contradictory claims should fail the phase.

### 9. A planned contract output was treated as a missing authoritative input

PRD-049 was retained before implementation because
`docs/contracts/familiar-toml.md` did not exist, even though structured
`expected_files` explicitly authorized the PRD to create it. The failure was
classified as `unclassified_result`, terminating the first Wave 2 session and
preventing PRD-051 from running.

**Integrated correction (`19c90d4`):** missing Markdown references are skipped
only when structured metadata declares the same path as an expected output;
ordinary missing references still fail closed.

**Required correction:** classify context-compilation failures precisely and
continue to other allowlisted PRDs when the failed PRD is retained.

### 10. Removing an approved `familiar.toml` hid drift

After PRD-049's first remediation, status returned `familiar.toml: absent`
before loading the durable approval. Deleting an approved snapshot suspended
authority but hid the approved hash and deletion diff. Independent review
found the defect, but it remained open while Familiar marked the PRD complete.

**Integrated correction:** status now consults durable approval first and
renders deletion as drift against empty current content, with a regression
test.

### 11. PRD-051's migration silently erased execution-history invariants

The candidate used `CREATE TABLE execution_history_new AS SELECT ...`, which
dropped primary-key, `NOT NULL`, type, and terminal-outcome constraints. A
unique index restored only one small part of the original schema.

**Integrated correction:** migration 030 performs an explicit schema-preserving
rebuild, removes only the obsolete `agent = 'codex'` restriction, and tests the
retained primary key, nullability, outcome check, and widened adapter field.

### 12. PRD-051 accounting ingestion was not crash-atomic

Evidence, observation, and cost facts were inserted in separate autocommit
steps. A crash after evidence insertion made replay see the source hash and
skip the missing observation or cost forever.

**Integrated correction:** evidence and observation now commit in one
transaction; replay returns the existing observation id; cost facts have a
unique observation/provenance key and idempotent insertion.

### 13. Recovery inventory disagrees with completed backlog state

After all four PRDs were durably completed, `resume all --dry-run` still
reported PRD-041 and two separate PRD-049 worktrees as
`implemented_pending_review`, plus PRD-048 and PRD-051 as stale/invalid
checkpoints. It offered completed PRDs for recovery and duplicated one PRD.

**Required correction:** recovery discovery must reconcile against current
backlog completion and integrated commit evidence, suppress superseded leases,
and group multiple candidates for one PRD explicitly instead of listing them
as independent resumable work.

### 14. Recovery phase and backlog status disagreed for PRD-049

`resume all --dry-run` called PRD-049 an invalid checkpoint, while
`backlog release` rejected the advertised recovery because the actual backlog
status was already pending rather than in progress.

**Required correction:** recovery output and suggested commands must be
computed from one reconciled transactionally consistent snapshot.

### 15. Long stages remained silent and poorly observable

Both drive sessions emitted only their warrant and then remained silent for
long periods. Progress output arrived in one large buffered block after the
process had already exited.

**Required correction:** emit bounded periodic phase heartbeats with PRD,
stage, elapsed time, child identity, and last durable transition. Ensure output
is flushed as events occur.

### 16. Codex model-cache schema errors flooded every run

`missing field base_instructions` was emitted repeatedly by model-cache load
and TTL renewal during otherwise successful work.

**Required correction:** invalidate an incompatible cache once, refresh it,
and emit one bounded diagnostic rather than thousands of false error lines.

### 17. Verification environments remained inconsistent

Agent summaries repeatedly attributed daemon process-test failures to sandbox
log-file permissions, while the same full workspace suites passed when rerun
under the operator-authorized environment.

**Required correction:** declare verification environment identity as part of
the check, preflight required writable paths, and classify environment denial
separately from implementation failure.

### 18. Metadata-check guidance is not operational under incremental policy

The execution plan says to run `backlog metadata-check` before a wave, but it
exits nonzero because 41 legacy PRDs remain under `policy=incremental`, even
though execution admission works and `next` resolves correctly.

**Required correction:** distinguish diagnostic migration debt from blocking
metadata invalidity, or provide separate `--strict` and advisory exit modes.

## Verification and reconciliation

- Each integrated candidate passed focused tests after manual findings were
  corrected.
- PRD-048 and PRD-051 passed `cargo test --workspace` after rebasing onto the
  actual integrated predecessor state.
- `cargo fmt --all -- --check` and `git diff --check` passed.
- The only recurring compiler warning is the pre-existing unused
  `enqueue_initial_scan` function.
- Durable backlog state now selects PRD-050, the first Wave 3 item.

## Recommended action before or during Wave 3

Prioritize a narrow orchestration follow-up covering defects 2–4 and 13–15:
integration-aware completion, session-base advancement, interactive hashed
scope approval, reconciled recovery inventory, and live phase heartbeats.
Without that work, Familiar can still implement PRDs, but every overlapping
candidate risks stale-base migrations and substantial manual landing work.

The remaining issues should be attached to their owning boundaries: review
terminal semantics (7), durable verification truth (8 and 17), context failure
classification (9), model-cache lifecycle (16), and metadata adoption UX (18).
