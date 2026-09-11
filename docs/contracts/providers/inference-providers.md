# Inference Providers

Part of the [provider configuration contract](index.md).

- Provider and model identifiers are stable, validated strings.
- Familiar probes a provider before persisting it and fails closed when the
  endpoint or required authentication is unavailable.
- Discovery results are cached with their verification time; refreshing them
  is explicit.
- `kind = "inference"` with `runtime = "unsloth"` identifies an externally
  managed Unsloth Studio endpoint. The CLI accepts `--kind unsloth` as shorthand.
  Familiar discovers it through authenticated OpenAI-compatible `/v1/models`;
  authentication must be an `env: NAME` reference and credential bytes are
  never persisted. This runtime identity does not imply OpenAI behavior.

Provider kinds and their typed extensions are added by the PRD that introduces
them. Unknown kinds and unknown extension fields fail validation closed.
