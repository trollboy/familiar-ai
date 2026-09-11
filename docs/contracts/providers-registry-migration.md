# Registry Migration Contract

Domain: how provider and worker registry configuration is mutated and
migrated over time, and how that history stays auditable.

- Configuration mutations preserve existing comments and record actor, time,
  command, and before/after content hashes.

See [`providers.md`](providers.md) for the invariants shared across all
provider domains.
