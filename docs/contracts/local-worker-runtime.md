# Local Worker Runtime Contract

**Status:** Normative
**Date:** 2026-09-03

This document defines PRD-063's local-inference worker contract: how
`provider = "local"` workers are identified, how their PRD-058 raw-runtime
adapters implement the shared loop, how their telemetry is recorded without
inventing cost, and how the PRD-056 scheduler acquires PRD-064 typed
reservations over the hardware they consume. It does not redefine anything
in the [agent-loop contract](agent-loop.md), the
[PRD-062 artifact registry](registry-migration.md), or the PRD-064
reservation lifecycle — adding a local worker changes no loop, routing,
accounting, or reservation semantics.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD
NOT**, and **MAY** are to be interpreted as described by RFC 2119.

## Scope and non-goals

- Phase 1 (this implementation): externally managed OpenAI-compatible local
  endpoints, two `RuntimeId`s (`ollama`, `unsloth`) over one neutral
  transport. Process-managed lifecycle (Familiar launching and supervising
  a server), MLX-native/llama.cpp-direct backends, and operator allocation
  policies beyond the disabled-by-default mechanism described below are
  explicitly backlogged.
- Real hardware telemetry (accelerator/CPU utilization sensors, thermal and
  power ceilings) is platform-dependent and not probed by this
  implementation; every such field is typed `Option` and stays absent
  (never a fabricated value) until a caller supplies a real measurement.
  "Absent sensor" and "not yet wired up" are recorded identically as
  unknown — this document does not claim to distinguish them.

## Worker identity

A local worker is a complete PRD-057 spec:

- `provider = "local"` (`familiar_ai_core::config::LOCAL_PROVIDER`).
- A `RuntimeId`: `unsloth` or `ollama`
  (`familiar_ai_core::config::LocalRuntimeKind`).
