# Provider Configuration Contract — Index

This contract defines Familiar's provider configuration boundary. Provider
entries identify endpoints and describe how an operator supplies
authentication; they never contain credential values.

The contract is split by domain so pending work can declare a single
domain document instead of the whole contract. Domains populated today:

- [Inference providers](inference-providers.md) — provider/model identity,
  endpoint probing, discovery caching, and the `unsloth` runtime example.
- [Credential authentication](credential-authentication.md) — the
  bring-your-own authentication model and the untrusted-output boundary.
- [Registry migration](registry-migration.md) — configuration mutation
  provenance (actor, time, command, before/after content hashes).

Domains named by the wider provider taxonomy with no invariants recorded
here yet — billing sources, deploy targets — have no dedicated document
until a PRD introduces content for them; do not infer any policy for
those domains from their absence.

Per-provider-kind and per-runtime extension contracts (for example a raw
inference adapter's own wire-format notes) are not domain documents. Per
the inference-providers invariant below, they are added, as their own new
file under `docs/contracts/providers/`, by the PRD that introduces that
provider kind or runtime.
