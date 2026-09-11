# Provider Configuration Contract — Index

Familiar's provider configuration boundary is split into one document per
domain so that PRDs touching only one domain can declare only that file.
This index is the stable entry point; it never moves.

## Domains

- [providers-inference.md](providers-inference.md) — inference endpoint
  discovery, probing, and caching.
- [providers-credentials.md](providers-credentials.md) — bring-your-own
  authentication descriptors and credential handling.
- [providers-billing.md](providers-billing.md) — billing-source
  configuration for provider cost collection.
- [providers-deploy.md](providers-deploy.md) — deploy-target configuration
  for provider-backed delivery recipes.
- [providers-registry.md](providers-registry.md) — provider registry
  mutation and migration discipline.

## Cross-domain invariants

These apply to every provider domain document above and are not
duplicated in any of them:

- Provider and model identifiers are stable, validated strings.
- Provider kinds and their typed extensions are added by the PRD that
  introduces them. Unknown kinds and unknown extension fields fail
  validation closed.
