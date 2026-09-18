# Familiar

> A witch's familiar does not cast the spells. It keeps the grimoire, prepares
> the circle, watches the summonings, and remembers everything — while the
> daemons come and go.
>
> *The Familiar remains. The summoned daemons simply become smarter.*

**Familiar is an agent-neutral engineering steward.** It holds the backlog, compiles
context, drives whatever coding agent you have installed, verifies the result
deterministically, has a *different* model review it adversarially, and accounts
for every token — so a single engineer can supervise continuous AI-driven
development without surrendering architectural judgment.

It is built to outlive every model, editor, and vendor it drives. Claude will
change. Codex will change. The next breakthrough model will change. Familiar
should not.

---

## Status

**Honest summary: the parts are built; the loop does not close. Two PRDs out of
thirty-nine attempts have ever been delivered without a human finishing them.**

Familiar can select a PRD, compile context for it, implement it with a real
coding agent, verify it deterministically, review it with an independent model,
and account for the cost. Each of those stages works. What has almost never
happened is all of them in sequence, unattended, ending in an integrated
candidate.

### Delivery record — this host only

Queried from `~/.local/share/familiar-ai/familiar.db` on the Linux box,
2026-09-17. **Read the scope carefully: the ledger is per-host and there is
no aggregate.** Every `repository_key` in this store is a `/home/trollboy`
path; the M1 Mac, which did the early heavy lifting and ran all of the Codex
work, keeps its own SQLite that nothing here can see. `driver_sessions` has
no host-identity column at all — PRD-091 exists to add one, for locking, and
even that would not merge histories.

So the numbers below are one machine's slice, not the project's record, and
nobody can currently produce the project's record.

| On this host | |
|---|---|
| Driver sessions | 26 |
| PRD attempts | 39 |
| Attempts with outcome `completed` | **2** |
| Attempts integrated by Familiar | **2** — PRD-53, PRD-81 |
| Largest wave integrated here | **1 PRD** |
| Last PRD attempt of any kind | 2026-09-05 |
| Adapters that have ever run here | `claude-code` only; `codex` never |

`driver_attempts.integrated_at` is the only integration record in the schema.
Everything else in `docs/prds/done/` was landed by a human, on one box or the
other.

### What is stopping it

| Retained reason | Count |
|---|---|
| Review precondition: PRD lacks an explicit Acceptance Criteria section | 6 |
| *(no reason recorded)* | 6 |
| `scope_broadened` | 5 |
| `scope_ambiguous` | 4 |
| `unclassified_result` | 3 |
| `interrupted` | 3 |
| `human_review_required` | 3 |

Scope authorization refuses nine. Six die on a review precondition before a
model is called. Six record no reason at all, which is measurement debt rather
than a category. One PRD was attempted six times in a single day and retained
six times for six different reasons.

### Cost

| | |
|---|---|
| Attributed spend, this host | **$359.07** |
| Attempts carrying no cost attribution | 19 of 39 |
| Cost per delivered PRD, this host | **≥ $179.54** — a floor on one machine, not a measurement |

The two deliveries: PRD-53 at $26.26 over 55.3 minutes, PRD-81 at $1.22 over
33.5 minutes, both `claude-code`/`sonnet`. The floor above is the honest
number because nineteen attempts carry no attribution at all; the true figure
is higher and nobody currently knows by how much.

Neither north-star metric below has ever been computed by a shipped command.
PRD-098 makes them one, and makes a documented claim the ledger does not
support fail a check.

| | |
|---|---|
| **Stage 1 — supervised overnight loop** | ✅ Built (adapter → isolation → driver loop → morning report) |
| **Stage 2 — full autopilot** | ⚠️ Planner and parallel worktrees built; the loop does not close unattended |
| **Wave Three — economy / multi-model routing** | ⚠️ Partly built — billing ledger, token discipline, local runtime landed; local workers are not yet reachable from dispatch (PRD-094) |
| **Verification gate** | ❌ No CI exists; the gate is four commands run by hand (PRD-099) |
| **Platforms** | Linux (kernel ≥ 5.13) and macOS. Windows unsupported |

