# Inference Providers Contract

Part of the [provider configuration contract family](providers-index.md).

## Core invariants

- Provider and model identifiers are stable, validated strings.
- Familiar probes a provider before persisting it and fails closed when the
  endpoint or required authentication is unavailable.
- `kind = "inference"` with `runtime = "unsloth"` identifies an externally
  managed Unsloth Studio endpoint. The CLI accepts `--kind unsloth` as shorthand.
  Familiar discovers it through authenticated OpenAI-compatible `/v1/models`;
  authentication must be an `env: NAME` reference and credential bytes are
  never persisted. This runtime identity does not imply OpenAI behavior.

See [credential authentication](providers-credential-authentication.md) for
how authentication is supplied, and
[registry migration](providers-registry-migration.md) for discovery-result
caching.

A PRD that adds a new inference adapter (a new `runtime` value) owns its own
`providers-inference-<adapter>.md` document rather than editing this shared
document.
