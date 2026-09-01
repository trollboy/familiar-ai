# Inference Providers Contract

Part of the [provider configuration contract](providers-index.md). Covers
`kind = "inference"` endpoints: discovery, probing, and runtime identity.

## Invariants

- Familiar probes a provider before persisting it and fails closed when the
  endpoint or required authentication is unavailable.
- Discovery results are cached with their verification time; refreshing them
  is explicit.
- `kind = "inference"` with `runtime = "unsloth"` identifies an externally
  managed Unsloth Studio endpoint. The CLI accepts `--kind unsloth` as shorthand.
  Familiar discovers it through authenticated OpenAI-compatible `/v1/models`;
  authentication must be an `env: NAME` reference and credential bytes are
  never persisted. This runtime identity does not imply OpenAI behavior.
