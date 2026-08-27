# Structured PRD Contract v1

PRD documents may begin at byte zero (apart from an optional UTF-8 BOM) with
the following versioned front matter. This metadata is the sole authority for
identity, workflow status, dependencies, scope, acceptance, risk, and external
gates when present. Text in the Markdown body never adds to or overrides it.

```yaml
---
familiar_ai_prd: 1
id: PRD-030
status: ready
dependencies:
  - PRD-009
expected_files:
  - crates/familiar-ai-core/src/backlog.rs
acceptance_criteria:
  - All scheduling paths consume this metadata.
risk_classes:
  - scheduling
external_gates: []
---
```

The required fields are `familiar_ai_prd`, `id`, `status`, `dependencies`,
`expected_files`, `acceptance_criteria`, and `risk_classes`. `external_gates`
is optional. Version 1 accepts scalar strings and either block or inline lists.
Unknown and duplicate fields fail closed. Status is one of `draft`, `ready`,
`in_progress`, `completed`, or `blocked`. Expected files, acceptance criteria,
and risk classes must be nonempty.

Identity uses `PRD-<number><optional-lowercase-suffix>` canonically. A
numbered-slug repository may spell it `PRD <zero-padded-number><suffix>`; both
map to the same internal identity. Dependencies name individual identities.
Ranges are intentionally unsupported because their membership is ambiguous.
Duplicate, missing, self, and cyclic dependencies are rejected.

Repository configuration selects an explicit migration policy with
`prd_metadata_policy = "incremental"` (the default) or `"strict"`.
Incremental mode accepts historical canonical metadata only for documents
without front matter. Numbered-slug body prose remains opaque. Strict mode
rejects every document without v1 front matter. Discovery is always read-only.
`familiar-ai backlog metadata-check` emits one deterministic diagnostic per
document and fails while legacy documents remain; it never rewrites files.