Familiar is used daily on its own development, but it is developed by hand far
more than by itself. It is not something to point at an unfamiliar codebase and
walk away from.

---

## The two goals

1. **Cheap** — intelligent multi-step, multi-model orchestration.
2. **Autonomous** — an app-building autopilot that does not bother the human.

These are one war against a single enemy: **wasted work**. Every dollar an agent
system burns is one of three fires — rediscovering context it already had,
retry-looping on a bad approach, or confidently building the wrong thing. The
same three fires generate the interruptions. Kill the waste and the system
becomes cheap and quiet in the same stroke.

Autonomy does not mean never needing a human. It means the system **saves up**
its need for humans and spends it at chosen gates. A retained `in_progress` entry
with an exact reason at 3 a.m. is success; a $40 hallucinated feature is failure.

### North-star metrics

| Metric | Direction | Meaning |
|---|---|---|
| **Cost per accepted PRD** | down | Tokens per unit of merged, review-passed work — not raw spend |
| **Human touches per accepted PRD** | toward 1 | Every recovery command, override, and question counts. Floor is 1: the batch approval |

A change that moves neither metric is off-mission.

---

## How it works

```
docs/prds/*.md ──► discovery ──► dependency graph ──► claim
                                                        │
                                                        ▼
                              compiled context (hard token ceiling)
                                                        │
                                                        ▼
                                  coding agent (claude / codex)
                                                        │
                                                        ▼
                          deterministic verification (build, test, lint)
                                                        │
                                                        ▼
                    scope authorization ──► isolated adversarial review
                                                        │
                              ┌─────────────────────────┴──────────────┐
                              ▼                                        ▼
                     clean terminal result                   retained, exact reason
                              │                                        │
                              └──────────► morning report ◄────────────┘
```

Familiar never commits, merges, rebases, pushes, or deletes a worktree
containing changes. Landing work is a human act.

---

## Quick start

**Requirements:** Rust (stable), Docker (for the test environment), Linux with
kernel ≥ 5.13 or macOS, and at least one coding agent CLI on `PATH`
(`claude` or `codex`).

The CLI is a real prerequisite today, not a recommendation: `AgentAdapterKind`
is a closed enum of three vendor-CLI variants and `as_agent_entry` maps any
unrecognised runtime to Codex, so a raw-API or local worker cannot currently be
selected to implement a PRD even though the loop to run one is built. PRD-100
adds the bridge; PRD-101 makes a host with one API key and no vendor CLI a
supported installation and deletes this paragraph.

```bash
git clone git@github.com:trollboy/familiar-ai.git
cd familiar-ai

# Build the CLI. --no-default-features skips the tray feature.
cargo build --release --no-default-features -p familiar-ai-daemon --bin familiar-ai
install -m755 target/release/familiar-ai ~/.local/bin/

# Point it at a repository containing docs/prds/*.md
familiar-ai next                        # what would run, and why
familiar-ai run docs/prds/PRD-001.md    # implement one PRD
familiar-ai report                      # what happened overnight
```

---

## CLI

| Command | Purpose |
|---|---|
| `next` | Select the next eligible PRD without executing it |
| `run <prd>` | Execute one PRD with the configured agent |
| `drive` | Execute eligible PRDs unattended until the backlog is empty, nothing is eligible, or the budget warrant is exhausted |
| `report [session]` | One screen: what got built, what stopped and why, what it cost, what needs human judgment |
| `history` / `usage` | Execution records and honest token accounting |
| `backlog` | Inspect, recover, or roll back backlog state |

`drive` **requires** a finite warrant (PRD count, cost, or duration) and refuses
to start without one. Command-line flags may only *tighten* the configured
warrant, never loosen it.

### Operator repairs

