# OpenAI Responses API Adapter Contract

**Status:** Normative
**Date:** 2026-09-01

This document defines the OpenAI-specific behavior of the PRD-060
`openai-api` raw runtime: how it implements the
[PRD-058 agent-loop contract](agent-loop.md) over OpenAI's Responses API. It
does not redefine anything in that contract — adding this adapter changes
no loop, routing, accounting, or execution semantics.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD
NOT**, and **MAY** are to be interpreted as described by RFC 2119.

## Scope and non-goals

- This document covers the `openai-api` runtime only: request/response wire
  shape, streaming event mapping, usage normalization, identity, billing
  mode, and security. The loop state machine, canonical capability
  vocabulary, write-ahead journal, and stop-reason set are entirely
  PRD-058's and are not restated here except where OpenAI's wire shape maps
  onto them.
- Provider details in this document were verified 2026-09-01 against the
  `openai-python` SDK's type definitions (the interactive API reference was
  not reachable from the implementation environment). OpenAI's schemas
  carry no stability guarantee; re-verify at the next change rather than
  freezing this snapshot.

## Protocol

Requests target `POST {base_url}/responses` with `stream: true`. The
adapter (`familiar_ai_llm::openai_api`) builds:

- **`input`** — the PRD-058 message history projected into Responses API
  input items: `system`/`user` messages become `{"type":"message","role":
  ..., "content":[{"type":"input_text","text":...}]}`; a replayed
  assistant text turn becomes the same shape with `output_text` content; a
  tool result becomes `{"type":"function_call_output","call_id":...,
  "output":...}`.
- **`tools`** — each canonical capability's simplified field-presence
  schema (`{"required":[...],"optional":[...]}`, PRD-058's own
  representation, not general JSON Schema) is projected into a minimal
  valid OpenAI function tool: every declared field becomes a `string`
  property (the canonical schemas validate presence, not type).
- **`text.format`** — a structured-output request becomes
  `{"type":"json_schema","name":...,"schema":...,"strict":true}`.
- **`reasoning.effort`** — relayed verbatim from the capability profile's
  `ReasoningControl.effort` when present; OpenAI has no equivalent of
  Anthropic's `budget_tokens`, so that field is never projected.
- **`prompt_cache_key`** — the PRD-029 stable-prefix version, whenever
  `SubmitRequest.cache_control` is not `None`. OpenAI's prompt caching is
  automatic once a stable prefix and key are supplied; there is no
  provider-side "ephemeral vs. persistent" breakpoint choice the way
  Anthropic's `cache_control` blocks offer, so `Ephemeral` and `Persistent`
  are equivalent for this runtime.
- **`service_tier`** — an optional, adapter-construction-time setting
  (`OpenAiResponsesConfig::service_tier`), not a per-turn `SubmitRequest`
  field: which processing tier (`flex`/`priority`/`batch`/...) a worker
  uses is a deployment choice, not something the loop decides per turn.

Model tool calls arrive as `function_call` output items carrying
`call_id`, `name`, and `arguments`; results are matched back by `call_id`.
Output items carry `status` (`in_progress`/`completed`/`incomplete`); the
top-level `response.status` (`completed`/`incomplete`/`failed`) is what
this adapter maps onto PRD-058 stop semantics (see below).

### Function-call replay

The Responses API requires the original `function_call` item to precede
its `function_call_output` whenever input is resent from scratch. PRD-058's
loop resends its full message history every turn but retains only the tool
*result* in that history, not the call's `name`/`arguments`. This adapter
compensates entirely inside its own boundary: `OpenAiResponsesClient` keeps
a `call_id -> (name, arguments)` cache populated from each response's own
`function_call` output items, and replays the cached item ahead of the
matching `function_call_output` on the next turn. This is a
provider-specific request-shape accommodation — the loop never sees it,
and no other adapter is required to do anything like it.

## Streaming

The adapter reads the complete SSE response body, then replays its events
to the loop's `StreamObserver` in provider order, mapping:

| OpenAI event | PRD-058 `StreamEvent` |
|---|---|
| `response.output_text.delta` | `TextDelta` |
| `response.output_item.added` (`function_call`) | `ToolCallDelta` (announces `call_id`/`name`) |
| `response.function_call_arguments.delta` | `ToolCallDelta` (argument fragment) |
| `response.function_call_arguments.done` / `response.output_item.done` | `ToolCallComplete` |
| `response.completed` / `response.incomplete` | terminal — see below |
| `response.failed` / `error` | mapped to `AdapterError`, not a stream event |