- A PRD-062 `ModelArtifactId` (`model_artifact`), verified where the
  runtime exposes the means (see [Artifact verification](#artifact-verification)).
- A capability profile (`capability_profile`, generic to every worker
  kind).
- An endpoint profile (`local.endpoint`): base URL and derived trust class.
- A hardware/resource profile (`local.resources`), where relevant.

The same model artifact under two different `RuntimeId`s (e.g. `ollama` vs.
a future `mlx`) is two distinct workers: distinct `spec_identity` hashes,
distinct routing candidates, distinct empirical history. Nothing in this
contract ever merges them.

```toml
[worker_registry.workers.llama3-ollama]
provider = "local"
runtime = "ollama"
model = "llama3"
model_artifact = "sha256:<64 hex>"
capabilities = ["implementation"]

[worker_registry.workers.llama3-ollama.local]
runtime_kind = "ollama"

[worker_registry.workers.llama3-ollama.local.endpoint]
base_url = "http://127.0.0.1:11434"

[worker_registry.workers.llama3-ollama.local.resources]
accelerator_memory_mb = 8192
concurrent_inference_slots = 1
```

`familiar_ai_core::config::WorkerRegistryConfig::validate` enforces:
`local.runtime_kind.as_str()` must equal the worker's `runtime`; declaring
a `local` profile requires `provider = "local"` (the `local` block is the
PRD-063 opt-in marker — the reverse is **not** required, since
`provider = "local"` is an ordinary free string other worker kinds may
already use for unrelated reasons, e.g. a Codex-harness worker labeled
"local" for a cheap on-box model); a non-loopback endpoint (see
[Endpoint trust](#endpoint-trust)) requires the ordinary `auth_profile`
BYO-Auth reference; `local` and the legacy `runtime_config`
(`OllamaRuntimeConfig`, owned by the Codex-harness `ollama` adapter path)
are mutually exclusive on the same worker entry.

## OpenAI-compatible transport is reuse only

`unsloth` and `ollama` share one neutral transport
(`familiar_ai_llm::local_runtime`): an OpenAI-compatible Chat Completions
client (`POST {base_url}/v1/chat/completions`, streaming SSE) and a
model-listing probe (`GET {base_url}/v1/models`). This is transport reuse
only — **never** a claim of OpenAI behavior, capabilities, pricing, or
semantics (PRD-061's discipline applied locally). Each runtime keeps its
own `RuntimeId`, capability profile, and empirical identity; nothing here
implies feature parity between the two backends or with any OpenAI
product.

### Request mapping

- PRD-058 message history projects onto Chat Completions messages:
  `system`/`user`/`assistant` text messages map directly;
  `MessageContent::ToolCalls` becomes an assistant message with
  `tool_calls`; a tool result becomes a `role: "tool"` message keyed by
  `tool_call_id`.
- Canonical capabilities project onto `tools: [{"type": "function",
  "function": {...}}]`, each declared field typed as a JSON Schema string
  property (the canonical schema validates presence, not type — matching
  every other adapter's projection in this workspace).
- A structured-output request becomes `response_format: {"type":
  "json_schema", "json_schema": {"name": ..., "schema": ...}}`.

### Streaming

The client reads the complete SSE response body (`data: {...}` frames
terminated by `data: [DONE]`), then replays events to the loop's
`StreamObserver` in order:

| Chat Completions delta | PRD-058 `StreamEvent` |
|---|---|
| `choices[0].delta.content` | `TextDelta` |
| `choices[0].delta.tool_calls[].id`/`.function.name` (first seen) | `ToolCallDelta` (announces `call_id`/`capability_id`) |
| `choices[0].delta.tool_calls[].function.arguments` | `ToolCallDelta` (argument fragment) |
| `choices[0].finish_reason` present | terminal — see below |

Tool calls are keyed by the backend's `index` field until a `call_id`
streams in; a call with no id ever announced falls back to `call_{index}`,
matching this workspace's other adapters' honest "reconstruct only what's
actually available" discipline. A body with no `finish_reason` at all is
the honest `Ambiguous` case: the endpoint may have accepted and executed a
request whose response never fully arrived. A body that produces no
parsable chunks whatsoever (not even `[DONE]`) is also `Ambiguous` — never
silently treated as an empty success.

### Stop reasons

| `finish_reason` | PRD-058 `AdapterStopReason` |
|---|---|
| `stop` | `EndTurn` |
| `tool_calls` | `ToolUse` |
| `length` | `MaxTokens` |
| `content_filter` | `ContentFilter` |
| anything else / absent | `AdapterError::Ambiguous` (never a guessed reason) |

### Usage

`prompt_tokens`/`completion_tokens` map to `uncached_input_tokens`/
`output_tokens`; `prompt_tokens_details.cached_tokens` and
`completion_tokens_details.reasoning_tokens`, when a backend reports them,
map to `cache_read_tokens`/`reasoning_output_tokens` and are subtracted
from the uncached total (the same convention used by every OpenAI-shaped
usage object in this workspace). Every category stays `None`, never a
fabricated zero, until reported.

### Errors and crash/disappearance

| Condition | `AdapterError` |
|---|---|
| HTTP 401/403 | `NonRetryable(Auth)` |
| HTTP 429 | `Retryable(RateLimited)` |
| HTTP 400/404 | `NonRetryable(InvalidRequest)` |
| HTTP 5xx | `Retryable(Overloaded)` |
| Connection refused/reset before any response (crash, never started, endpoint disappeared) | `Retryable(TransientTransport)` — nothing was executed |
| Client-side timeout waiting for a response | `Ambiguous` — the endpoint may have already started executing |
| Response body cut short after headers arrived | `Ambiguous` — same reasoning as a timeout |

The PRD-058 loop's own handling of these variants means a `Retryable`
failure records **zero** attempts (nothing billable happened) and an
`Ambiguous` failure records exactly one attempt marked `ambiguous` with
entirely-unknown usage — this document changes none of that; it only
supplies the local-specific classification feeding into it.

## Artifact verification

The adapter (`LocalInferenceAdapter::verify_artifact`) compares the
endpoint's claimed model against a registered PRD-062 artifact digest
wherever the runtime exposes one:

- **`ollama`**: `GET {base_url}/api/tags` lists each served model's name
  and digest. A matching name with a matching digest is `Verified`; a
  matching name with a **different** digest is `Mismatch` (an active
  refusal signal — never silently accepted); anything else (no registered
  digest to compare against, the probe fails, the name is absent) is
  `Unverifiable`.
- **`unsloth`**: always `Unverifiable` — its OpenAI-compatible surface
  exposes no digest metadata as of this writing (PRD-063's assumption log;
  re-verify against Unsloth's current serving-mode documentation before
  assuming otherwise at a later date).

`Unverifiable` and `Mismatch` are **never** treated as a match. A worker
whose artifact is `Unverifiable` runs as a degraded unverified-artifact
worker (`familiar_ai_storage::repos::local_telemetry::
LocalArtifactVerificationState::DegradedUnverified`); routing policy
decides whether a degraded worker is eligible at all — this contract does
not itself refuse it, matching `ModelArtifactConfig::routing_eligible`'s
existing `require_verified` switch elsewhere in the registry.

## Telemetry, not invoices

Local inference has no provider invoice and none is fabricated. Every
attempt's typed, timestamped observation is recorded to migration 057's
`local_worker_telemetry` table (`familiar_ai_storage::repos::
local_telemetry::LocalTelemetryRepository`), attributed to the full spec
identity (`spec_identity`, `empirical_version`, `worker_identity`,
`runtime_id`, `model_artifact_id`), project, execution, and stage exactly
like any other worker's ledger row — but in a table entirely separate from
the PRD-051 `usage_observations`/`cost_estimates` ledger, since local
telemetry carries fields (wall time, time to first token, tokens/sec, load
time, peak memory, accelerator/CPU utilization, retries, failure kind) that
have no equivalent there and **no cost field of any kind**.

Every telemetry field is typed `Option` (or, for token categories, the
existing PRD-058 `UsageCategories` unknown-safe convention) and stays
absent rather than a fabricated zero until credibly measured. Energy
(`energy_wh`) is recorded only alongside its measurement provenance — a
`CHECK` constraint in migration 057 enforces that an energy value can never
be stored without saying where it came from.

### Operator allocation estimates

Operator-configured allocation policies (electricity, amortized hardware,
hosted local-server cost) MAY later produce **operator-allocation cost
estimates** — `familiar_ai_storage::repos::local_telemetry::
LocalTelemetryRepository::record_allocation_estimate`, backed by migration
057's `local_allocation_policies`/`local_allocation_estimates` tables:

- A policy is **disabled by default** (`enabled = 0`) and is **never
  enabled implicitly** — `record_allocation_estimate` fails closed
  (`FamiliarError::Config`) whenever the named `(policy_id,
  policy_version)` is unregistered, or registered but not explicitly
  flipped to `enabled = 1`.
- Every estimate row carries: the policy identity and version it was
  computed under, the input measurements and declared assumptions behind
  it (as JSON), a currency, an effective period, a provenance string, and
  a closed `estimated_authority_label = 'estimated'` — this is never a
  `subscription-declaration` and never presented as authoritative.
- `cost_category` is closed to the single value `'operator-allocation'`
  (a `CHECK` constraint), distinguishing it from every PRD-051
  `cost_estimates.provenance` value (`vendor-reported`, `configured-rate`,
  `known-zero`) at the schema level.
- Allocation estimates live in their own table, never inside
  `cost_estimates` — the two are combinable only by an explicit join a
  caller performs, never automatically, satisfying "never mixed with
  provider invoices without explicit grouping."

## Scheduling: hardware is the local rate limit

The daemon (`familiar_ai_daemon::local_worker_runtime`) treats developer
hardware as scarce, reserved capacity via the ordinary PRD-064
`ReservationRepository` lifecycle — this module adds no new reservation
mechanics, only the local-specific request shapes and policy glue:

- `execution_resource_requests(pool_prefix, accelerator_memory_mb,
  system_memory_mb, exclusive_runtime)` builds one PRD-064
  `ResourceRequest` per resource this worker's profile declares: one
  `InferenceSlots` unit (every execution), the worker's own declared
  `AcceleratorMemory`/`SystemMemory` footprint when configured, and one
  `ExclusiveRuntime` unit when the worker requires sole occupancy of its
  runtime process. `ModelLoadingSlots` is requested separately
  (`acquire_model_load_reservation`) and held only for the cold-start
  window, independent of the inference-slot reservation covering the rest
  of the execution.
- **Unknown capacity never invents availability.** A pool nobody has ever
  called `define_pool` for reports zero available capacity by construction
  (PRD-064's own `ReservationRepository::acquire`), so an unconfigured
  resource is refused by default. `UnknownCapacityPolicy::
  SerializeConservatively` is the one explicit alternative: it bootstraps
  exactly one single-occupant pool per unknown resource before retrying —
  never a larger invented number, and never automatic.
- **Managed vs. externally shared capacity.** `LocalCapacityClass::Managed`
  capacity is exclusively Familiar's; `ExternallyShared` capacity can be
  consumed by another process, user, or client outside Familiar's control.
  `CapacityObservation` carries this class alongside a freshness timestamp
  (`capacity_is_fresh`); coexistence is guaranteed only over `Managed`
  capacity — this contract never claims otherwise for `ExternallyShared`
  pools.
- **Honest resolution, never masked as worker error.** `RunOutcome`'s
  `StopReason` resolves the reservation via `resolve_reservation`:
  - `Completed`/ceiling/refusal outcomes **commit** with observed
    consumption (`ReservationRepository::commit`), and an observation
    larger than the granted amount is recorded as an honest `overrun` —
    unexpected external contention or mid-run memory pressure surfaces
    here, never silently absorbed.
  - `Cancelled` **releases** the reservation cleanly (nothing was
    consumed).
  - `Timeout` and `ProviderFailure` (which covers crash and endpoint
    disappearance — see [Errors and crash/disappearance](#errors-and-crashdisappearance))
    **hold** the reservation via `SettlementObservation::Unknown{policy:
    HoldReservation}`: consumption is genuinely unknown, so capacity is
    never optimistically released nor double-counted as freed while a
    process might still be using it. Recovery of a held reservation
    follows the ordinary PRD-064 owner-liveness lifecycle
    (`ReservationRepository::recover`), unchanged by this contract.
- **Reservation races** are the same atomic, transaction-scoped
  `ReservationRepository::acquire` every other PRD-064 consumer relies on;
  this contract adds no separate locking of its own (see the storage
  crate's own `concurrent_claimants_cannot_double_spend_capacity` test and
  this contract's own race test over an `ExclusiveRuntime` pool).

## Security

- **Endpoint trust is explicit and derived**, never operator-declared:
  `familiar_ai_core::config::classify_local_endpoint_trust` classifies a
  base URL's host into `Loopback` (127.0.0.0/8, `::1`, `localhost`),
  `Lan` (RFC1918 IPv4, link-local, unique-local IPv6), or `Remote`
  (everything else — the stricter default for anything not positively
  classified as private). A non-`Loopback` endpoint **requires** the
  ordinary `auth_profile` BYO-Auth external reference; nothing about
  "owned hardware" waives that requirement.
- **No extra capability from local execution.** A local model gains no
  tool authority, filesystem access, or environment beyond PRD-058's
  `ScopeAuthorizer`/`SandboxedToolExecutor` rules merely because it runs on
  hardware the operator owns — this contract introduces no new
  authorization surface.
- **Weights and templates are untrusted data.** A malicious chat or
  tool-call template can shape the text a local model produces, but tool
  authority, validation, and execution flow entirely through PRD-058's
  ordinary `validate_tool_call`/`ToolAuthorizer`/write-ahead journal —
  identical to every other adapter. Template-driven injection cannot
  change what capability, argument, or write scope is authorized.
- **Telemetry is privacy-clean.** `local_worker_telemetry` rows carry only
  identity fields, token *counts*, timing, resource, and failure-kind
  strings — never a prompt, a model output, or a credential.
- **Credentials never cross the transport boundary implicitly.**
  `LocalAuthToken` never implements `Display` and its `Debug` output is
  always redacted, matching every other adapter's `ApiKey`-shaped
  credential type in this workspace.

## Comparability with money-semantics workers (PRD-032)

Local-resource consumption (reservations, telemetry) is its own metric
class, entirely distinct from dollar cost. An unknown dollar cost never
ranks as zero or as cheap merely because a worker is local — the existing
PRD-007 discipline (`estimated_cost_microusd: Option<u64>`, absent means
unmeasured, never free) is unchanged by this contract, and local workers
carry no cost field to compare against a hosted worker's dollar figure in
the first place. Acceptance-rate and remediation metrics for local workers
come from the ordinary PRD-032 evidence pipeline, not from this adapter.

## Test harness

`crates/familiar-ai-llm/src/local_runtime.rs` unit-tests the wire protocol
(health probe, streaming, tool calls, structured output, usage categories,
timeout/crash/HTTP error taxonomy, artifact digest verification and its
degraded/mismatch paths) against `wiremock` — a loopback fake, never a real
network call. `crates/familiar-ai-agent/tests/local_worker.rs` drives the
real PRD-058 `run_loop` against the same kind of fake server, including
cancellation, endpoint disappearance, and exactly-once-per-submission
attempts, for both `ollama` and `unsloth`.
`crates/familiar-ai-daemon/tests/local_worker_runtime.rs` exercises the
full daemon-side pipeline — reservation acquisition, the real loop against
a fake endpoint, reservation resolution, and telemetry persistence — plus
dedicated coverage for mid-run memory pressure (honest overrun) and
endpoint disappearance (held reservation, zero fabricated attempts).
`crates/familiar-ai-daemon/src/local_worker_runtime.rs` additionally
unit-tests unknown-capacity refusal/conservative-serialization and a
concurrent reservation race over an exclusive-runtime pool. No test in any
of these files performs, or is able to perform, real model execution or
any network beyond a loopback fake.
