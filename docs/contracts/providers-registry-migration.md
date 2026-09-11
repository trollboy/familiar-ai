# Registry Migration Contract

Part of the [provider configuration contract family](providers-index.md).

## Core invariants

- Discovery results are cached with their verification time; refreshing them
  is explicit.
- Configuration mutations preserve existing comments and record actor, time,
  command, and before/after content hashes.
