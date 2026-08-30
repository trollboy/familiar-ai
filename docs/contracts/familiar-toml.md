# `familiar.toml` contract

`familiar.toml` is untrusted, checked-in repository input. Its declarations
have zero authority until a human operator records approval of the exact
SHA-256 content snapshot. Approval records the canonical repository path,
actor, timestamp, content hash, and content. Editing, replacing, or removing
the approved file suspends all authority derived from it. Status displays the
approved/current hashes and a diff; re-approval records a new decision.
Revocation is an explicit durable decision.

## Closed shareable schema

The root accepts only `environments`, `profile`, `active_dir`, `archived_dir`,
`prd_metadata_policy`, `reference_roots`, `risk_vocabulary`, `review`,
`execution_context`, and `verification`. Unknown fields fail validation.
Environment entries accept only `requires` and `name`. Verification entries
accept only `check_id`, an argv array, and a repository-relative
`working_directory`. Existing closed schemas govern review, execution context,
and reference roots.

All strings are checked recursively. Credential-shaped values, absolute or
home-relative paths, malformed identifiers, and unknown fields fail closed.
Provider hosts, authentication, credentials, and bindings are not members of
this schema. Commands are represented only as argv arrays and may only become
effective from the exact approved snapshot.

## Declare and bind

An environment declaration names a portable requirement and logical
environment. The operator binds that logical name to an existing machine-local
provider with:

```text
familiar-ai config provider bind <environment-name> <provider>
```

Bindings live under the matching machine config
`[repositories."<canonical-path>".bindings]`; they are never written into the
repository. A missing or wrong-kind binding grants no authority. `status`
names each unbound requirement and prints the bind command.

## Resolution and provenance

For approved snapshots, each declared project value overlays the corresponding
machine-local repository value, which overlays the global default. Bindings
remain machine-local. `familiar-ai config show --effective` prints every
effective project value with its source layer. If `familiar.toml` is absent,
the existing user-repository/global resolution is unchanged.

The threat boundary assumes a repository may attempt command injection,
credential capture, endpoint redirection, or path escape. Closed decoding,
recursive scalar validation, exact-snapshot approval, drift suspension, and
machine-local endpoint binding are mandatory defenses; Git provenance alone is
not authority.
