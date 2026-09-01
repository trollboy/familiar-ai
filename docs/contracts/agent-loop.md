# Familiar Raw-Model Agent Loop Contract

**Status:** Normative
**Date:** 2026-09-01

This document defines the contract for Familiar's own agent loop against
raw inference endpoints (PRD-058): how a prompt is composed, how a model's
tool calls are validated, authorized, journaled, and executed, how the loop
terminates, and how evidence and usage are recorded. It governs every
**Familiar raw runtime** and **local raw runtime** worker (PRD-057); it does
not describe external harnesses (Claude Code, Codex), which own their own
loop, tools, and permissions.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD
NOT**, and **MAY** are to be interpreted as described by RFC 2119.

## Goals

- One deterministic state machine, identical across every adapter.
- A canonical, closed tool capability vocabulary that MCP, native provider
  tool calling, and schema-constrained templates all project from — never
  define independently.
- Every tool call validated against its schema, then authorized against
  authority scope and side-effect class, before it can have any effect.
- A write-ahead journal so a resumed loop never blindly repeats a
  non-idempotent or destructive call.
- Honest, closed stop reasons: a failed stage is its own failure, never
  relabeled as something else.
- Usage that lands in the PRD-051 ledger with distinct token categories and
  full PRD-057 spec identity, and evidence that never carries a prompt, a
  response, source code, or raw tool output.

## Non-goals

- Real hosted provider wire adapters (Anthropic/OpenAI/xAI/local-runtime
  HTTP clients). This contract defines the adapter trait every such client
  implements; shipping one is configuration plus adapter work under the
  PRD-057 contract, not a change to this document.
- Execution-lifecycle orchestration, worker routing, or backlog claiming.
  Those are PRD-056/044/057 concerns; this loop is what a worker *does*
  once selected.
- A general JSON Schema validator. The canonical capability set is small
  and closed; validation is closed field-presence checking against each
  capability's declared schema.

## The state machine

    compose -> submit -> streaming turn -> {text | tool calls |
    structured output | stop} -> validate -> authorize -> execute
    (journaled) -> insert results -> iterate | terminal

Each iteration:

1. **Compose.** The prompt is a byte-stable prefix (repository/config
   context, PRD-029) followed by volatile task state. A volatile-only
   change MUST NOT perturb the stable prefix's bytes.
