# Credential Authentication

Part of the [provider configuration contract](index.md).

- Authentication is bring-your-own and represented only by a diagnostic
  descriptor such as a CLI login, environment-variable name, SSH agent, or
  `none`.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.
