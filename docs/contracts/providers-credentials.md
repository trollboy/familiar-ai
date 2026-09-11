# Provider Configuration Contract — Credential Authentication

Part of the [provider configuration contract](providers-index.md); see that
document for cross-domain invariants shared with the other provider domains.

This document defines how providers describe authentication and how
provider-sourced output is handled with respect to credentials.

## Invariants

- Authentication is bring-your-own and represented only by a diagnostic
  descriptor such as a CLI login, environment-variable name, SSH agent, or
  `none`.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.
