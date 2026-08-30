# Wave Fixes Review

**Date:** 2026-08-30  
**Reviewed range:** `5eb5858..163234c`  
**Primary change:** PRD-065 ready-set scheduling, warrant allowlists,
repository-identity policy resolution, and transactional
`approve-and-complete`.

## Assessment

The change is directionally correct and `cargo test --workspace` passes, but
PRD-065 should not be considered fully accepted until the findings below are
resolved. The highest-risk issue can create a false durable completion bound to
the wrong commit.

## Findings

### F1 — Approved candidate is not cryptographically bound to the commit

**Severity:** High  
**Files:** `crates/familiar-ai-daemon/src/bin/familiar-ai.rs`,
`crates/familiar-ai-storage/src/repos/backlog.rs`

`backlog approve-and-complete` accepts any nonempty `--commit` value. When the
flag is absent it uses `git rev-parse HEAD`, but it never proves that this commit
contains the checkpoint's approved candidate. Storage verifies only that the
caller repeats the checkpoint's `diff_hash`; the commit is stored as text in an
event detail.

This permits completion with an unrelated, nonexistent, or pre-candidate
commit. In the exact Wave 1 recovery that motivated this feature, defaulting to
`HEAD` before committing the candidate would have bound completion to the base
revision.

**Required remediation:**

- Resolve the commit to a real object in the checkpoint's repository.
- Reconstruct or calculate the candidate tree represented by the checkpoint.
- Require the supplied commit's tree to equal that approved candidate tree.
- Persist the approved hash and commit in typed columns, not only free-text
  event detail.
- Add rejection tests for nonexistent commits, the unchanged base commit, and
  a different valid commit.

### F2 — Declared resource conflicts are not implemented

**Severity:** High  
**File:** `crates/familiar-ai-daemon/src/drive.rs`

PRD-065 requires ready PRDs to serialize for overlapping mutable scopes **or an
explicitly declared resource conflict**. The implementation compares only
expected-file scopes. There is no structured resource declaration, conflict
comparison, `deferred_resource` storage value, or regression test.

Consequently two PRDs with disjoint files but a shared exclusive resource can
run concurrently, contrary to the scheduling contract. Migration 025's closed
decision vocabulary also omits `deferred_resource`.

**Required remediation:**

- Add a closed, validated resource-conflict field to the structured PRD
  contract or another explicitly authoritative scheduling input.
- Compare resource identifiers during ready-set selection.
- Persist `deferred_resource` with the resource and current holder named.
- Add selection, migration, rendering, and recovery tests.

### F3 — The Wave 1 width-six regression does not use the real scopes

**Severity:** Medium  
**File:** `crates/familiar-ai-daemon/src/drive.rs`

The regression reproduces Wave 1's dependency edges but substitutes artificial
disjoint expected-file scopes (`w/36/` through `w/47/`). The actual PRDs contain
broad and overlapping scopes, including shared core, daemon, storage, and
documentation surfaces.

The test therefore proves removal of dependency-component serialization, but
does not prove the stated acceptance criterion that the recorded Wave 1 input
selects all six. It can also conceal a mismatch between claimed width and the
width achievable under the new scope-conflict rules.

**Required remediation:**

- Build the regression fixture from the archived PRDs' actual structured
  metadata or pinned byte-exact fixture copies.
- Assert the honest achievable selection and every scope deferral.
- If width six is truly required, define a narrower mutable-scope authority
  that makes those jobs non-conflicting rather than fabricating test scopes.

### F4 — Repository-identity conflicts do not consistently fail closed

**Severity:** Medium  
**File:** `crates/familiar-ai-core/src/config.rs`

`repository_entry_checked` returns an exact path match before examining other
entries with the same Git common-directory identity. This allows a
worktree-specific entry to silently shadow another entry for the same
repository, despite the stated fail-closed rule.

Additionally, the compatibility `repository()` and `effective_execution()`
paths discard identity-resolution errors and fall back to defaults. Callers
using those APIs can therefore lose repository policy instead of receiving the
conflict diagnostic.

**Required remediation:**

- Resolve all configured paths to repository identities before selecting an
  entry, including exact-path candidates.
- Reject multiple entries for one identity unless an explicit, validated
  override contract exists.
- Remove or make fallible the APIs that convert resolution errors to defaults.
- Cover daemon, MCP, drive, run, and resume call sites with conflict tests.

### F5 — The merged revision is not formatting-clean

**Severity:** Low  
**Files:** changed Rust files in PRD-065

`cargo fmt --all -- --check` fails on the merged revision, including changes in
core configuration, daemon CLI/drive code, and backlog storage.

**Required remediation:** run `cargo fmt --all`, verify `cargo fmt --all --
--check`, and commit the mechanical result separately or with the corrective
changes above.

## Verification performed

- `git pull --ff-only`: clean fast-forward from `5eb5858` to `163234c`.
- `cargo test --workspace`: passed completely.
- `cargo fmt --all -- --check`: failed with formatting differences in changed
  PRD-065 files.

## Recommendation

Treat PRD-065 as implemented but remediation-required. Fix F1 and F2 before
running Wave 2; they are contract and correctness gaps. Fix F3 and F4 in the
same reliability pass so the scheduler's evidence and repository-policy
boundary are trustworthy. F5 is mechanical but should remain a required merge
gate.
