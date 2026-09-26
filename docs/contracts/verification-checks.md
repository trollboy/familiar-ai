# Named verification checks

Verification commands are global named definitions. Define a check once and
reference its name in the ordered verification assignment:

```toml
[checks.fmt]
argv = ["cargo", "fmt", "--all", "--", "--check"]
working_directory = "."
timeout_ms = 120000
required = true
path_prefixes = []
environment = { PATH = "/usr/local/bin:/usr/bin:/bin" }

[assignments]
verification = ["fmt"]
```

Every assigned or overridden name must exist. `argv` must be non-empty and
`timeout_ms` positive. Conflicting legacy definitions with the same name are
invalid. Configuration loading performs this validation, so the save path
cannot persist an invalid shape.

## Project assignment and overrides

A repository replaces the global ordered list and may override only the
fields that legitimately vary by project:

```toml
[repositories."/work/project".assignments]
verification = ["fmt"]

[repositories."/work/project".checks.fmt]
required = false
timeout_ms = 60000
working_directory = "."
environment = { PATH = "/project/bin:/usr/bin:/bin" }
```

Repository overrides may change `required`, `timeout_ms`,
`working_directory`, or `environment`. They cannot replace `argv` or
`path_prefixes`; those remain properties of the named global definition.

## Legacy rewrite

Legacy `[[review.verification]]` entries remain readable. On load, each entry
becomes a global named definition using its `check_id`, while authored order
becomes `assignments.verification`. Repository arrays become repository
assignments. Ordering is preserved and conflicting duplicate identifiers are
rejected. Numeric `review.verification.N` sections are deprecated and are not
shown in Settings.

## Execution and presentation

The effective ordered assignment is exactly the list executed and recorded by
review. Global Settings exposes named **Checks** definitions. Project Settings
shows the effective ordered **Verification** list, its global or project
origin, and per-project named-check overrides.
