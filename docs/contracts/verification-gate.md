# Verification gate

**PRD-099.** What verification means in this repository, who runs it, and what
each answer is allowed to mean.

## The definition

`scripts/gate.sh` is the gate. It is the only statement of what verification
is, and every caller runs it without substituting or adding steps:

| Caller | Invocation |
|---|---|
| GitHub Actions | `.github/workflows/gate.yml` runs it inside the compose `test` image |
| A contributor | `docker compose run --rm test` |
| The compose service | its `command:` is `scripts/gate.sh` |

Two lists that are supposed to match will eventually not match, so there is one
list. `crates/familiar-ai-daemon/tests/gate_contract.rs` fails the build if a
`cargo fmt`, `cargo clippy`, `cargo test` or workspace `cargo build` appears in
a workflow file, in `docker-compose.yml`, or in `README.md`.

What the steps *contain* — which features they enable, which system
dependencies they need, what they compile — belongs to PRD-092, not here. This
contract is about the gate running at all.

## Triggers

Push to `main`, and nothing else. The trigger is unconditional: no
`workflow_dispatch`, no `if:` on the job or its steps, and no
`continue-on-error`. A verification that runs when someone chooses to run it
measures diligence, not correctness.

**Deliberately not on pull requests.** This is a locally running desktop
application, not a deployed service. The durable question CI can answer is
whether `main` is verified and whether that is recorded; a branch mid-
development is not an artifact that question is about. Firing on every push to
an open pull request spends several minutes of runner time per work-in-progress
commit and reports a verdict on code the author already knows is in flux.

Verification during development is the same definition run locally, where the
build cache is warm and the answer takes seconds:

```bash
docker compose run --rm test    # or: scripts/gate.sh
```

**Runs queue rather than cancel.** A cancelled run is reported as failure, so
cancelling a superseded run would brand its commit red forever on the strength
of a later push. `cancel-in-progress` is `false`, and `gate_contract.rs` fails
the build if that changes — the rule and its consequence live in different
files and must not drift apart.

## The four answers

`familiar-ai gate status [--commit SHA]` answers for any commit, and exits
non-zero for anything but green.

| Answer | Meaning | Pass? |
|---|---|---|
| `green` | the gate ran to completion and succeeded | yes |
| `red` | it ran and did not succeed — including cancelled, timed out, skipped, still queued, in progress, or completed with no conclusion | no |
| `absent` | nothing ever verified this commit | no |
| `unreadable` | the verdict could not be read: no network, no auth, a malformed response | no |

Three properties are load-bearing:

- **Incomplete is failed.** A cancelled run, a lost runner, a step that never
  executed, a conclusion that is missing — each is `red`. Absence of evidence is
  never recorded as evidence. A gate that reports green because it could not
  tell is worse than no gate, because it is trusted.
- **Absent is not red.** A commit nothing ever verified is a different fact
  from a commit that failed, and conflating them is how a project convinces
  itself that unverified code is merely unlucky.
- **Only the gate decides.** A check run named `gate` is the gate. Another
  workflow's green check on the same commit cannot make the verdict green;
  a commit with other passing checks and no gate run is `absent`.

## Overrides

A merge past a red or absent gate is possible. Doing it quietly is not.

```
familiar-ai gate require  [--commit SHA]   # refuses unless green or overridden
familiar-ai gate override --actor <who> --reason <why> [--commit SHA]
```

The override is a row in `gate_overrides` (migration 074) naming the commit,
the verdict being overridden, the actor, the reason and the time. Both the
actor and the reason are `NOT NULL` with a non-empty `CHECK`, so **an override
that names nobody cannot be written at all** — the refusal is a database
constraint, not a convention the caller has to remember. Recording the verdict
alongside the override keeps the record meaningful after the forge has aged out
the run it refers to.

`gate override` refuses when the gate is already green: there is nothing to
override, and a record suggesting otherwise would be a lie in the ledger.

## Precondition: branch protection

**Making the gate *required* is a one-time human step outside this repository**,
and it is named here rather than designed around.

Enabling GitHub Actions is already done. What remains is branch protection on
`main` with the `gate` check required. Until that is on, criterion 4 of PRD-099
is unenforceable by this repository: `gate require` will refuse a bad merge
when it is asked, but nothing compels the forge to ask it.

The honest order is:

1. Land the gate and let it run on a few merges.
2. Confirm it goes green, and that its verdict matches a local run.
3. *Then* turn on branch protection with `gate` required.

Turning protection on before the gate has ever run green refuses every merge on
day one, and the first thing anyone does then is switch it off — which is how
gates die. An unprotected branch makes the gate advisory, and the gate is
honest about that rather than reporting green.

## No setting outside this tree may weaken it

Everything that decides the verdict is a reviewed file here. The workflow uses
no `secrets.` or `vars.`, and `scripts/gate.sh` branches on no environment
variable that could skip a step. A step that can be disabled by a setting
stored somewhere else is a step whose removal leaves no diff and no reviewer,
and `gate_contract.rs` fails the build if one appears.

## Scope

Out of scope here, with their owners: what the gate compiles and which features
it enables (PRD-092); which tests it selects and under what load (PRD-088);
declared, expiring gate exclusions (PRD-089); build-time ceilings; release
artifacts and installers; and self-hosted or multi-platform runners — one Linux
runner matching the documented environment is the whole of this contract, and a
macOS job is a successor with its own acceptance.
