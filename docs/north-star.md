# Familiar: North Star

> A witch's familiar does not cast the spells. It keeps the grimoire, prepares
> the circle, watches the summonings, and remembers everything — while the
> daemons come and go.
>
> *The Familiar remains. The summoned daemons simply become smarter.*

**Date:** 2026-08-03
**Status:** Mission definition. Supersedes the goals and non-goals of
`docs/prds/vision.md` (the wave-one "memory sidecar" vision, retained as
historical record). `docs/philosophy.md` and
`docs/architecture/target-state.md` remain authoritative for principles and
architecture. Amending or retiring `vision.md` itself is an open human
decision.

---

## Mission

Load your coding agents — Claude Code, Codex, OpenCode, Cursor, whatever
exists this month. Familiar loads with them. Write a set of design documents.
Say **"have at it."** Leave.

While you are gone, Familiar:

1. drafts a dependency-ordered batch of bounded PRDs from your design docs;
2. waits for the one human gate — you review the *decomposition*, not the
   code, attach a budget ceiling, and approve the batch (~20 minutes);
3. executes the backlog unattended: compiled context, isolated worktrees,
   the right model for each stage, deterministic verification, independent
   adversarial review by a different model than the implementer;
4. writes a morning report.

You wake up to an MVP — or to a partial MVP plus a precise, auditable account
of exactly what stopped, why, and what it cost. Both are the system working.

The epigraph is a design constraint, not a slogan: something better than
today's best model may launch tomorrow. It must not matter. Familiar adapts to
whatever is out there and strategically routes the best available worker to
each class of work. Frontier models are for frontier problems. Nobody needs an
archdemon to sort a CSV — and mostly, a CSV needs `sort`, not a model at all.

## The Two Goals

1. **Cheap** — intelligent multi-step, multi-model orchestration.
2. **Autonomous** — app-building autopilot; it does not bother the human.

These are one war against a single enemy: **wasted work**. Every dollar an
agent system burns is one of three fires — rediscovering context it already
had, retry-looping on a bad approach, or confidently building the wrong thing.
Those same three fires generate the interruptions. Kill the waste and the
system becomes cheap and quiet in the same stroke:

- memory and compiled context attack rediscovery;
- deterministic verification catches failure while it is still cheap;
- PRD gates stop the wrong-thing fire before the tokens are spent.

Autonomy does not mean the system never needs a human. It means the system
**saves up** its need for humans and spends it at chosen gates: fail closed,
but fail cheap and fail informative. A retained `in_progress` entry with an
exact reason at 3 a.m. is success; a $40 hallucinated feature is failure.

## North-Star Metrics

Both are computable from state Familiar already records (execution history,
backlog events, PRD-012 recovery audit):

| Metric | Direction | Meaning |
|---|---|---|
| **Cost per accepted PRD** | down | Tokens/dollars per unit of merged, review-passed work — not raw spend. |
| **Human touches per accepted PRD** | toward 1 | Every recovery command, override, and clarifying question counts. The floor is 1: the batch approval. |

Scope filter: a proposed change to Familiar that moves neither metric is
off-mission — a sharper creep test than any principles document.

## Multi-Model Doctrine

- **Dispatch table, not relay.** A serial pipeline (cheap model → analyst
  model → distiller → builder) is a game of telephone: each hop is lossy
  compression across a trust boundary, and errors compound. Familiar composes
  staged workflows where each stage's model is chosen by policy — and every
  distilled artifact carries provenance with fallback to authoritative source,
  so the final worker can drill past a summary that smells wrong.
- **Stage zero: no model at all.** The first routing decision is whether the
  task needs inference. Parsers, git, grep, compilers, and test suites answer
  first (Determinism Before Intelligence). The second-best choice is the
  cheapest model that survives review.
- **Routing is deterministic policy.** Config-declared, versioned,
  inspectable capability tables — never an LLM choosing which LLM to call.
  Every model invocation has an explicit purpose, bounded input, selected
  provider policy, and observable result.
- **Multi-vendor pays at review time.** Different models are valuable because
  they fail differently, and that pays when outputs are *compared*, not
  concatenated: one vendor implements, a different vendor reviews
  adversarially, deterministic systems verify, the human approves.
- **Model probation.** Familiar holds no opinions about models — opinions rot
  in weeks. New models earn routing trust the way implementations do: a
  probation warrant of bounded low-risk PRDs, reviewed by an incumbent,
  promoted or demoted on observed review-pass rate and cost per accepted PRD
  from execution history. Trust Is Earned Through Verification applies to the
  workers themselves. The proving ground — backlog, review gate, accounting —
  is the durable asset; the scores update as the daemons churn.
