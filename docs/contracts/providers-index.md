# Provider Configuration Contracts — Index

This is the index for Familiar's provider configuration boundary. Provider
entries identify endpoints and describe how an operator supplies
authentication; they never contain credential values. The boundary is split
by domain so that PRDs extending one domain do not declare the whole
document family as their scope:

- [Inference providers](providers-inference.md) — provider/model identity,
  endpoint probing, fail-closed discovery, and runtime-kind identification
  (e.g. `runtime = "unsloth"`).
- [Billing sources](providers-billing-sources.md) — cost/usage collection
  surfaces distinct from inference endpoints.
- [Deploy targets](providers-deploy-targets.md) — delivery/deploy recipe
  endpoints.
- [Credential authentication](providers-credential-authentication.md) —
  bring-your-own authentication descriptors and the credential-secrecy
  invariant.
- [Registry migration](providers-registry-migration.md) — discovery-result
  caching and configuration-mutation audit trail.

Provider kinds and their typed extensions are added by the PRD that
introduces them. Unknown kinds and unknown extension fields fail validation
closed. A PRD that adds a new inference adapter should add its own
`providers-inference-<adapter>.md` document rather than editing the shared
domain documents above.
