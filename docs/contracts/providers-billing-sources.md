# Billing Sources Contract

Part of the [provider configuration contract family](providers-index.md).

Billing sources are cost/usage collection surfaces distinct from inference
endpoints (for example an organization billing API separate from the
inference API it bills for). No billing-source-specific invariants exist in
the provider contract yet; the PRD that introduces a billing source owns
this document and populates it under the same closed-validation, no-credential-
value invariants stated in the [index](providers-index.md).