- **Subscription arbitrage.** Driving vendor CLIs as subprocesses (claude,
  codex, opencode) rides subscriptions already paid for rather than metered
  API keys. "Which subscription has headroom" is a routing input alongside
  "which model is cheapest."

## Current State (2026-08-03)

### Built and working

- Backlog lifecycle: discovery, dependency graph, claim, fail-closed
  completion, explicit audited recovery (PRD-009, PRD-011, PRD-012).
- Agent-neutral execution: `CodingAgent` trait, Codex adapter, streaming,
  honest usage accounting where unknown stays unknown — never zero.
- Isolated adversarial review with verification, evidence budgets, retry and
  remediation ceilings (PRD-008) — isolation currently macOS-only.
- Context compilation with hard token ceilings and byte-pinned prompts.
- Wave-one memory surface over MCP: ten tools (status, pack_for_task,
  summaries, decisions, rollups, search) in `crates/familiar-mcp`.

### Specified, pending execution

- **PRD-013** — deterministic review scope authorization (Expected Files
  contract).
- **PRD-014** — Claude Code agent adapter with independent
  implementation/reviewer selection via `[agents]` config.

### Known gaps and debt

- **Linux review isolation missing:** `denied_read_path` enforcement is
  macOS `sandbox-exec` only; on Linux it fails closed — blocking isolated
  review on the primary dev machine. Needs Landlock or bubblewrap.
- **MCP knows nothing about wave two:** no backlog, execution-history,
  review-finding, or handoff tools; the most valuable state Familiar holds is
  CLI-only.
- **Wave-one crate debt:** roughly 9–10k lines (mcp, llm, tray, summary,
  watcher, dashboard) predate the current execution architecture; dormant vs.
  dead is unaudited.
- **Inference defaults to Disabled**, so summary/rollup memory may be
  unpopulated on fresh installs.
- **PRD metadata is parsed prose** (line-heuristic acceptance criteria);
  structured, machine-checkable PRD front matter would align the spec format
  with the project's own determinism principle.

## Critical Path to "Have At It"

Roughly 8–12 PRDs in three stages. Familiar increasingly builds itself, so
cadence should compound.

### Stage 1 — Supervised overnight loop (~4–6 weeks part-time)

Human writes the PRDs; Familiar executes them unattended overnight.

1. **PRD-014** — Claude Code adapter (spec complete, ready to run).
2. **Linux review isolation** — Landlock/bubblewrap denied-read enforcement.
3. **Backlog driver loop** — run until backlog empty, budget exhausted, or
   nothing eligible; global overnight budget warrant.
4. **Morning report** — one screen: what got built, what stopped and exactly
   why, what it cost, what needs human judgment.

### Stage 2 — Full autopilot (~2–4 months cumulative)

5. **Planner** — design docs + repository intelligence → dependency-ordered
   PRD batch → batch human approval gate. The highest-leverage unbuilt organ;
   decomposition quality determines everything downstream.
6. **Parallel worktree execution** — independent dependency branches continue
   past a failure instead of one stuck PRD stalling the night.

### Stage 3 — Hardening tail (~1 month)

Unattended failure-mode burn-in before genuine trust. Every autopilot pays
this tax; fail-closed design means paying it in stranded runs and precise
handoffs, not burned tokens.

Supporting work, high value but off the critical path: read-only MCP exposure
of wave-two state; wave-one crate audit; structured PRD front matter;
empirical model routing atop probation data.

## Feasibility

**Possible.** Nothing on the critical path requires a research breakthrough;
it is composition of parts that already run. Existence proofs: `familiar run`
executes PRDs end-to-end with claim, verification, review, and honest
accounting today; the planner stage has been performed manually (design
requirements in, mergeable PRD-014 out) and needs productizing, not
inventing.

The two real risks, both gated rather than open:

1. **Planner decomposition quality** — mushy PRDs scale into confident
   garbage. Gated by the batch approval: the human reviews the decomposition
   before any execution spend.
2. **Unattended reliability** — the long tail of 3 a.m. failure modes. Gated
   by fail-closed completion, budget ceilings, retry limits, and audited
   recovery; the cost of failure is a stranded entry, not a burned allotment.

---

*Keep the daemons replaceable. Keep the grimoire. Keep score.*
