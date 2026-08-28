# Familiar Product Backlog

**Updated:** 2026-08-27
**Authority:** `docs/north-star.md`

This index reconciles the active backlog with current implementation. Location
and explicit status must agree: completed specifications live in `done/`; active
or partial work lives directly in `docs/prds/` until PRD-030 defines the next
structured layout contract.

## Critical path: “have at it”

1. PRD-039 — checkpointed execution and concurrent `resume all`. **Completed.**
2. PRD-030 — structured PRD contract and profile-neutral dependencies. **Completed.**
3. PRD-031 — worker capability registry and deterministic routing. **Completed.**
4. PRD-033 — actionable review decisions and repository delivery policy. **Completed.**
5. PRD-019 — planner and one human batch-approval gate. **Completed.**
6. PRD-020 — reconcile parallel scheduling with the canonical graph. **Completed.**
7. PRD-034 — portable persistent worker. **Completed.**
8. PRD-036 — repository onboarding.
9. PRD-037 — security and recovery burn-in.
10. PRD-038 — multi-repository product acceptance.

## Economy track

- PRD-024 — finish genuinely enforceable execution budgets. **Completed.**
- PRD-028 — cost-tiered review. **Completed.**
- PRD-029 — stable prompt prefixes and cache economics. **Completed.**
- PRD-032 — model probation and empirical routing.

## Supporting product surface

- PRD-026 — complete per-repository execution policy resolution. **Completed.**
- PRD-035 — execution-era API/MCP/dashboard state surface.

Supporting work may proceed in parallel but does not replace the critical path.
PRD-035 becomes acceptance-critical only for facts PRD-038 requires clients to
query consistently.

## Delivery assurance policy

- Default: reviewed PR, manual merge and deployment authority.
- PoC: explicit, finite, visibly low-assurance self-approval warrant.
- Governed: independent review and separate approval evidence suitable for
  later control mapping; Familiar does not claim compliance certification.
- No policy: no merge or deployment.

## Reconciliation record

- PRDs 001–031, 033–034, and 039 are implemented and archived.
- PRDs 032 and 035–038 are active; checkpointed recovery
  is now available for their execution.
- PRDs 028 and 029, and the completion intent for PRD-027, were recovered from
  `origin/prd-023-location-is-truth`; divergent old implementation commits were
  not merged.
- Historical wave-one documents in `done/` retain their legacy naming.

No specification is deleted merely because it is old. A future deletion must
name the duplicate or superseding PRD and preserve the decision in Git.
