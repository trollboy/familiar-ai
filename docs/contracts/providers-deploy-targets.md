# Deploy Targets Contract

Part of the [provider configuration contract family](providers-index.md).

Deploy targets are delivery/deploy recipe endpoints (see
`DeployRecipeConfig` in `crates/familiar-ai-core/src/config/providers.rs`).
No deploy-target-specific invariants exist in the provider contract yet; the
PRD that introduces deploy-target discovery or probing owns this document
and populates it under the same closed-validation, no-credential-value
invariants stated in the [index](providers-index.md).
