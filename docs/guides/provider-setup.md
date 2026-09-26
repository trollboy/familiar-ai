# Operator Guide: Registering Providers and Models

Practical walkthrough for putting real workers behind Familiar on one
machine — hosted APIs, optional subscription CLIs (Codex, Claude Code), local Ollama, and
authenticated OpenAI-compatible servers (Unsloth). Written 2026-08-31
against the current CLI; each step notes its known caveats from
`docs/running_bugs.md` so you hit them knowingly instead of by surprise.

## The mental model: five separate states

No vendor CLI is mandatory. A usable installation needs one reachable worker:
a provider key (`OPENAI_API_KEY` or `ANTHROPIC_API_KEY`), a local endpoint, or
a CLI. With no explicit worker declaration Familiar selects an available path
and reports every remedy when it finds none.

A model is useful only when all five hold. `provider list` / `model list`
show fragments of this today (FAM-BUG-001; a unified inventory command is
PRD-057 work):

1. **Installed** — the CLI or server exists on this machine.
2. **Registered** — `config provider add` persisted and probed it.
3. **Enabled** — `config model enable` created a registry worker with
   declared capabilities.
4. **Authenticated** — the auth descriptor resolves *in the context that
   will run it* (your shell and a launchd daemon are different contexts).
5. **Routable** — an adapter/runtime can actually execute it. Registered
   and enabled do NOT imply this (FAM-FRICTION-001: Unsloth models cannot
   execute until the local raw runtime, PRD-058/063, lands).

## Subscription CLIs (Codex, Claude Code)

```sh
familiar-ai config provider add codex  --kind inference --auth "cli-login: codex"
familiar-ai config provider add claude --kind inference --auth "cli-login: claude"
familiar-ai config provider verify claude
familiar-ai config model enable codex/codex --capabilities implementation,review,remediation
```

Caveats:

- **Claude auth is parsed, not trusted**: admission requires an explicit
  `"loggedIn": true` from `claude auth status` (FAM-BUG-004 fix). If you
  re-auth, run `config provider verify claude` again before enabling.
- **CLI "model" identities are synthetic** (FAM-FRICTION-002 /
  FAM-BUG-009): the probe records the command label (`codex`, `claude`),
  not a real selectable model. Enabling `claude/claude` produced a worker
  whose `--model claude` the CLI rejected, burning an entire wave's
  attempts. Until PRD-057 lands honest identity: after enabling a
  CLI-backed worker, run one cheap `familiar-ai run` against a trivial
  PRD before trusting it in a warranted drive session.

## Local Ollama

```sh
familiar-ai config provider add ollama --kind inference --host http://127.0.0.1:11434
familiar-ai config model enable ollama/<model> --capabilities review,narrow-task
```

Caveats:

- Discovery uses `/api/tags` through the standard HTTP client (chunked
  responses fine since FAM-BUG-005's fix).
- **Codex-driven review requires Ollama ≥ 0.13.4** (FAM-BUG-013): with an
  older Ollama the reviewer fails every retry as "malformed output."
  Upgrade Ollama before routing review to it.
- Capabilities you assert here are **declared, not verified**
  (FAM-FRICTION-003) — start conservative (`review`, `narrow-task`);
  promotion on evidence is PRD-032/057 work.

## Unsloth (authenticated OpenAI-compatible server)

```sh
export UNSLOTH_API_KEY=...   # see the Keychain caveat below
familiar-ai config provider add unsloth-local --kind unsloth \
    --host http://127.0.0.1:8000 --auth "env: UNSLOTH_API_KEY"
familiar-ai config provider verify unsloth-local
```

`--kind unsloth` selects the typed runtime: authenticated `/v1/models`
discovery with nested model IDs (FAM-BUG-002 fix). Caveats:

- **Registered ≠ executable.** Verification and discovery succeed today;
  the model cannot run PRD work until the neutral local runtime
  (PRD-058/063) lands. `provider list` showing it healthy is honest about
  the endpoint, silent about executability (FAM-FRICTION-001).
- **Auth must be `env: NAME`** for Unsloth. A macOS Keychain-stored
  credential needs a bridge into the environment of *whatever runs
  Familiar* — your shell export does not reach a launchd daemon
  (FAM-BUG-003). Native use-time Keychain resolution is PRD-074.

## Routing reality check

- Without `worker_registry.routing.ladder`, routing retains the historical
  configured rules, pins, and lowest-cost/id tiebreak. Enable cheap-first
  routing with that single ladder table and list each job class's workers in
  cheapest-first order. Every worker still needs a PRD-086 cost basis.
- `risk_floors` prevents a declared risk class from using rungs below the
  named worker. `maximum_cheap_fraction_basis_points` skips a cheap rung when
  it costs too large a fraction of the next rung, avoiding an unprofitable
  extra attempt.
- A failed cheap attempt escalates once with its verification or review
  evidence and still consumes its cost/token reservation. Failure on the next
  rung stops for a human decision; Familiar does not climb indefinitely.
- Local rungs may start in probation. Promotion and demotion use recorded
  review-pass, remediation, failure, and known cost-per-accepted-PRD evidence,
  within the configured low-risk and expected-file bounds. Unknown cost never
  promotes a worker.
- `familiar-ai report` prints the durable rung path, active ladder rule,
  escalation reason, and unknown-safe cost per accepted PRD. No ladder section
  is printed for sessions that did not use one.
- The first `model enable` against a legacy `[agents]` configuration is
  refused before writing (FAM-BUG-006 guard). The audited lossless
  migration command is PRD-075.
- Capability spellings in `model list` are canonical and paste-safe
  (`narrow-task`, FAM-FRICTION-004 fix).

## When something looks wrong

- `config provider verify <name>` re-probes and updates decision rows.
- `familiar-ai report` shows the latest session, warrant, and attempts.
- `familiar-ai resume all --dry-run` shows recovery state; completed and
  integrated PRDs are suppressed from it (FAM-BUG-016 fix) — if one still
  appears, that is a bug; log it.
- New defects go in `docs/running_bugs.md` — bugs outrank all planned
  work (execution-plan policy, 2026-08-31).
