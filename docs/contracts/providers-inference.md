# Inference Provider Contract

Domain: raw and local inference endpoints — the `kind = "inference"` provider
entries an operator adds so Familiar can route work to a model.

- Familiar probes a provider before persisting it and fails closed when the
  endpoint or required authentication is unavailable.
- Discovery results are cached with their verification time; refreshing them
  is explicit.
- `kind = "inference"` with `runtime = "unsloth"` identifies an externally
  managed Unsloth Studio endpoint. The CLI accepts `--kind unsloth` as shorthand.
  Familiar discovers it through authenticated OpenAI-compatible `/v1/models`;
  authentication must be an `env: NAME` reference and credential bytes are
  never persisted. This runtime identity does not imply OpenAI behavior.

See [`providers.md`](providers.md) for the invariants shared across all
provider domains, and [`providers-credentials.md`](providers-credentials.md)
for how an inference provider's authentication is described.
