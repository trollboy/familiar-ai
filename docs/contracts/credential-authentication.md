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

## Identity selection (PRD-095)

*Who* Familiar acts as is a descriptor and stays on the configuration side of
the boundary above; *what proves* it is a credential and stays outside.

- A repository declares the identity its authored commits and published pull
  requests carry —
  `[repositories."<key>".delivery.identity]` with `author_name`,
  `author_email`, `forge_account`, and `account_probe_argv`. All four are
  names an operator could paste into a bug report.
- Absence is fail-closed at the authoring *and* publication boundaries, with a
  diagnostic naming the repository. Nothing falls back to the daemon's ambient
  account: on a host with one account that is accidentally right, and on a host
  with several it is silently wrong until a write fails.
- Identity is applied **per invocation** — `git -c user.name=…/user.email=…`
  for authoring, and `provider_env` for the adapter — never by toggling
  machine-global state. `gh auth switch` and `git config --global` are refused
  in configuration validation, not merely avoided: Familiar runs concurrent
  workers against multiple worktrees, so a global toggle around a critical
  section is a race, and two overlapping deliveries to differently-owned
  repositories would each publish under the other's account depending on
  scheduling.
- `provider_env` selects *which configuration the adapter reads*, never a
  bearer secret. Names shaped like credentials (`*TOKEN*`, `*SECRET*`,
  `*PASSWORD*`, `*PASSPHRASE*`, `*CREDENTIAL*`, `*_KEY`) are refused by
  validation, because a value placed there would reach process arguments and
  diagnostics. Secrets remain the business of `credential_references` and the
  bring-your-own mechanisms above.
- Before publication, `account_probe_argv` is run and its reported account
  compared against `forge_account`; a mismatch stops the delivery with a
  diagnostic naming the declared identity, the observed identity, and the
  repository, and is recorded in the delivery journal as `identity_mismatch`.
  This exists because the underlying provider failure is not legible on its
  own — an account without write access can return an opaque 500 that names
  neither identity nor permission.
