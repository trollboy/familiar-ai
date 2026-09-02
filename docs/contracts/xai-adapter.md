# xAI Grok Raw-API Adapter Contract

**Status:** Normative
**Date:** 2026-09-01

This document defines PRD-061: xAI as a first-class provider — Grok models
through the official xAI API, as a PRD-058 raw runtime (`familiar_ai_llm::xai_api::XaiAdapter`,
`RuntimeId = xai-api`). It governs xAI's own wire protocol, capability
profile, usage/pricing, and security posture; it does not restate the
provider-neutral agent loop, which is [`agent-loop.md`](agent-loop.md)'s
contract.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD
NOT**, and **MAY** are to be interpreted as described by RFC 2119.

## Goals

- xAI is its own `ProviderId` (`xai`) with its own `RuntimeId` (`xai-api`)
  and Grok as its own model family — never "an OpenAI worker" in any
  routing, scoring, pricing, capability, or accounting path.
- Every wire detail this adapter relies on is verified against current
  `docs.x.ai` documentation at implementation time (2026-09-01, this
  document) or safely probed; nothing is declared from OpenAI protocol
  resemblance.
- Tool-call streaming, usage fields, structured output, reasoning controls,
  and billing capability all carry honest PRD-057 provenance
  (`declared`/`probed`/`observed`/`unknown`) — resemblance is never
  evidence.
- Credentials are BYO-Auth external references only, resolved in the
  adapter boundary, never written to configuration, prompts, tools,
  accounting rows, logs, or unrelated subprocess environments.

## Non-goals

- A new wire-protocol abstraction. Where xAI's `/v1/chat/completions` shape
  happens to resemble another OpenAI-compatible surface, that resemblance
  is engineering economy only; nothing in this adapter is owned by, or
  requires, PRD-060's OpenAI adapter, and PRD-060 owns none of this wire
  contract.
- The provider-neutral loop, tool journal, authority/sandbox model, or
  PRD-051 ledger mechanics — all of that is [`agent-loop.md`](agent-loop.md)'s
  contract, reused unmodified.
