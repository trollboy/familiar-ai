# Credential Authentication Contract

Part of the [provider configuration contract](providers-index.md). Covers
how operators supply authentication and the credential non-persistence
boundary, shared by every provider kind.

## Invariants

- Authentication is bring-your-own and represented only by a diagnostic
  descriptor such as a CLI login, environment-variable name, SSH agent, or
  `none`.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.
