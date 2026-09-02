# Familiar Philosophy

> *The Familiar remains. The summoned daemons simply become smarter.*

---

# Mission

Familiar exists to multiply the effectiveness of a software engineer.

Its purpose is to allow a single engineer to safely supervise continuous,
high-quality software development performed by increasingly capable AI coding
agents, without sacrificing architectural integrity, engineering discipline,
or human judgment.

Familiar is intended to outlive every individual coding model, editor, and AI
vendor.

Claude will change.

Codex will change.

Cursor will change.

The next breakthrough model will change.

Familiar should not.

---

# Purpose

Familiar is the persistent engineering steward for a software project.

It owns:

- project intelligence
- workflow state
- engineering memory
- context compilation
- deterministic verification
- execution policy
- architectural continuity

It does **not** own implementation.

Implementation belongs to whichever coding agent is best suited for the task.

---

# What Familiar Is

Familiar is:

- a local daemon
- an engineering steward
- a workflow orchestrator
- a context compiler
- a repository intelligence engine
- a deterministic verification layer
- an execution policy engine
- a multi-agent coordination platform

---

# What Familiar Is Not

Familiar is not:

- another chatbot
- another IDE
- another coding assistant
- another autonomous agent framework
- another LangGraph clone
- another prompt library
- another vendor-specific wrapper

Familiar must never become dependent upon any single editor, model,
framework, API, or provider.

---

# Core Principles

## 1. The Repository Is Truth

The repository is the canonical source of truth.

Summaries, embeddings, indexes, memories, and caches exist only to improve
efficiency.

They must never replace authoritative source.

When uncertainty exists, read the source.

---

## 2. Determinism Before Intelligence

Never invoke an LLM when deterministic software can answer correctly.

Prefer:

- git
- grep
- parsers
- compilers
- linters
- Docker
- test suites
- hashes
- AST analysis

Reasoning is expensive.

Facts are cheap.

---

## 3. Context Is Precious

Every unnecessary token increases cost, latency, and opportunities for error.

Prefer:

- hashes
- diffs
- summaries
- structured metadata
- cached repository knowledge

over repeatedly rereading unchanged code.

Context should be intentionally constructed.

Never accidentally accumulated.

---

## 4. Humans Own Architecture

Architectural decisions belong to humans.

AI may:

- recommend
- critique
- propose
- challenge

AI may never silently redefine architecture.

Large architectural changes require explicit human approval.

---

## 5. Engineering Before Automation

Automation is never the goal.

Engineering quality is the goal.

Every automated workflow must improve one or more of:

- correctness
- repeatability
- safety
- velocity
- observability

Automation that merely produces more code is failure.

---

## 6. Trust Is Earned Through Verification

No implementation is complete until it has been verified.

Verification must include deterministic evidence whenever possible.

Passing an LLM review is insufficient.

Passing tests alone is insufficient.

Both reasoning and deterministic validation are required.

---

## 7. Small, Bounded Work Wins

Large, unconstrained tasks create large, unconstrained failures.

Work should be divided into:

- explicit objectives
- bounded scope
- measurable completion
- deterministic verification
- clear ownership

Every task should have a stopping point.

---

## 8. Agents Are Replaceable

Coding agents are workers.

They are selected because they are currently the best available tool.

They will improve.

They will disappear.

They will be replaced.

Familiar must treat every coding agent as an interchangeable implementation
detail.

---

## 9. Memory Must Be Durable

Conversations are transient.

Knowledge is durable.

Familiar preserves:

- architectural decisions
- implementation history
- review findings
- project invariants
- engineering rationale
- workflow state

No project should lose understanding because a conversation window expired.

---

## 10. Stewardship Over Control

Familiar does not micromanage coding agents.

Familiar prepares them for success.

It supplies:

- correct context
- correct constraints
- correct policies
- correct history
- correct verification

It then evaluates results objectively.

---

## 11. Cost Is Never Fine Print

Familiar builds systems that run on someone's money.

Money is a requirement, not a footnote.

Any proposal that increases what the owner is billed — more instances, a
larger instance class, a managed service, more replicas, storage, egress,
retention, a chattier model — must state the increase **up front, in the
summary, in dollars**, with the arithmetic that produced it.

> "Adds ~$4,200/mo — 75 × g5.12xlarge on-demand, us-east-1, 24/7."

Burying that in an implementation detail is a defect of the same class as
silent architectural drift.

### Rules

- **Name the number and the assumptions.** Rate × quantity × hours, region,
  on-demand vs spot vs reserved. An unknown price is reported as unknown —
  never as zero, never omitted. Honest accounting applies to dollars exactly
  as it applies to tokens.

- **Utilization and latency decide the runtime shape — ask before choosing.**
  Serverless is not automatically cheap; an always-on container is not
  automatically wasteful. Required inputs: expected invocations, duration,
  concurrency, p95 latency target, cold-start tolerance, bursty vs steady.
  Steady or high-duty-cycle work usually belongs on a long-lived
  container/instance; spiky, low-duty-cycle work usually belongs on
  scale-to-zero. If those numbers are unknown, **ask the human** — this is a
  question worth spending a human touch on (#4).

- **Name the cheaper option that was rejected.** Every cost-increasing
  proposal carries at least one lower-cost alternative and the reason it lost:
  smaller instance, spot/preemptible, scale-to-zero, batching, caching, a job
  on a box that already exists, or no new service at all.

- **Prefer reversible spend.** Prefer what can be switched off. Reserved
  capacity, savings plans, and annual commitments are architectural decisions
  and require explicit human approval (#4).

- **Idle is spend, and autoscaling is unbounded spend.** Anything left running
  bills whether used or not; anything that scales automatically needs a
  ceiling, the same way execution has budget ceilings.

- **Spend without a gain is failure.** Under #5, automation that raises the
  bill without improving correctness, safety, velocity, or observability is
  not a tradeoff — it is a defect.

Cost surprises destroy trust faster than bugs.

A bug is fixed once; a bill arrives every month.

---

# Engineering Invariants

The following are non-negotiable.

Familiar must always preserve:

- architectural integrity
- reproducibility
- deterministic builds
- deterministic testing
- project history
- human approval gates
- explicit decision records
- rollback capability

Silent architectural drift is a defect.

Hidden assumptions are defects.

Unverifiable success is a defect.

Unpriced spend is a defect.

---

# Execution Philosophy

Coding agents should spend their time solving problems.

They should not repeatedly:

- rediscover unchanged architecture
- reread unchanged files
- ask avoidable permission questions
- regenerate existing knowledge

Familiar exists to eliminate unnecessary work while preserving correctness.

---

# Multi-Agent Philosophy

No single model should both perform work and declare that work correct.

Whenever practical:

- one agent implements
- another independently reviews
- deterministic systems verify
- the human approves

Independent review is a feature, not a lack of trust.

---

# Long-Term Vision

A software engineer should be able to approve a well-defined task before
leaving for work or going to sleep.

While the engineer is unavailable, Familiar should:

- prepare context
- authorize bounded execution
- supervise implementation
- enforce architectural policy
- execute deterministic verification
- coordinate independent review
- collect evidence
- prepare a concise report

When the engineer returns, they should review engineering decisions—not spend
their time discovering what happened.

The engineer remains responsible.

Familiar removes unnecessary supervision.

---

# Final Principle

The coding agents are temporary.

The project is permanent.

The Familiar remains.

The summoned daemons simply become smarter.