2. **Submit.** One [inference attempt](#inference-adapter-contract) is
   sent: messages, offered tool definitions, model, cache controls, and an
   optional structured-output request.
3. **Streaming turn.** Text deltas, tool-call deltas, and a final stop
   reason arrive through the adapter's stream. Usage for the attempt is the
   adapter's final reported total, never derived by summing deltas.
4. **Validate.** Every requested tool call is checked against the canonical
   capability schema before anything else happens. An unknown capability, a
   call whose arguments did not parse, and an oversized payload are
   refusals — recorded, never executed.
5. **Authorize.** A validated call is evaluated against the execution's
   granted capability set, the capability's side-effect class, and — for a
   write — the [deterministic scope authorization contract](#write-scope-authorization).
   A refusal carries a continuation: inform the model and continue, or stop
   closed.
6. **Execute (journaled).** See [Write-ahead tool journal](#write-ahead-tool-journal).
7. **Insert results.** The tool's result is appended to the conversation as
   untrusted data (a `ToolResult` message), never as an instruction.
8. **Iterate.** Ceilings (iteration count, output tokens, wall-clock) are
   checked preemptively before every submission, not only after one is
   exceeded.

## Canonical tool capabilities

The initial closed vocabulary (`familiar_ai_agent::raw_runtime::CapabilityId`):

| Capability | Side-effect class | Notes |
|---|---|---|
| `read-file` | read-only | worktree-confined |
| `search-list` | read-only | worktree-confined |
| `run-command` | destructive | policy-gated: allowlisted commands only, network deny-by-default |
| `apply-edit` | idempotent-write | bounded by the write-scope authorization contract |
| `report-progress` | idempotent-write | acknowledged, journaled; grants nothing |
| `submit-evidence` | idempotent-write | acknowledged, journaled; grants nothing |
| `request-escalation` | idempotent-write | creates a pending human gate; grants nothing |

Each capability carries a stable identity, a schema version, its
input-schema field list, timeout, idempotency declaration, audit
requirement, and risk classification. A projection (MCP tool definition,
native provider tool definition, or a schema-constrained template for a
runtime without native tool calling) **MAY** narrow this set for a given
worker; it **MUST NOT** widen it or invent a capability outside this table.

## Inference adapter contract

`familiar_ai_llm::attempt::InferenceAdapter` is the minimum provider-neutral
surface a PRD-057 raw runtime implements: `submit(request, observer) ->
Result<SubmitOutcome, AdapterError>`, where `observer` receives streamed
text/tool-call/usage/stop events and `SubmitOutcome` carries the final stop
reason, usage categories, and provider request identity.

- **Every `submit` call is its own globally unique attempt** with its own
  budget reservation. A retry is a new `AttemptId` and a new reservation —
  never a free replay — unless the provider offers a documented idempotency
  guarantee, in which case the adapter records the provider's idempotency
  key and replay of the *same persisted provider event* is idempotent.
- **The error taxonomy is closed:** `Retryable` (rate-limited, overloaded,
  transient transport), `NonRetryable` (auth, invalid request, refused
  content), and `Ambiguous` (timeout with unknown completion). An ambiguous
  outcome records usage as unknown/pending for that attempt — never zero.
- Tool execution never crosses this boundary; only inference does. The loop
  never retries a tool call through the provider path.
- Provider-specific request/response fields stay inside adapter-owned
  types. The loop and this contract see only `SubmitRequest`/`SubmitOutcome`.
  Adding an adapter is configuration plus an `InferenceAdapter`
  implementation; it changes no loop, authority, or persistence semantics.

## Write-ahead tool journal

Before a tool call executes, its **intent** (call id, capability, argument
hash, side-effect class) MUST be durably recorded. The **result**
(succeeded with a content hash, or failed with a detail) is recorded after
execution. Resume never replays blindly:

| Journal state | Side-effect class | Resume decision |
|---|---|---|
| result recorded | any | never re-run (`AlreadyDone`) |
| intent only, no result | read-only | may re-run (`ReplayAllowed`) |
| intent only, no result | idempotent-write | may re-run (`ReplayAllowed`) |
| intent only, no result | destructive | fails closed to a human gate (`FailClosed`) |

`familiar_ai_agent::raw_runtime::resume_decision_for` is this exact
function. A host's resume path MUST consult it (via
`familiar_ai_daemon::agent_runtime::resume_readiness` for the SQLite-backed
journal) for every pending intent before resuming the loop; a single
destructive intent without a result blocks resume until an operator
resolves it.

Journal entries key on the model-issued call id within one execution. A
provider-level retry that produces the *same* tool call is not a new
journal entry merely because the inference attempt differs — the journal
records the tool call's own identity, not the inference attempt's.

## Write-scope authorization

A write (`apply-edit`) is authorized only when its target path matches a
declared entry, evaluated **before** the executor ever runs.
`familiar_ai_daemon::agent_runtime::write_scope_authorizer_from_prd` derives
this directly from the active PRD's own PRD-013 `## Expected Files` grammar
(`familiar_ai_review::parse_expected_files`) — the same exact-file /
directory / `directory/**` normalization, not a second heuristic. A path
outside the declared contract is refused with `OutOfWriteScope` and the
executor is never invoked.

`run-command` is gated the same way: a command is authorized only when its
`argv[0]` is in the execution's configured allowlist
(`agent_runtime.sandbox.allowed_commands`); an unlisted command is refused
before any process launches.

## Authority and sandboxing

- A raw model receives no ambient authority. Every tool call is
  project/execution-scoped (`AuthorityContext`).
- `run-command` executes with an environment built from an explicit
  allowlist only (`agent_runtime.sandbox.allowed_environment`) — the host
  process's environment is never inherited. Configuration validation
  (`AgentRuntimeConfig::validate`) rejects any allowlisted name that
  contains a billing/admin credential marker.
- Filesystem capabilities are confined beneath the execution's worktree
  root; a path containing `..` or an absolute path is refused.
- Network access for tool commands is deny-by-default
  (`agent_runtime.sandbox.network_allowed`, default `false`).
- Cancellation and timeout kill the tool's process group (reusing the
  Landlock/`sandbox-exec`/process-group watchdog already used for harness
  isolation, `familiar_ai_agent::{spawn_watchdog, finish_watchdog}`), not
  just the direct child.
- `request-escalation` creates a pending human gate. No tool result and no
  model output can grant a capability, raise authority, or approve
  anything — the executor for every capability only ever acknowledges or
  executes; it never mutates the authorizer's decision. Repository content
  and tool results are always inserted as `ToolResult` data, never as
  directives the loop interprets.

## Stop reasons

The closed, honest set (`familiar_ai_agent::raw_runtime::StopReason`):

`Completed { structured_output }`, `IterationCeiling`,
`TokenOrContextCeiling`, `BudgetStop`, `Timeout`, `Cancelled`,
`ProviderFailure { taxonomy }`, `FatalToolRefusal`,
`InvalidStructuredOutput`.

A stage that failed for its own reason is recorded as that reason — a
provider error is never relabeled `TokenOrContextCeiling`, and a fatal tool
refusal is never recorded as `Completed`.

## Evidence and accounting

**Execution evidence** (`agent_runtime_evidence`, `agent_runtime_tool_intents`,
`agent_runtime_tool_results`, migration 055) reconstructs deterministically:
prompt-template version, worker spec identity and empirical version, the
offered tool set and schema versions, every requested call's disposition
(validated/refused/authorized/executed), a content hash of each result, the
stop reason, and the resume point. It **MAY** reference transcripts under
the event-model's retention rules but is a separate store from accounting.

**PRD-051 usage** lands through `familiar_ai_daemon::agent_runtime::persist_run_outcome`,
which calls the existing `AccountingRepository::append_observation` for
every attempt — the same ledger every other execution uses, with the same
`UsageObservation` sanitized envelope. An accounting row **MUST NOT** ever
carry a prompt, a response, source code, or tool output; it carries token
categories, an attempt id, a source-event hash, and identity fields only.
An attempt with entirely unknown usage records an explicit
`unknown_reason`, never a fabricated zero.

## Test harness

Every test in `crates/familiar-ai-agent/tests/raw_runtime.rs` and
`crates/familiar-ai-daemon/tests/raw_runtime.rs` runs exclusively against
`familiar_ai_llm::attempt::FakeInferenceAdapter`, a deterministic script of
turns, tool-call sequences, usage categories, stop reasons, and the
retryable/non-retryable/ambiguous error taxonomy. No test in either file
performs, or is able to perform, a live or billable call.
