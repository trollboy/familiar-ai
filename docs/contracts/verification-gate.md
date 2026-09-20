# Verification gate

**PRD-099.** What verification means in this repository, who runs it, and what
each answer is allowed to mean.

## The definition

`scripts/gate.sh` is the gate. It is the only statement of what verification
is, and every caller runs it without substituting or adding steps:

| Caller | Invocation |
|---|---|
| The pre-push hook | `familiar-ai gate run`, which runs it and records the verdict |
| A contributor | `docker compose run --rm test`, or `scripts/gate.sh` directly |
| The compose service | its `command:` is `scripts/gate.sh` |

Two lists that are supposed to match will eventually not match, so there is one
list. `crates/familiar-ai-daemon/tests/gate_contract.rs` fails the build if a
`cargo fmt`, `cargo clippy`, `cargo test` or workspace `cargo build` appears in
`docker-compose.yml`, in `README.md`, or in the hook.

What the steps *contain* — which features they enable, which system
dependencies they need, what they compile — belongs to PRD-092, not here. This
contract is about the gate running at all.

## Triggers

**Verification is local.** This is a desktop application that runs on the
machine doing the work, not a service deployed from a pipeline. There is no
hosted runner, no build minutes, and no network required to know whether a
commit was verified.

The trigger is `scripts/hooks/pre-push`, installed with:

```bash
ln -sf ../../scripts/hooks/pre-push .git/hooks/pre-push
```

It runs `familiar-ai gate run`, which executes the single definition and
records the verdict against `HEAD` in Familiar's own ledger. Nothing leaves
the machine unverified without that being visible afterwards.

The bypass is `git push --no-verify`. It is deliberately available and
deliberately leaves a trace: the pushed commit has no recorded verdict, so
`familiar-ai gate status` answers `absent` for it. A bypass you can see
afterwards is worth more than one that cannot happen.

**Why not a hosted runner.** An earlier cut of this PRD ran the gate in GitHub
Actions on push and pull request. That was webapp-shaped thinking applied to a
local client: it cost around eight minutes of cold-cache compilation per run,
fired on every work-in-progress commit, coupled verification to a forge whose
independence PRD-101 exists to establish, and made the answer to "is this
verified" depend on a network call. The objective — runs unasked, recorded,
queryable — never required any of it.

## The four answers

`familiar-ai gate status [--commit SHA]` reads Familiar's ledger and answers
for any commit, offline, exiting non-zero for anything but green.

| Answer | Meaning | Pass? |
|---|---|---|
| `green` | the gate ran to completion and succeeded | yes |
| `red` | it ran and did not succeed | no |
| `absent` | nothing ever verified this commit — including a `--no-verify` push | no |
| `unreadable` | a verdict is recorded but this build does not recognise it | no |

Three properties are load-bearing:

- **Incomplete is failed.** A cancelled run, a lost runner, a step that never
  executed, a conclusion that is missing — each is `red`. Absence of evidence is
  never recorded as evidence. A gate that reports green because it could not
  tell is worse than no gate, because it is trusted.
- **Absent is not red.** A commit nothing ever verified is a different fact
  from a commit that failed, and conflating them is how a project convinces
  itself that unverified code is merely unlucky.
- **The latest run is the truth about that tree.** Re-running the gate on a
  commit replaces its verdict, so a stale red does not outlive the fix and a
  stale green does not outlive a regression.

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

## Two paths, and why they differ

Verification runs in two places, with different isolation, and the difference
is deliberate rather than an oversight.

| Path | Runs | Isolation |
|---|---|---|
| **Candidate verification** — `[[review.verification]]` checks during `run`/`drive` | model-written code no human has reviewed | Docker, via `docker compose -p familiar-ai-verify run` |
| **`scripts/gate.sh`** — the pre-push hook and `gate run` | code already on a branch a human is pushing | the host, natively |

**Docker is the sandbox for unreviewed code.** A candidate's own new tests are
compiled and executed to verify it; that is model-written code running before
any human has read it. The agent's tool calls are sandboxed separately by
landlock (`crates/familiar-ai-agent/src/isolation.rs`), which does not cover
verification. The container is what covers it.

That is the *only* reason Docker is load-bearing here. It is not required to
install Familiar, not required to run the daemon, and absent from
`config/default.toml` entirely — the Docker argv lives in the operator's own
`[[review.verification]]` config and could be pointed at native `cargo`
commands tomorrow. Doing so would trade the sandbox away, knowingly.

**One compose project for every check.** All verification runs pass
`-p familiar-ai-verify`. Without it Compose derives a project name from the
worktree directory, so each PRD gets its own network *and its own
`cargo-cache` volume*: networks accumulate until the address pool is exhausted
and every run starts with a cold cache. That is not hypothetical — it
exhausted the pool on 2026-09-19 and failed an entire four-PRD wave, after
leaking silently since at least 2026-09-04. Verification dropped from
1,045–4,292s to 181s once the cache was shared.

## No setting outside this tree may weaken it

Everything that decides the verdict is a reviewed file here. `scripts/gate.sh`
branches on no environment variable that could skip a step, and the hook
carries no bypass of its own. A step that can be disabled by a setting
stored somewhere else is a step whose removal leaves no diff and no reviewer,
and `gate_contract.rs` fails the build if one appears.

## Scope

Out of scope here, with their owners: what the gate compiles and which features
it enables (PRD-092); which tests it selects and under what load (PRD-088);
declared, expiring gate exclusions (PRD-089); build-time ceilings; release
artifacts and installers; and self-hosted or multi-platform runners — one Linux
runner matching the documented environment is the whole of this contract, and a
macOS job is a successor with its own acceptance.