**Implementation note on incrementality.** This reads the full HTTP
response body before replaying events, rather than consuming the
`Transfer-Encoding: chunked` network stream chunk-by-chunk (the workspace's
`reqwest` dependency does not enable the `stream` feature). Observers still
see every event in order before the final outcome, and a body that ends
without a terminal event is classified `Ambiguous` exactly as an
incremental reader would classify a connection cut mid-stream — the
provider may have accepted, executed, and billed a request whose response
never fully arrived. True incremental network delivery MAY be added later
behind that feature without changing this module's public surface.

A malformed or unknown tool call is **never** this adapter's concern to
refuse: the adapter relays whatever raw argument text and capability name
the provider sent, faithfully and unedited. Validation and refusal (unknown
capability, unparseable accumulated arguments, oversized payload) are
PRD-058's `validate_tool_call`, exercised identically for every adapter.

## Usage

The Responses API usage object maps onto PRD-051 categories:

| OpenAI field | PRD-058 `UsageCategories` field |
|---|---|
| `input_tokens - input_tokens_details.cached_tokens` | `uncached_input_tokens` |
| `input_tokens_details.cached_tokens` | `cache_read_tokens` |
| `input_tokens_details.cache_write_tokens` | `cache_write_tokens` |
| `output_tokens` | `output_tokens` |
| `output_tokens_details.reasoning_tokens` | `reasoning_output_tokens` |

`cached_tokens` is a subset already counted within `input_tokens` (the
Chat Completions convention this API inherits), so uncached input is
derived by subtraction, never double-counted.

**Verified deviation from the PRD-060 design note (2026-08-30):** that note
assumed the provider reports no cache-write category. As of 2026-09-01 the
Responses API usage object *does* report
`input_tokens_details.cache_write_tokens`. This adapter records it
distinctly whenever the provider sends it and leaves the field `None` only
when the provider omits it — no category is fabricated in either
direction; the row simply reflects what the provider currently reports.
Every field stays `None`, never a fabricated zero, until the provider
reports it; a response with no `usage` object at all leaves every category
unknown.

The response `id` is the provider request identity, recorded on
`SubmitOutcome.provider_request_id` per the PRD-058 contract. The
PRD-058 contract has no field for the **response-resolved model identity**
or **service tier applied** — those are provider-specific facts, not loop
concerns — so this adapter additionally records them (plus a copy of the
provider request id, since the shared `run_loop`/`AttemptUsage` pairing
does not carry that field forward past one iteration) in a per-attempt
metadata map, exposed as `OpenAiInferenceAdapter::response_meta(attempt_id)`.
A host MAY use it to enrich its own `AccountingRepository` calls with the
exact identity that ran, entirely through the ordinary repository API — no
loop or persistence *semantics* change to do so, and the requested
(possibly aliased) model identifier is never overwritten: both stay
independently available.

## Structured output and reasoning

Structured output uses `text.format` with a `json_schema` object exactly
as the caller's `StructuredOutputRequest` supplies (schema name and JSON
Schema document). Reasoning effort is configured per capability profile
via `reasoning.effort` and is never separately estimated or assumed:
reasoning spend is whatever `output_tokens_details.reasoning_tokens`
reports, recorded through the normal `reasoning_output_tokens` category.

## Stop reasons

| OpenAI `response.status` | Detail | PRD-058 `AdapterStopReason` |
|---|---|---|
| `completed` | any `function_call` output items present | `ToolUse` |
| `completed` | otherwise | `EndTurn` |
| `incomplete` | `incomplete_details.reason == "max_output_tokens"` | `MaxTokens` |
| `incomplete` | `incomplete_details.reason == "content_filter"` | `ContentFilter` |
| `incomplete` | any other/unrecognized reason | `AdapterError::Ambiguous` (never a guessed `MaxTokens`) |
| `failed` | `error.code` a content-policy code | `AdapterError::NonRetryable(RefusedContent)` |
| `failed` | `error.code` a server-side code | `AdapterError::Retryable(Overloaded)` |
| `failed` | otherwise | `AdapterError::NonRetryable(InvalidRequest)` |