Three repairs that used to require the source tree and `cargo run --example`
are shipped commands. Each writes durable orchestration state or consults the
scheduler, so each demands an explicit human actor and a reason, and each
refuses while the control-plane claim is live rather than racing a running
driver.

| Command | Repairs |
|---|---|
| `operator rebind` | `resume` refuses a candidate because a surgical edit moved its content away from the recorded hash |
| `operator set-phase` | A checkpoint is stuck in a phase the pipeline will not advance |
| `operator width` | An authored wave width disagrees with what the scheduler will actually admit |
| `stewardship substance` | Which approval substance hashes this repository has durably approved |

```sh
# Rebind a checkpoint to its worktree's current content after a repair.
familiar-ai operator rebind PRD-63 \
  --actor human:you --reason "fixed a dangling doc reference blocking resume"

# Move a stuck checkpoint through the audited transition API.
familiar-ai operator set-phase <checkpoint-id> implemented \
  --actor human:you --reason "verification passed out of band"

# Ask the scheduler what it will actually admit, for the whole backlog or a subset.
familiar-ai operator width PRD-63 PRD-72 \
  --actor human:you --reason "sizing the next wave"

# Read-only: the approval substance this repository has approved.
familiar-ai stewardship substance
```

Both hashes, the actor and the reason are recorded on the checkpoint's own
event trail, so a rebind is auditable rather than a silent overwrite.

---

## Configuration

Familiar reads `~/.config/familiar-ai/config.toml` (XDG; `Familiar-AI` on macOS).
Environment overrides use the `FAMILIAR_AI_` prefix — stale `FAMILIAR_` variables
fail closed rather than being silently ignored.

```toml
[driver]                                  # the unattended warrant
max_prds_per_session   = 3
max_session_duration_ms = 28800000

[agents.implementation]                   # who writes the code
adapter = "claude-code"
executable = "claude"
model = "sonnet"

[agents.reviewer]                         # must differ from the implementer
adapter = "claude-code"
executable = "claude"
model = "opus"

[review]
enabled = true
max_review_attempts = 3
max_total_tokens = 400000
allowed_paths = ["crates/"]

[execution_context]
hard_ceiling_tokens = 60000               # caps the compiled prompt
```

Agent selection is deterministic config, never a model choosing a model.
Same-model review is refused: independence is the point.

---

## Architecture

A Rust workspace of fourteen crates. The wave-two execution path is
`core → storage → agent → review → daemon`.

| Crate | Role |
|---|---|
| `familiar-ai-core` | Config, backlog domain, PRD discovery, paths, identity |
| `familiar-ai-storage` | SQLite, migrations, repositories (backlog, review, history, driver) |
| `familiar-ai-agent` | `CodingAgent` trait, Codex and Claude Code adapters, process isolation |
| `familiar-ai-review` | Scope authorization, evidence capture, review coordination |
| `familiar-ai-daemon` | `run`, `drive`, `report`, the `familiar-ai` binary |
| `familiar-ai-context` / `-tokens` | Context compilation with hard token ceilings |
| `familiar-ai-llm` | OpenAI-compatible backends (Ollama, vLLM, LM Studio, OpenRouter) |
| `familiar-ai-mcp` / `-summary` / `-watcher` / `-tray` | Wave-one memory surface (dormant w.r.t. the autopilot) |
| `familiar-ai-logging` / `-testutil` | Tracing setup; shared test fixtures |

### Key concepts

**PRDs are machine contracts.** Every PRD declares an `Expected Files` section in
a closed grammar — exact paths, `dir/`, or `dir/**`. It is parsed, pinned by
test, and used to authorize the change set. A change outside the contract is a
scope finding, not a merge conflict discovered later.

**Fail closed on unknown state.** Unknown token usage stays unknown — never zero
— because zero sails past every ceiling while unknown stops the run. The same
rule governs ambiguous scope, missing evidence, and unenforceable isolation.

