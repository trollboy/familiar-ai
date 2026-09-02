# Provider Configuration Contract — Index

This is the stable index for Familiar's provider configuration boundary.
Provider entries identify endpoints and describe how an operator supplies
authentication; they never contain credential values. The invariants below
apply to every provider kind; each linked document covers the invariants
specific to its domain.

## Core invariants

- Provider and model identifiers are stable, validated strings.

Provider kinds and their typed extensions are added by the PRD that introduces
them. Unknown kinds and unknown extension fields fail validation closed.

## Domain documents

- [Inference providers](inference-providers.md) — endpoint discovery,
  probing, and runtime identity (including `runtime = "unsloth"`).
- [Billing sources](billing-sources.md) — organization billing collection.
- [Deploy targets](deploy-targets.md) — remote deploy-recipe endpoints.
- [Credential authentication](credential-authentication.md) — BYO-auth
  descriptors and the credential non-persistence boundary.
- [Registry migration](registry-migration.md) — configuration mutation
  and migration bookkeeping.
- [Anthropic adapter](anthropic-adapter.md) — the `anthropic-api` raw
  runtime: PRD-058 wire mapping, stop reasons, caching, and billing mode.
