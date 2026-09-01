# Registry Migration Contract

Part of the [provider configuration contract](providers-index.md). Covers
configuration mutation and migration bookkeeping.

## Invariants

- Configuration mutations preserve existing comments and record actor, time,
  command, and before/after content hashes.
