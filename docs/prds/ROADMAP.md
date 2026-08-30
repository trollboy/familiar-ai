# Familiar Product Backlog

**Updated:** 2026-08-29
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
- PRD-042 — repository risk vocabulary. **Completed 2026-08-29** — the first
  PRD executed end-to-end by Familiar's own autopilot on this repository
  (sonnet implementation, opus independent review, human risk acceptance).
- PRD-043 — risk-aware registry route rules. **Completed 2026-08-29** by the
  autopilot with reviewer-driven remediation; the remediation also delivered
  per-PRD route context, an advance on PRD-044.
- PRD-044 — per-PRD worker selection; retires `model_routes`. **Approved 2026-08-29.**
- PRD-045 — declared-risk review tiering. **Approved 2026-08-29.**
- PRD-041 — verification-failure escalation. **Approved 2026-08-29.**
- PRD-032 — model probation and empirical routing. Sequenced after PRD-041/044/045:
  its empirical scores require the reconciled registry routing and persisted
  risk inputs those PRDs create.
- PRD-040 — superseded by PRD-042–045 (owner-approved decomposition, 2026-08-29);
  retained as the design record.

## Supporting product surface

- PRD-026 — complete per-repository execution policy resolution. **Completed.**
- PRD-035 — execution-era API/MCP/dashboard state surface. **Completed 2026-08-30** —
  first fully autonomous finding→remediation→clean-review→completion cycle.

## Provider and delivery track (approved 2026-08-30)

- PRD-047 — provider/model configuration CLI: kind-tagged endpoints,
  BYO-Auth diagnostics, probe-before-persist, comment-preserving edits,
  decision-row audit.
- PRD-048 — internal delivery targets: Familiar as its own CI/CD at
  garage scale; deploy-target providers, recipes, smoke evidence, and the
  first `external_gates` resolver (internal evidence).
- PRD-049 — shareable project configuration: checked-in familiar.toml
  under declare-and-bind with an approved-snapshot authority gate.
- PRD-050 — cloud deploy targets (AWS/GCP/Azure/DO) as replaceable CLIs.

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
- PRDs 032, 035–038, 041, and 042–045 are active; checkpointed recovery
  is now available for their execution. PRD-040 is superseded by 042–045.
  PRDs 042–045 are this backlog's first contract-v1 front-matter documents.
- PRDs 028 and 029, and the completion intent for PRD-027, were recovered from
  `origin/prd-023-location-is-truth`; divergent old implementation commits were
  not merged.
- Historical wave-one documents in `done/` retain their legacy naming.

No specification is deleted merely because it is old. A future deletion must
name the duplicate or superseding PRD and preserve the decision in Git.