- Authoritative xAI organization billing. No official programmatic
  administrative usage/cost API was found in the consulted xAI
  documentation; that surface is out of scope until one exists (see
  [Billing](#billing)).

## Wire protocol (verified 2026-09-01 against `docs.x.ai`)

- **Base URL:** `https://api.x.ai/v1`. **Auth:** `Authorization: Bearer
  <key>` — configured as a BYO-Auth `env: NAME` descriptor (e.g. `env:
  XAI_API_KEY`); the credential value is never accepted in configuration.
- **Endpoint used:** `POST /v1/chat/completions` with `"stream": true` and
  `"stream_options": {"include_usage": true}`. xAI also documents a
  `/v1/responses` endpoint whose non-streaming request/response shape
  (`input`, `tools`, `tool_choice`, `parallel_tool_calls`,
  `previous_response_id`; output items typed `output_text` /
  `function_call` with `call_id`/`name`/`arguments`) is OpenAI-Responses-
  shaped and consistent with this PRD's design note — but its **streaming
  SSE event names could not be confirmed** in the consulted documentation
  (only `/v1/chat/completions` streaming is documented: `data: {json}`
  chunks terminated by `data: [DONE]`). Building the streaming path against
  unconfirmed event names would be resemblance-based guessing, which this
  PRD forbids; `/v1/chat/completions` is used instead because its streaming
  shape, usage fields, and per-request cost figure are all confirmed. This
  is a deliberate deviation from the PRD's `/v1/responses`-first framing,
  made because implementation-time verification pointed the other way — re-
  verify `/v1/responses` streaming before switching to it.
- **Tool calls:** functions are declared as `{"type": "function",
  "function": {"name", "parameters"}}`; the model returns calls with
  `call_id`/`name`/`arguments` (`arguments` a JSON string); **parallel tool
  calls are enabled by default**. **Streaming delivers a function call
  whole in a single chunk, not argument-streamed across deltas** — a
  verified xAI-specific capability difference from delta-streamed
  providers, and the reason `XaiAdapter` emits `ToolCallDelta` immediately
  followed by `ToolCallComplete` for every tool call it sees, rather than
  accumulating fragments.
- **Usage** (in the final streamed chunk): `prompt_tokens`,
  `completion_tokens`, `prompt_tokens_details.{text_tokens,cached_tokens,audio_tokens,image_tokens}`,
  `completion_tokens_details.reasoning_tokens`, and a per-request
  `cost_in_usd_ticks`. **No cache-*write* token count is documented
  anywhere consulted** and stays `None`, never zero, never borrowed from
  another provider's semantics. `uncached_input_tokens` is populated only
  from the `prompt_tokens_details.text_tokens` breakdown, never guessed
  from a bare `prompt_tokens` total that could include cached tokens.
- **Model identity:** the request's `model` field is the alias Familiar's
  config selected (e.g. `grok-4`); the response's own `model` field is
  xAI's provider-resolved identity (e.g. `grok-4-0709`), which can differ
  and can drift over time. `XaiAdapter::last_resolved_model()` exposes this
  separately, purely as telemetry — a caller MUST NOT overwrite canonical
  PRD-057 worker identity (`worker_specs.model_id`, keyed on the
  *configured* alias) with a resolved value that can move underneath a
  stable config.
- **Errors:** HTTP status is the observed signal (`docs.x.ai` does not
  document a stable error-body schema in the pages consulted): `401`/`403`
  → `NonRetryable::Auth`; `429` → `Retryable::RateLimited` (reading
  `Retry-After` if present); `400`/`404`/`422` → `NonRetryable::InvalidRequest`;
  `5xx` → `Retryable::Overloaded`; a connect/pre-response transport failure
  → `Retryable::TransientTransport` (nothing was billed); a transport
  failure or stream closure *after* data has started arriving, or a stream
  that never reaches a `[DONE]`/`finish_reason` → `Ambiguous` (usage for
  that attempt is unknown/pending, never zero — the provider may have
  already generated and billed tokens for a response that never fully
  arrived).
- **Retries/idempotency:** no officially documented request-level
  idempotency guarantee was found; `provider_idempotency_key` is always
  `None`. Every `submit` is its own `AttemptId` and PRD-064 reservation per
  `agent-loop.md`; a retry is a new attempt, never a free replay.
- **Structured output:** `response_format: {"type": "json_schema",
  "json_schema": {"name", "schema"}}` is sent when requested. xAI's
  overview page documents structured outputs only at a high level; this
  exact wire shape is **probed, not documentation-verified** — see
  [Capability provenance](#capability-provenance).
- **Reasoning controls:** no request-side reasoning-control parameter was
  verifiable in the consulted documentation. `XaiAdapter` sends none.

## Capability provenance

Per PRD-057, every capability below carries honest provenance —
`declared` (confirmed against official documentation), `probed`
(exercised and observed, not documentation-confirmed), or `unknown` (not
established either way; never inferred from resemblance to another
provider). See `familiar_ai_agent::xai::XAI_CAPABILITY_PROFILE` for the
machine-readable form an operator's `[worker_registry.capability_profiles.*]`
configuration should be populated from.

| Capability | Provenance | Note |
|---|---|---|
| `native-tool-calling` | declared | `call_id`/`name`/`arguments`, parallel by default |
| `streaming` | declared | SSE, `data: [DONE]` termination |
| `parallel-tool-calls` | declared | on by default per `docs.x.ai` |
| `usage-reporting-categories` | declared | fields listed above; no cache-write count |
| `cost-reporting-mode` | declared | per-request `cost_in_usd_ticks` is vendor-reported; no admin/org billing API |
| `structured-output` | probed | `response_format` shape exercised against mocks only |
| `reasoning-controls` | unknown | no verifiable request parameter; none sent |

## Usage and accounting

Observations normalize only the fields verified above and land as PRD-051
`UsageObservation` rows through the existing, provider-agnostic
`familiar_ai_daemon::agent_runtime::persist_run_outcome` — unmodified by
this PRD — with `adapter = "xai-api"` and `model_identity` set to the
*requested* alias. An attempt with entirely unknown usage records an
explicit `unknown_reason`, never a fabricated zero, exactly as
`agent-loop.md` requires generically.

xAI's per-request `cost_in_usd_ticks` (10,000,000,000 ticks = 1 USD; see
`docs.x.ai`'s cost-tracking guide) is a genuine vendor-reported cost figure
— rarer among providers — but the shared `SubmitOutcome`/`UsageObservation`
contracts carry no cost field for any provider today. `XaiAdapter` captures
it losslessly via `last_cost_usd_ticks()` (never pre-converted, so a later
reconciliation stage can interpret it exactly) as the observational hook a
future accounting-wiring PRD plumbs into `provider_cost_lexical`; this PRD
does not modify `persist_run_outcome` to consume it, matching every other
provider's adapter-specific-field state today.

## Pricing

Grok price schedules are versioned, dated, and sourced from
`docs.x.ai/docs/models` as consulted 2026-09-01 (cross-checked against
independent secondary sources for consistency). They key on the
*model family* member; xAI's per-model context-tier surcharge (a higher
rate at ≥200k prompt tokens) is represented, where needed, as a distinct
model-string key (e.g. `grok-4.6` for <200k, `grok-4.6-200k` for ≥200k) —
`PriceScheduleConfig` has no native tiered-rate representation, and this is
the same limitation every provider's schedule has today, not an xAI-
specific gap.

Example schedule (nanoUSD, 1 USD = 1,000,000,000 nanoUSD), reproduced and
tested in `crates/familiar-ai-agent/tests/xai.rs`:

```toml
[execution_history.price_schedules.xai-2026-09-01]
effective_at = "2026-09-01T00:00:00Z"
currency = "USD"
calculation_version = "xai-pricing-2026-09-01"

[execution_history.price_schedules.xai-2026-09-01.models.grok-4.6]
uncached_input_nanousd_per_million = 2_000_000_000
cache_read_nanousd_per_million = 500_000_000
output_nanousd_per_million = 6_000_000_000

[execution_history.price_schedules.xai-2026-09-01.models.grok-4.3]
uncached_input_nanousd_per_million = 1_250_000_000
cache_read_nanousd_per_million = 200_000_000
output_nanousd_per_million = 2_500_000_000
```

No OpenAI price schedule is ever applied to an `xai-api` worker; xAI has no
entry in the `BillingMode` vocabulary and none is added by this PRD (see
[Billing](#billing)).

## Billing

**No official programmatic administrative usage or cost API was found in
the consulted xAI documentation.** Authoritative xAI billing is therefore
explicitly unsupported, in the PRD-051 `external-billing`/unknown sense:
`familiar_ai_core::config::BillingMode` gets no `Xai` variant from this
PRD, and any xAI month-to-date spend report is computed only from local
`configured-rate` price-schedule estimates (or, once a future PRD wires it
through, `vendor-reported` per-request `cost_in_usd_ticks`) — never
presented as authoritative, and always labeled as local-estimate coverage
only. If xAI ships an official organization billing/cost-reporting API
later, it arrives as its own collector under the PRD-052/054 pattern, not
as an assumption here.

## Security

- The xAI API key is an external reference (`AuthDescriptor::Env`)
  resolved fresh inside `XaiAdapter::submit`, never cached beyond one
  call's stack, never written to configuration, a prompt, a tool, an
  accounting row, or a log. A non-`env:` descriptor, or a missing named
  variable, **fails closed** with `NonRetryable::Auth` and the same
  BYO-Auth remedy text used elsewhere in Familiar: configure an `env: NAME`
  descriptor and export the named variable — a credential value is never
  accepted in configuration.
- `run-command` tool subprocess environments inherit nothing from the
  daemon process (per `agent-loop.md`'s sandbox model, unmodified); the xAI
  key never reaches a tool subprocess.
- There is no xAI *admin* credential class to confuse with the per-request
  API key, because no xAI admin surface is integrated (see
  [Billing](#billing)).

## Test harness

- `crates/familiar-ai-llm/src/xai_api.rs` (`#[cfg(test)]`): wire-level
  `wiremock` coverage — whole-chunk tool calls, parallel tool calls,
  malformed tool-call arguments forwarded verbatim, a partial stream with
  no `[DONE]` (ambiguous), missing usage (stays unknown), alias drift,
  missing/non-`env` auth (fails closed), HTTP 401/429/400/500, and that
  every `submit` is its own HTTP request (no dedup).
- `crates/familiar-ai-agent/tests/xai.rs`: `XaiAdapter` round-tripped
  through the unmodified `raw_runtime::run_loop` — whole-chunk tool call to
  completion, a real wall-clock timeout against a stalled mock response
  (ambiguous usage, `StopReason::Timeout`), a `5xx` mapping to
  `ProviderFailure{Retryable}`, auth failure with no request ever sent, and
  the Grok price-schedule/cost-tick fixtures above.
- `crates/familiar-ai-daemon/tests/xai_adapter.rs`: the same SQLite-backed
  tool journal, sandboxed executor, and `persist_run_outcome` path proven
  generically in `raw_runtime.rs`, exercised against the real `XaiAdapter`;
  and a direct `AccountingRepository::append_observation` idempotency test
  proving a replayed provider event (same `source_event_hash`) never
  double-counts while a genuinely separate attempt remains its own
  observation.

No test in any of the three files performs, or is able to perform, a live
or billable call.

## Known deviations from this PRD's design note

- **Endpoint:** `/v1/chat/completions` is used instead of `/v1/responses`
  for streaming, because `/v1/responses`' streaming SSE event names could
  not be verified — see [Wire protocol](#wire-protocol-verified-2026-09-01-against-docsxai).
- **`crates/familiar-ai-core/src/config/providers.rs`** (listed in this
  PRD's `expected_files`) is unmodified. Its `InferenceRuntimeKind` enum
  and the `[providers.*]` `kind = "inference"` table it belongs to govern a
  different, narrower concern — self-hosted/discoverable endpoint entries
  probed live via `familiar-ai config provider add` (today, only
  `unsloth`) — orthogonal to a raw-API cloud adapter's `RuntimeId`. That
  identity is already fully generic: `RegistryWorkerConfig.provider`/`.runtime`
  are free, validated strings (`provider = "xai"`, `runtime = "xai-api"`),
  requiring no enum change, and `RegistryWorkerConfig.auth_profile` names a
  top-level `[auth_profiles.*]` `AuthDescriptor` the same way every other
  raw-API worker does. Adding an unused `InferenceRuntimeKind` variant with
  no CLI or probe wiring behind it (`config_cli.rs`'s `provider add`
  command, which is what actually sets that field, only recognizes
  `unsloth`) would be dead code with no caller. If a later PRD makes xAI
  discoverable the way Unsloth is, that is new, real behavior for that PRD
  to add — not a speculative enum variant here.
