# Anthropic Raw-API Adapter Contract

**Status:** Normative
**Date:** 2026-09-01

Part of the [provider configuration contract](providers-index.md). Covers
runtime `anthropic-api` (PRD-059): the first [PRD-058 inference
adapter](agent-loop.md#inference-adapter-contract) implementation, wiring
Familiar's own raw-model agent loop directly to the Anthropic Messages API.
This document is normative for how that wire mapping behaves; it does not
restate the agent-loop's own contract (tool capability vocabulary, write-ahead
journal, stop-reason closed set, accounting shape) — see
[agent-loop.md](agent-loop.md) for that.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD
NOT**, and **MAY** are to be interpreted as described by RFC 2119.

## Scope and non-goals

This adapter changes no loop, routing, authority, or persistence semantics —
it is configuration plus an `InferenceAdapter` implementation, exactly as
[agent-loop.md](agent-loop.md#inference-adapter-contract) requires of every
such client. It does not assume Claude Code semantics (no client-side cost
estimate, no subscription entitlement, no harness permission model): an
`anthropic-api` worker is a distinct [PRD-057](../prds/done/PRD-057.md)
identity from a Claude Code worker of the same nominal model, even when both
name `claude-sonnet-5`.

## Module layout

| Module | Owns |
|---|---|
| `familiar_ai_llm::anthropic_api` | Wire-level HTTP client: request/response JSON shapes, SSE frame parsing, the closed error taxonomy classification, non-billable probes (token counting, model metadata). Knows nothing about Familiar's canonical tool capabilities or the raw-runtime loop. |
| `familiar_ai_agent::anthropic` | The actual `InferenceAdapter` (`familiar_ai_llm::attempt::InferenceAdapter`) implementation. Projects `SubmitRequest`/`SubmitOutcome` onto the wire client; owns the tool-definition projection, message-history reconstruction, cache-control placement, and stop-reason mapping. |

## Tool definitions, `tool_use`, and `tool_result`

Each offered `ToolDefinition.capability_id` (e.g. `read-file`) becomes an
Anthropic tool's `name` verbatim — Familiar's canonical capability ids are
already valid Anthropic tool names (`^[a-zA-Z0-9_-]{1,128}$`), so no
translation table is needed in either direction: a model's `tool_use.name`
maps back to `capability_id` unchanged.

`ToolDefinition.json_schema` is documented at the PRD-058 boundary as "a
serialized JSON Schema document," but the loop core's current tool
projection (`familiar_ai_agent::raw_runtime::offered_tool_definitions`)
actually emits a placeholder shape,
`{"required":[...],"optional":[...]}` — not a schema document. This adapter
handles both: when `json_schema` parses as an object already carrying
`type`/`properties`, it is passed through unchanged as the tool's
`input_schema`; otherwise it is expanded into a minimal permissive schema
(`{"type":"object","properties":{<name>:{}},"required":[...]}`) naming
every required/optional field with an unconstrained value type. This keeps
the wire request valid Messages API shape without requiring a change to the
shared loop core.

### The `tool_use` replay problem

The loop core's `Message`/`MessageContent` (`familiar_ai_llm::attempt`)
carry only `Text` and `ToolResult` content — there is no `ToolUse` variant,
because the loop never needs to *read back* a model's own tool call, only
record that it happened and feed the result to the next turn. The Anthropic
wire format, however, requires every `tool_result` block to be preceded in
history by the exact `tool_use` block it answers (same `id`, `name`,
`input`).

This adapter closes that gap with adapter-local state: as it streams a
response, every completed `tool_use` content block is remembered in an
in-memory `tool_use_registry` keyed by call id (`content_block_stop`'s
accumulated `input_json_delta` fragments, parsed once and cached). When a
later request's `messages` history reaches the matching `tool_result`
message(s), the adapter looks up the registry and reconstructs the
assistant's `tool_use` block(s) to precede them. The registry is
adapter-instance-scoped, not conversation-scoped — call ids are
provider-generated and effectively globally unique, so no cross-conversation
key collision is expected in practice.

Grouping: the loop pushes an `Assistant(Text)` message (if the model
produced any text) followed by zero or more contiguous `Tool(ToolResult)`
messages for that turn's calls. The adapter converts this into exactly two
wire messages when tool calls are present — one `assistant` message
carrying the text (if any) plus every reconstructed `tool_use` block, and
one `user` message carrying every `tool_result` block for that turn — "all
results for a parallel batch in one user message," per the Messages API
shape. A turn with no tool calls converts to a single `assistant` or `user`
text message as expected.

### Malformed or unknown tool calls

The adapter never validates or refuses a tool call itself — it forwards
exactly what streamed (capability name, accumulated argument fragments) and
lets the loop core's `TurnCollector`/`validate_tool_call`
([agent-loop.md](agent-loop.md)) do so. If the accumulated
`input_json_delta` fragments do not parse as JSON when the block completes,
`StreamEvent::ToolCallComplete` still fires (the call is not silently
dropped), but the adapter does not populate `tool_use_registry` for that
call id — an unrecoverable malformed call is never replayed either. The
loop's own validation refuses it before it ever executes.

## Streaming

Text, tool-call, and usage deltas surface to the loop and its observers as
they arrive via `StreamEvent::{TextDelta, ToolCallDelta, ToolCallComplete,
UsageDelta}`. The `content_block_start` for a `tool_use` block always
carries the empty-object placeholder `input: {}` on the real API — the
adapter does not treat it as fragment data (doing so would double-count
against the real content delivered via `input_json_delta`).

A partial or interrupted stream (dropped connection, no `stop_reason` ever
observed) is reported by `anthropic_api::stream_messages` as
`AdapterError::Ambiguous`, never a fabricated `end_turn` or a zeroed usage.
Everything the observer already received before the interruption stays
delivered — "preserves observed usage" applies to what reaches the
observer; the attempt itself is recorded ambiguous per the closed error
taxonomy. Familiar never assumes provider-side resumption of the
interrupted request: a caller that wants to continue mints a fresh
`AttemptId` and calls `submit` again, exactly as every other retry does
under the [inference adapter contract](agent-loop.md#inference-adapter-contract).

Usage reported to the loop (`SubmitOutcome.usage`) is the adapter's final,
authoritative total — the latest-known value per category across
`message_start`/`message_delta` events, never a sum of the streamed
`UsageDelta`s.

## Usage and cache categories

The usage block's categories map one-to-one onto
`familiar_ai_llm::attempt::UsageCategories`:

| Anthropic field | `UsageCategories` field |
|---|---|
| `input_tokens` | `uncached_input_tokens` |
| `cache_read_input_tokens` | `cache_read_tokens` |
| `cache_creation_input_tokens` | `cache_write_tokens` |
| `output_tokens` | `output_tokens` |
| — | `reasoning_output_tokens` (always `None`) |

Anthropic bills reasoning/thinking spend inside `output_tokens` and exposes
no separate reasoning category on the Messages API; this adapter never
fabricates a split the provider does not report. A field absent from the
provider's response stays `None`, never a zero.

## Prompt caching (PRD-029 stable-prefix strategy)

`SubmitRequest.cache_control == Ephemeral` places exactly one
`cache_control: {"type":"ephemeral"}` breakpoint, on the last block of the
top-level `system` array. The loop core's `compose_messages` always
produces exactly one system message (the stable prefix); Anthropic's render
order is `tools` → `system` → `messages`, so a single breakpoint at the end
of `system` caches the byte-stable prefix (tools plus system) as one unit,
with volatile task state and turn history following, uncached, after it —
matching the PRD-029 strategy without needing a second breakpoint on the
tool definitions. Cache effectiveness is measurable directly from the
recorded `cache_read_tokens`/`cache_write_tokens` categories above (PRD-051
ledger): a non-zero `cache_read_tokens` on a repeated attempt against an
unchanged stable prefix is the cache actually working; an unexpectedly zero
value with an unchanged prefix indicates a silent invalidator upstream.

## Stop reasons

Anthropic's wire `stop_reason` maps onto
`familiar_ai_llm::attempt::AdapterStopReason`:

| Wire `stop_reason` | `AdapterStopReason` |
|---|---|
| `end_turn` | `EndTurn` |
| `tool_use` | `ToolUse` |
| `max_tokens` | `MaxTokens` |
| `stop_sequence` | `StopSequence` |
| `pause_turn` | `PauseTurn` |
| `refusal` | `Refusal { category }` (from `stop_details.category` when the provider exposes one) |

`PauseTurn` and `Refusal` are additive extensions to the closed
`AdapterStopReason` vocabulary (`crates/familiar-ai-llm/src/attempt.rs`),
added by this PRD because the prior five-variant set (modeled generically,
not against Anthropic specifically) had no honest way to represent either:

- **`pause_turn` ("pause-class continuation")** occurs when a provider-side
  server-side tool loop (e.g. hosted web search) hits its own internal
  iteration cap without the model producing a final turn. This adapter
  never declares Anthropic's server-side tools, so `pause_turn` is not
  expected in ordinary operation, but the loop core
  (`familiar_ai_agent::raw_runtime::run_loop`) still handles it correctly
  when it occurs: with no tool calls pending, a `PauseTurn` stop causes the
  loop to `continue` — resubmitting as a fresh, independent attempt with
  unchanged history — rather than terminating with `Completed`,
  `TokenOrContextCeiling`, or the `InvalidStructuredOutput` a bare `ToolUse`
  stop with no actual tool calls would otherwise produce. This is exactly
  "Familiar resumes its own workflow state" rather than assuming
  provider-side resumption of the interrupted request.
- **`refusal`** is a safety-classifier decline, categorically different from
  `ContentFilter` (a generic, uncategorized content-filter stop already in
  the closed set) and from `MaxTokens` — the acceptance requirement is that
  a refusal is recorded as its own honest terminal reason, "never token
  exhaustion." The loop core maps `Refusal { .. }` onto
  `StopReason::ProviderFailure { taxonomy: NonRetryable }` (the existing
  loop-level closed vocabulary in `raw_runtime.rs` has no dedicated
  "refusal" terminal reason, and widening it further was judged out of
  scope for "adding an adapter changes no loop semantics" — see
  [Design notes](#design-notes) below). The refusal's category, when the
  provider exposes one, remains available on the adapter's own
  `SubmitOutcome.stop_reason` and via `AnthropicAdapter::attempt_metadata`
  for any caller that wants it, even though the loop-level `StopReason`
  itself does not carry a dedicated field for it.

## Thinking and effort

Thinking and effort are configured per capability profile at adapter
construction (`AnthropicAdapterConfig.{thinking_enabled, effort}`), not by
the loop core, which never sets `SubmitRequest.reasoning_control`. A
request's own `reasoning_control`, when present, overrides the adapter's
configured default for that one attempt (effort only; the loop core's
`ReasoningControl.budget_tokens` field is not used by this adapter — current
Claude models take adaptive thinking, not a fixed token budget).

Thinking blocks are replayed unchanged on the same model, per the provider's
replay rules — using the same adapter-local remembrance mechanism as
`tool_use` (above), since the loop core's message history has nowhere to
carry a `Thinking` content variant either. Any thinking block(s) streamed
immediately before a `tool_use` block are captured (text plus the opaque
signature, via `content_block_start`/`thinking_delta`/`signature_delta`) and
attached to that call's remembered entry; when the matching `tool_result`
is later reconstructed, the thinking block(s) are replayed immediately
ahead of the reconstructed `tool_use` block, matching production order
(thinking always precedes what it led to). A thinking block preceding only
final text (no tool call) needs no replay — that turn is terminal; no later
request ever reconstructs it. Reasoning spend is billed and recorded exactly
as the provider reports it, inside `output_tokens` — the adapter never
assumes a separately billed reasoning category.

## Model identity

The requested `SubmitRequest.model` (which may be an alias, e.g.
`claude-sonnet-5`) and the response's resolved model identity (from
`message_start.message.model`, which may name a dated snapshot) are both
recorded: the adapter exposes the resolved identity via
`AnthropicAdapter::attempt_metadata(&AttemptId)`, distinct from the
requested identifier, so a moving alias is never silently frozen into
canonical worker identity. `SubmitOutcome` itself (the PRD-058 shared
contract type) carries no resolved-model field — widening it would touch
every existing `SubmitOutcome` construction site across the PRD-058 test
suite for a fact only this adapter currently produces, so it is exposed as
a supplementary, adapter-owned API instead.

## Capability and limit probing

`AnthropicHttpClient::retrieve_model` (`GET /v1/models/{id}`) and
`AnthropicHttpClient::count_tokens` (`POST /v1/messages/count_tokens`) are
the two non-billable probe surfaces (PRD-047 discipline): no completion is
requested, no output is billed. A probe failure (network error, 4xx/5xx) is
propagated as an `Err`, leaving the capability or size estimate unknown —
callers must never default a probed fact to an assumed value on failure.

## Attempts, retries, and idempotent replay

Every `submit` call is its own globally unique attempt with its own budget
reservation, per the shared [inference adapter contract](agent-loop.md#inference-adapter-contract) — this adapter mints no
attempt ids itself and never retries internally. A timeout (the loop core's
`tokio::time::timeout` wrapping `submit`) or a stream that closes before a
stop reason arrives both record ambiguous usage for that attempt, never a
fabricated zero. Anthropic's `/v1/messages` documents no official
idempotency-key replay guarantee, so `SubmitOutcome.provider_idempotency_key`
is always `None` for this adapter — replay-safety for identical persisted
provider events comes from the accounting ledger's own idempotency key
(`AccountingRepository::append_observation`'s `source_event_hash`, derived
from `execution_id:attempt_id`), not from anything Anthropic-specific.
Separate billable attempts (distinct `AttemptId`s) always remain separate
observations.

Retryable errors carry `retry-after` when the provider supplies it (429
responses); the caller (budget/retry orchestration) is expected to respect
it within its own budget — this adapter does not sleep or retry on its own.

## Error taxonomy and authentication

HTTP status and, where available, the response body's `error.type` classify
into the closed `AdapterError` taxonomy:

| Condition | `AdapterError` |
|---|---|
| 429 | `Retryable(RateLimited { retry_after_ms })` |
| 529, or body `error.type == "overloaded_error"` | `Retryable(Overloaded)` |
| Other 5xx | `Retryable(TransientTransport)` |
| Connection failure | `Retryable(TransientTransport)` |
| 401/403 | `NonRetryable(Auth)` |
| 400/404/413/422 | `NonRetryable(InvalidRequest)` |
| Request timeout, or stream closed before a stop reason | `Ambiguous` |

### Credentials

The API key is an external BYO-Auth reference (`AuthDescriptor`,
`familiar_ai_core::config`) resolved through a `CredentialResolver` at call
time, immediately before a request is built — never cached beyond that
call, never written to configuration, prompts, tool definitions, accounting
rows, logs, or any subprocess environment (tool execution is an entirely
separate boundary; the key never crosses into it). The default resolver
(`EnvCredentialResolver`) handles `env: NAME` descriptors, matching how
Anthropic API keys are supplied in practice; any other descriptor kind (or
a missing/empty environment variable) fails closed with an exact remedy
(`anthropic_api::missing_env_remedy` /
`anthropic_api::unsupported_auth_remedy`), logged via `tracing::error!`, and
`submit` returns `NonRetryable(Auth)` — no request is ever sent. A host that
must resolve through a platform credential store (macOS Keychain, etc.)
supplies its own `CredentialResolver` (or a `StaticCredentialResolver`
wrapping an already-resolved value) via
`AnthropicAdapter::with_credential_resolver`; this crate does not depend on
any platform-specific store.

## Billing mode

Runtime `anthropic-api` with an API-key auth reference is always
`local-estimate` billing mode by construction — it is a `kind = "inference"`
provider entry, which the existing provider-configuration contract
(`crates/familiar-ai-core/src/config/providers.rs`) already forbids from
declaring `billing_mode`/`organization_id` (those are `kind = "billing"`
concerns, PRD-052's Admin-key organization collector). Local estimates are
produced from a versioned, dated, source-attributed price schedule
(`[execution_history.price_schedules."anthropic-api-<date>"]`,
`crates/familiar-ai-core/src/config/accounting.rs`'s existing
`PriceScheduleConfig`/`PriceScheduleRateConfig` — no new schema was needed;
see `config/default.toml` for a worked, dated example sourced from
`platform.claude.com/docs/en/pricing`), reconcilable against PRD-052
organization cost collection when an Admin-key billing source is configured
for the same organization. Nothing here assumes Claude Code's client-side
cost estimation, subscription entitlement, or harness permission model —
this is a distinct empirical identity by construction (PRD-057), never
mixed with Claude Code subscription usage of the same nominal model.

## Worker configuration

No `runtime_config` typed extension is required for `runtime =
"anthropic-api"` (unlike `ollama`, which needs one for its local host
address) — the generic `RegistryWorkerConfig` fields (`provider`, `model`,
`auth_profile`, `capability_profile`) are sufficient. A default capability
profile — `familiar_ai_core::config::anthropic_api_default_capability_profile`
— declares the capabilities this document establishes (native tool calling,
MCP client, structured output, streaming, prompt caching, reasoning
controls, parallel tool calls, usage/cost reporting categories,
remote-or-local, max-context) with `Declared` provenance; an operator may
still author a narrower profile, and probed/observed provenance layers on
top as the worker actually runs. See `config/default.toml` for a complete,
commented worked example (`[providers.anthropic]`,
`[worker_registry.workers.claude-api]`,
`[worker_registry.capability_profiles.anthropic-api-default]`).

## Design notes

Two extension points beyond a pure "drop in a new `InferenceAdapter`" were
required to make this adapter honest, and are recorded here so a future
adapter does not need to re-derive them:

1. **`AdapterStopReason` gained `PauseTurn` and `Refusal { category }`**
   (additive; every existing variant and match arm elsewhere in the
   workspace was unaffected — the sole exhaustive match site,
   `raw_runtime::run_loop`, was updated to handle both honestly). Without
   these, a pause could only be mapped onto an existing variant that
   produces a *wrong* loop-level stop (a fabricated ceiling, an invalid
   structured-output refusal, or a false "completed"), and a refusal could
   only be indistinguishable from a generic content-filter stop with no way
   to preserve its category even at the adapter boundary.
2. **`AttemptUsage` (`raw_runtime.rs`) gained `provider_request_id`**,
   threaded through `familiar_ai_daemon::agent_runtime::persist_run_outcome`
   into `UsageObservation.provider_request_id` — previously this field was
   unconditionally `None` for every adapter, because `SubmitOutcome`'s
   `provider_request_id` was captured nowhere in the loop's per-attempt
   bookkeeping. This is generic plumbing (not Anthropic-specific) that any
   future adapter benefits from.

Deliberately **not** extended: `SubmitOutcome` was not widened with a
resolved-model field, because doing so would require editing every one of
the 17 existing `SubmitOutcome { .. }` construction sites across
`crates/familiar-ai-agent/tests/raw_runtime.rs` and
`crates/familiar-ai-daemon/tests/raw_runtime.rs` — PRD-058's own fixture
suite — for a fact only this adapter currently needs. That fact is exposed
instead as a supplementary, adapter-owned API
(`AnthropicAdapter::attempt_metadata`), matching the "Adding an adapter is
configuration plus an `InferenceAdapter` implementation; it changes no
loop... semantics" principle at the boundary that actually matters: the
shared contract types other adapters and the loop core depend on.

## Test harness

`crates/familiar-ai-llm/src/anthropic_api.rs`'s own `#[cfg(test)]` module,
`crates/familiar-ai-agent/tests/anthropic.rs`, and
`crates/familiar-ai-daemon/tests/anthropic_adapter.rs` cover streaming,
parallel and sequential tool calls, the `tool_use` replay across turns,
malformed tool arguments, partial/interrupted streams, missing usage,
cache-category reporting, model-alias-vs-resolved-identity, refusal and
pause-turn stop reasons, rate limits and retry-after, provider errors,
authentication failure, and exactly-once accounting persistence on replay —
every one of them against `wiremock::MockServer`. No test anywhere in this
adapter's suite performs, or is able to perform, a live or billable call.