An incomplete or refused output is always recorded as its own honest
reason — never as token exhaustion by default. A response whose `status`
this adapter does not recognize is `Ambiguous`, for the same reason: an
unrecognized status is not evidence of any particular ceiling.

**Known limitation, not introduced here:** a `response.failed` status
carries a genuine terminal fact (unlike a truly ambiguous stream cutoff),
and its response payload can include a `usage` object. PRD-058's `run_loop`
does not record any `AttemptUsage` for `Retryable`/`NonRetryable` adapter
errors (only for `Ok` and `Ambiguous` outcomes) — that is shared,
adapter-neutral loop behavior this PRD does not change. Usage present on a
`failed` response is therefore not currently recorded; this is a property
of the shared loop's attempt model, not something specific to OpenAI.

## Identity

The requested `model` may be a moving alias (e.g. `gpt-5`); the response's
`model` field is the resolved identity and is what price schedules key on
where available (per PRD-057). The requested identifier is never
overwritten by the resolved one — both are independently recorded (see
[Usage](#usage)) so an alias is never frozen into canonical worker
identity.

## Conversation state

Familiar owns the loop (PRD-058); this adapter resends explicit input
items every turn, reconstructed from PRD-058's own message history plus
the function-call replay cache described above.
`previous_response_id`-style server-side continuation is not used by this
adapter: it is an isolated, optional transport optimization the Responses
API offers, never the source of truth for loop state, and a provider-side
session can never strand Familiar's own recovery path.

## Billing

Runtime `openai-api` under a project API key is `local-estimate` billing
mode (PRD-051), reconcilable against PRD-054's organization usage
(`/v1/organization/usage/completions`) and cost collectors where an Admin
source is configured. A project API key never grants Admin reporting
authority (PRD-054's rule) — the two are separate provider entries with
separate credential references and separate process-exposure rules.
Nothing from Codex CLI or ChatGPT-subscription billing (plan credits,
allowance, `turn.completed` telemetry) applies to this runtime: those are
different PRD-057 empirical identities (`codex` runtime) with different
billing modes entirely.

Price schedules for this runtime are ordinary PRD-051
`[execution_history.price_schedules."<id>"]` entries keyed by model; a
service-tier-dependent rate is expressed as a distinct model key (for
example `"gpt-5:flex"`) rather than a new schema dimension, since
`PriceScheduleConfig` already keys generically by string and no PRD-060
acceptance criterion requires widening that schema.

## Security

- The API key is a BYO-Auth external reference
  (`docs/contracts/credential-authentication.md`), resolved by the host at
  the adapter boundary (construction time) and held only for that
  adapter's lifetime. `familiar_ai_llm::openai_api::ApiKey` never
  implements `Display` and its `Debug` output is always `ApiKey([REDACTED])`.
  A missing `env: NAME` credential fails closed with the exact remedy
  (`required environment variable NAME is missing — export \`NAME\`.`,
  `familiar_ai_daemon::config_cli::check_auth`/`resolve_auth_with_store`).
- The key never appears in configuration values, prompts, tool
  definitions, accounting rows, logs, or evidence envelopes — accounting
  persists only token categories, identity fields, and content hashes,
  never raw request/response text (PRD-058's rule, unchanged).
- Tool subprocess environments inherit nothing from the adapter; the
  adapter has no subprocess surface at all — it only issues HTTP requests
  from the host process.
- An OpenAI Admin/organization-reporting credential (PRD-054) is a
  completely separate provider entry (`kind = "billing"`) with its own
  credential reference; nothing in this adapter's inference path can
  resolve or use it.

## Test harness

`crates/familiar-ai-llm/src/openai_api.rs` unit-tests the wire protocol
(streaming, tool calls, partial streams, malformed arguments, rate limits,
provider errors, missing usage, cached-input/cache-write/reasoning
categories, alias drift, auth failure, retry metadata) against `wiremock`.
`crates/familiar-ai-agent/tests/openai.rs` drives the real PRD-058
`run_loop` against the same fake server, including cancellation and
exactly-once per-submission attempts. `crates/familiar-ai-daemon/tests/
openai_adapter.rs` exercises the full daemon-side pipeline — SQLite
journal, evidence, `AccountingRepository` persistence, BYO-Auth resolution,
and idempotent replay of the same persisted provider event — against the
same fake server. No test in any of these files performs, or is able to
perform, a live or billable OpenAI call.
