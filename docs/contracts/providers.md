# Provider Configuration Contract

This contract defines Familiar's provider configuration boundary. Provider
entries identify endpoints and describe how an operator supplies
authentication; they never contain credential values.

This document is the stable index. Domain-specific invariants live in:

- [`providers-inference.md`](providers-inference.md) — inference providers.
- [`providers-billing.md`](providers-billing.md) — billing sources.
- [`providers-deploy-targets.md`](providers-deploy-targets.md) — deploy targets.
- [`providers-credentials.md`](providers-credentials.md) — credential authentication.
- [`providers-registry-migration.md`](providers-registry-migration.md) — registry migration.

## Core invariants

These apply across every provider domain above.

- Provider and model identifiers are stable, validated strings.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.

Provider kinds and their typed extensions are added by the PRD that introduces
them. Unknown kinds and unknown extension fields fail validation closed.
