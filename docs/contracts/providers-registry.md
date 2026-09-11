# Provider Configuration Contract — Registry Migration

Part of the [provider configuration contract](providers-index.md); see that
document for cross-domain invariants shared with the other provider domains.

This document defines the audit discipline for mutations to the provider
registry (the operator-facing configuration store, not database schema
migrations).

## Invariants

- Configuration mutations preserve existing comments and record actor, time,
  command, and before/after content hashes.
