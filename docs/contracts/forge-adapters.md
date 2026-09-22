# Forge Adapters Contract

Part of the [provider configuration contract](providers-index.md). Covers the
`forge` identity that selects which forge grammar delivery speaks, and the
closed verb vocabulary every adapter implements against.

This PRD makes the **forge** pluggable — the thing that hosts change
requests, runs checks, and merges. It does not make the **version control
system** pluggable; Git and `git worktree` stay assumed. See PRD-097 for why
that boundary is deliberate.

## The verb vocabulary

Delivery issues exactly six forge verbs. This is the whole surface — it is
small because delivery's needs are small — and it is closed: delivery never
spells a forge command inline, only through one of these verbs.

| Verb          | Purpose                                              |
|---------------|-------------------------------------------------------|
| `publish`     | Open a change request from a branch against a base.   |
| `locate`      | Find the change request already open for a branch.    |
| `wait_checks` | Block until a change request's checks finish.         |
| `check_named` | Query one named check on a change request.            |
| `merge`       | Merge a change request and delete its source branch.   |
| `comment`     | Post a comment on a change request.                    |

## Forge identity

`forge` is declared in `[repositories."...".delivery]`, validated closed
against the known adapter set exactly as provider kinds are: `github`,
`gitlab`, `gitea`, or `none`. An unknown identity fails configuration
validation rather than silently behaving like GitHub.

`provider_argv` remains the executable/prefix override (a wrapper script, a
pinned binary path); it never redefines the verb grammar itself. The
grammar is fixed per forge identity.

A section that omits `forge` keeps speaking `github`, the pre-PRD-097
baseline, regardless of whether `provider_argv` happens to be populated —
inferring `none` from an empty `provider_argv` would let a missing or
mistyped adapter executable validate as an intentional "no forge" section
instead of failing closed. `forge = "none"` must be declared explicitly.

## `none`: no forge at all

`forge = "none"` is a first-class adapter, not a failure mode. Delivery
pushes the branch and stops at a terminal phase
(`awaiting_manual_publication`) that names the branch and the base, for a
human to open the change request by hand. It never fails with a missing
change-request-number diagnostic, because there was never a forge to return
one.

## Change request identity

A change request is identified by an opaque, adapter-supplied string (plus a
URL where the adapter reports one) — never `Option<u64>`. Nothing in
delivery parses or does arithmetic on it, but the id is fed back into that
same adapter's own verbs as a command-line argument, so it must stay in
whatever spelling those verbs accept — GitLab's `glab` wants the bare IID
`123`, not the `!123` a human reads on the merge request page. When an
adapter's argument spelling and human spelling diverge, the human spelling
goes in a separate `display` field instead of decorating `id`; a caller
showing a change request to a person uses `display`, falling back to `id`
when there is no separate display spelling. This is what lets a Gerrit-style
change id or a GitLab-style `!123` display round-trip through the delivery
journal without lossy parsing, while every verb still gets an id it can
actually run.

## Declining a verb

Not every forge can perform every verb. `gitea`'s `tea` CLI has no
CI-awareness in its grammar, so the `gitea` adapter declines `wait_checks`
and `check_named`. `forge = "none"` declines all six.

A declined verb is a typed, journaled outcome — never an error, and never a
silent skip that lets an automatic delivery mode claim an authority it never
exercised. When delivery reaches a verb the active adapter declines, it
stops at the `awaiting_merge_authority` phase (the same phase a
`reviewed_pr_manual` policy stops at) with a detail naming the forge and the
declined verb, and returns that as a successful outcome awaiting a human —
not a failure.

## Adapters

- **`github`** reproduces the `gh` CLI grammar exactly (`pr create`,
  `pr view`, `pr checks`, `pr check`, `pr merge`, `pr comment`). Declines
  nothing.
- **`gitlab`** mirrors `glab`'s `mr` grammar (`mr create`, `mr view`,
  `mr checks`, `mr merge`, `mr note`). Declines nothing. The cheap proof
  that the abstraction covers a near-clone of `gh`.
- **`gitea`** speaks `tea`'s `pulls`/`comment` grammar, genuinely different
  from both of the above. Declines `wait_checks` and `check_named`.
- **`none`** pushes the branch and stops; declines every verb.
