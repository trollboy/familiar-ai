# Familiar Product Backlog

**Updated:** 2026-08-26
**Authority:** `docs/north-star.md`

This index reconciles the active backlog with current implementation. Location
and explicit status must agree: completed specifications live in `done/`; active
or partial work lives directly in `docs/prds/` until PRD-030 defines the next
structured layout contract.

## Critical path: “have at it”

1. PRD-039 — checkpointed execution and concurrent `resume all`. **Next.**
2. PRD-030 — structured PRD contract and profile-neutral dependencies. **Completed.**
3. PRD-031 — worker capability registry and deterministic routing.
4. PRD-019 — planner and one human batch-approval gate.
5. PRD-020 — reconcile parallel scheduling with the canonical graph.
6. PRD-033 — repository delivery policy and approval modes.
7. PRD-034 — portable persistent worker. **Completed.**
8. PRD-036 — repository onboarding.
9. PRD-037 — security and recovery burn-in.
10. PRD-038 — multi-repository product acceptance.

## Economy track

- PRD-024 — finish genuinely enforceable execution budgets. **Completed.**
- PRD-028 — cost-tiered review.
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

- PRDs 001–018, 021–027, 029, 030, and 034 are implemented and archived.
- PRD-020 is partial and remains active.
- PRDs 019, 020, 028, 031–033, and 035–039 are active; PRD-039 gates all
  other active work until resumable recovery is implemented.
- PRDs 028 and 029, and the completion intent for PRD-027, were recovered from
  `origin/prd-023-location-is-truth`; divergent old implementation commits were
  not merged.
- Historical wave-one documents in `done/` retain their legacy naming.

No specification is deleted merely because it is old. A future deletion must
name the duplicate or superseding PRD and preserve the decision in Git.