**Review isolation is real.** Linux uses Landlock (kernel ≥ 5.13, allowlist-only,
`restrict_self` in `pre_exec`, refuses to run unless fully enforced); macOS uses
`sandbox-exec`. No host package, no container capabilities, no sudo. Hosts that
cannot enforce it fail closed with a naming diagnostic.

**Everything is audited.** Status transitions, recovery actions, and manual
overrides all require an explicit `human:<identity>` actor and a non-empty
reason, recorded durably.

---

## Core principles

From [`docs/philosophy.md`](docs/philosophy.md):

1. **The Repository Is Truth** — state lives in the repo, not in a model's head
2. **Determinism Before Intelligence** — parsers, git, compilers and tests answer first; a model is the *second* choice
3. **Context Is Precious** — bounded, compiled, and budgeted
4. **Humans Own Architecture** — decomposition and design are human gates
5. **Engineering Before Automation** — automate a discipline you already have
6. **Trust Is Earned Through Verification** — including trust in the models themselves
7. **Small, Bounded Work Wins**
8. **Agents Are Replaceable** — no vendor is load-bearing
9. **Memory Must Be Durable**
10. **Stewardship Over Control**
11. **Cost Is Never Fine Print** — spend increases are stated in dollars up front, never buried; utilization and latency decide serverless vs always-on, and unknown numbers get asked about

---

## Development

**Tests, linters, formatters and migrations run in Docker**, so the environment
is identical everywhere:

```bash
docker compose build test
docker compose run --rm test
```

That is the gate. The compose service runs `scripts/gate.sh`, which is the
single definition of what verification means here — the same file CI runs on
every push to `main` and every pull request. There is no second list of steps
to keep in sync, and `gate_contract.rs` fails the build if one appears.

To ask whether a commit has actually been verified, rather than assuming:

```bash
familiar-ai gate status              # HEAD of the default branch
familiar-ai gate status --commit SHA
```

It answers `green`, `red`, `absent` or `unreadable`, and exits non-zero for
anything but `green`. A commit that was never verified is `absent`, which is
deliberately not the same answer as `red` and is never reported as a pass.
See `docs/contracts/verification-gate.md`.

Host commands are for repository inspection and version control only.

Known environment quirks: the tester image excludes `.git/`, so a handful of
git-dependent tests fail there by design; `rustfmt`/`clippy` need
`rustup component add` at container runtime; the default `tray` feature has a
pre-existing compile error, hence `--no-default-features`.

### Contributing

Work is PRD-driven. A change starts as a numbered spec in `docs/prds/` with a
machine-valid `Expected Files` contract, and every PRD's parse is pinned by
[`prd_fixtures.rs`](crates/familiar-ai-review/tests/prd_fixtures.rs) — so adding
or editing a PRD requires updating its pin.

---

## Roadmap

**Next:** enforceable execution budgets (PRD-024, implemented and awaiting
review), then the planner — design docs to a dependency-ordered PRD batch with a
single human approval gate. That is the highest-leverage unbuilt organ;
decomposition quality determines everything downstream.

**After that**, parallel worktree execution, and the economy track: reconnecting
wave-one memory to wave-two execution, computing the north-star metrics, and
task-class routing built on observed data rather than opinions about models —
because opinions about models rot in weeks.

---

## Documentation

| | |
|---|---|
| [`docs/north-star.md`](docs/north-star.md) | Mission, multi-model doctrine, critical path |
| [`docs/philosophy.md`](docs/philosophy.md) | Principles and engineering invariants |
| [`docs/architecture/`](docs/architecture/) | Current state, target state, gap analysis, subsystems |
| [`docs/adr/`](docs/adr/) | Decision records: state semantics, human identity, execution warrants |
| [`docs/contracts/`](docs/contracts/) | Command, event, and query models |
| [`docs/prds/`](docs/prds/) | Every specification, shipped and queued |

---

## License

No license file is present yet, so default copyright applies — all rights
reserved. Open an issue if you need this clarified.

---

*Keep the daemons replaceable. Keep the grimoire. Keep score.*
