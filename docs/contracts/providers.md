# Provider Configuration Contract

This contract defines Familiar's provider configuration boundary. Provider
entries identify endpoints and describe how an operator supplies
authentication; they never contain credential values.

## Core invariants

- Provider and model identifiers are stable, validated strings.
- Authentication is bring-your-own and represented only by a diagnostic
  descriptor such as a CLI login, environment-variable name, SSH agent, or
  `none`.
- Familiar probes a provider before persisting it and fails closed when the
  endpoint or required authentication is unavailable.
- Discovery results are cached with their verification time; refreshing them
  is explicit.
- `kind = "inference"` with `runtime = "unsloth"` identifies an externally
  managed Unsloth Studio endpoint. The CLI accepts `--kind unsloth` as shorthand.
  Familiar discovers it through authenticated OpenAI-compatible `/v1/models`;
  authentication must be an `env: NAME` reference and credential bytes are
  never persisted. This runtime identity does not imply OpenAI behavior.
- Configuration mutations preserve existing comments and record actor, time,
  command, and before/after content hashes.
- Provider output is untrusted and credentials must not appear in
  configuration, process arguments, logs, reports, comments, or database rows.

Provider kinds and their typed extensions are added by the PRD that introduces
them. Unknown kinds and unknown extension fields fail validation closed.
