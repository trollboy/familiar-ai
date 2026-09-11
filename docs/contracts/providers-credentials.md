# Credential Authentication Contract

Domain: how a provider entry, of any kind, describes the authentication an
operator supplies.

- Authentication is bring-your-own and represented only by a diagnostic
  descriptor such as a CLI login, environment-variable name, SSH agent, or
  `none`.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.

See [`providers.md`](providers.md) for the invariants shared across all
provider domains.
