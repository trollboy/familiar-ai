//! OpenAI Responses API wire protocol client (PRD-060).
//!
//! Request/response shapes verified 2026-09-01 against the current
//! Responses API (cross-checked against the `openai-python` SDK type
//! definitions, since the interactive API reference is not fetchable from
//! this environment): input items, native function tools,
//! `text.format`/JSON-schema structured output, `reasoning.effort`,
//! streaming SSE events, and the usage object's
//! `input_tokens_details.{cached_tokens,cache_write_tokens}` and
//! `output_tokens_details.reasoning_tokens` breakdowns.
//!
//! **Verified deviation from the PRD-060 design note (2026-08-30):** the
//! design assumed the provider reports no cache-write category. As of
//! 2026-09-01 the Responses API usage object *does* report
//! `input_tokens_details.cache_write_tokens`. This module maps it into
//! [`UsageCategories::cache_write_tokens`] distinctly whenever the provider
//! sends it and leaves it `None` only when the provider omits it — no
//! category is fabricated either way; the row simply reflects what the
//! provider currently reports.
//!
//! This module owns only the wire protocol and HTTP transport. The PRD-058
//! `InferenceAdapter` projection — canonical tool capabilities to OpenAI
//! function tools, PRD-058 message history to Responses API input items —
//! lives in `familiar_ai_agent::openai`, the sole consumer of this client.
//!
//! **Streaming note:** this client reads the complete SSE response body
//! before replaying its events to the [`StreamObserver`] in provider order,
//! rather than consuming the `Transfer-Encoding: chunked` network stream
//! incrementally chunk-by-chunk. The workspace's `reqwest` dependency does
//! not enable the `stream` feature. Observers still see every event in
//! order before the final outcome, and a response cut short before a
//! terminal event (`response.completed`/`response.incomplete`/
//! `response.failed`) is classified [`AdapterError::Ambiguous`] exactly as
//! an incrementally-read client would classify it. True incremental
//! network delivery can be added later behind that feature without
//! changing this module's public surface.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use crate::attempt::{
    AdapterError, AdapterStopReason, Message, MessageContent, MessageRole, NonRetryableKind,
    ReasoningControl, RetryableKind, StreamEvent, StreamObserver, StructuredOutputRequest,
    SubmitOutcome, ToolDefinition, UsageCategories,
};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// A resolved OpenAI API key, held only for the lifetime of the client that
/// needs it to authenticate a request. Never `Display`s or `Debug`s its
/// value — the BYO-Auth boundary (`docs/contracts/credential-authentication.md`)
/// requires that a credential never appears in a log line by accident.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn bearer_header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiResponsesConfig {
    pub base_url: String,
    pub request_timeout_secs: u64,
    /// Optional fixed `service_tier` request field (`flex`, `priority`,
    /// `batch`, ...). This is a per-worker deployment choice, not a
    /// per-turn one, so it is not part of the PRD-058 `SubmitRequest`
    /// contract; absent lets the provider apply its own default.
    pub service_tier: Option<String>,
}

impl Default for OpenAiResponsesConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            request_timeout_secs: 120,
            service_tier: None,
        }
    }
}

/// Per-response facts a host may need after the PRD-058 loop has already
/// moved on: the response-resolved model identity (the requested
/// identifier may be a moving alias, e.g. `gpt-5`), the service tier the
/// provider actually applied, and the provider response identity. The
/// first two have no field anywhere in the PRD-058 contract; the third
/// (`provider_request_id`) *is* part of `SubmitOutcome`, but the shared
/// `raw_runtime::run_loop`/`AttemptUsage` pairing does not carry it
/// forward past one iteration — this map is the only way a host can
/// recover it once the loop has returned, without any change to that
/// shared, adapter-neutral code. Recorded per attempt so accounting can
/// bind the exact identity that ran without ever freezing an alias into
/// canonical worker identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenAiResponseMeta {
    pub resolved_model: Option<String>,
    pub service_tier: Option<String>,
    pub provider_request_id: Option<String>,
}

/// One request-side canonical tool call, replayed as OpenAI's
/// `function_call` item ahead of its `function_call_output` — see the
/// module-level note on `call_cache`.
#[derive(Debug, Clone)]
struct CachedCall {
    name: String,
    arguments: String,
}

pub struct OpenAiResponsesClient {
    config: OpenAiResponsesConfig,
    client: Client,
    api_key: ApiKey,
    /// Replay cache for the model's own past function-call items. The
    /// Responses API requires the original `function_call` item to precede
    /// its `function_call_output` when input is resent from scratch — the
    /// PRD-058 loop resends full message history itself and only retains
    /// the tool *result*, not the call's `name`/`arguments`. This is a
    /// provider-specific request-shape accommodation kept entirely inside
    /// the adapter; the loop never sees it. Keyed by the model-issued
    /// `call_id`, which is unique within one execution.
    call_cache: Mutex<HashMap<String, CachedCall>>,
}

impl OpenAiResponsesClient {
    pub fn new(api_key: ApiKey, config: OpenAiResponsesConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|error| format!("failed to build OpenAI HTTP client: {error}"))?;
        Ok(Self {
            config,
            client,
            api_key,
            call_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Submits one Responses API request and streams its events to
    /// `observer`. Every call is exactly one HTTP request — no retry, no
    /// provider-side resumption — matching the PRD-058 attempt model.
    pub async fn submit(
        &self,
        request: &ResponsesRequest<'_>,
        observer: &mut dyn StreamObserver,
    ) -> Result<(SubmitOutcome, OpenAiResponseMeta), AdapterError> {
        let body = self.build_request_body(request);
        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.api_key.bearer_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(|seconds| seconds * 1000);
        // Unlike `send()` above, status and headers have already been
        // received here — the provider has accepted, executed, and billed
        // the request. A failure while reading the body is a mid-stream
        // cutoff, never a free retry.
        let text = response.text().await.map_err(|error| AdapterError::Ambiguous {
            reason: format!("OpenAI response body could not be read; provider completion is unknown: {error}"),
        })?;

        if !status.is_success() {
            return Err(map_http_error(status, &text, retry_after_ms));
        }

        self.consume_stream(&text, observer)
    }

    fn build_request_body(&self, request: &ResponsesRequest<'_>) -> Value {
        let mut body = json!({
            "model": request.model,
            "input": self.build_input_items(request.messages),
            "stream": true,
        });
        let map = body.as_object_mut().expect("body is always an object");

        if !request.tools.is_empty() {
            map.insert(
                "tools".into(),
                Value::Array(request.tools.iter().map(tool_definition_to_param).collect()),
            );
        }
        if let Some(structured) = request.structured_output {
            map.insert(
                "text".into(),
                json!({ "format": structured_output_to_format(structured) }),
            );
        }
        if let Some(reasoning) = request.reasoning_control {
            if let Some(effort) = reasoning_effort(reasoning) {
                map.insert("reasoning".into(), json!({ "effort": effort }));
            }
        }
        if let Some(cache_key) = request.prompt_cache_key {
            map.insert("prompt_cache_key".into(), json!(cache_key));
        }
        if let Some(tier) = &self.config.service_tier {
            map.insert("service_tier".into(), json!(tier));
        }
        body
    }

    /// Converts PRD-058 message history into Responses API input items,
    /// replaying a cached `function_call` item ahead of each
    /// `function_call_output` per the module-level note.
    fn build_input_items(&self, messages: &[Message]) -> Vec<Value> {
        let cache = self.call_cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = Vec::with_capacity(messages.len());
        let mut recorded_calls: std::collections::BTreeSet<String> = Default::default();
        for message in messages {
            match (&message.role, &message.content) {
                (MessageRole::System, MessageContent::Text(text)) => {
                    items.push(json!({
                        "type": "message",
                        "role": "system",
                        "content": [{"type": "input_text", "text": text}],
                    }));
                }
                (MessageRole::User, MessageContent::Text(text)) => {
                    items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    }));
                }
                (MessageRole::Assistant, MessageContent::Text(text)) => {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                // A transcript that RECORDS the assistant's calls serializes
                // them verbatim; the cache/fallback path below then has
                // nothing to reconstruct (FAM-BUG-049).
                (_, MessageContent::ToolCalls(calls)) => {
                    for call in calls {
                        items.push(json!({
                            "type": "function_call",
                            "call_id": call.call_id,
                            "name": call.capability_name,
                            "arguments": call.arguments,
                        }));
                        recorded_calls.insert(call.call_id.clone());
                    }
                }
                (MessageRole::Tool, MessageContent::ToolResult(result)) => {
                    // The Responses API rejects a function_call_output whose
                    // function_call is absent. The stream cache is the
                    // authority when present; a cache miss (fresh adapter,
                    // resumed transcript) falls back to the capability the
                    // result itself carries rather than omitting the item
                    // and sending a transcript the provider refuses.
                    match cache.get(&result.call_id) {
                        _ if recorded_calls.contains(&result.call_id) => {}
                        Some(cached) => items.push(json!({
                            "type": "function_call",
                            "call_id": result.call_id,
                            "name": cached.name,
                            "arguments": cached.arguments,
                        })),
                        None if !result.capability_name.is_empty() => items.push(json!({
                            "type": "function_call",
                            "call_id": result.call_id,
                            "name": result.capability_name,
                            "arguments": "{}",
                        })),
                        None => {}
                    }
                    let output = if result.is_error {
                        format!("ERROR: {}", result.content)
                    } else {
                        result.content.clone()
                    };
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": result.call_id,
                        "output": output,
                    }));
                }
                _ => {
                    // No other role/content pairing exists in the PRD-058
                    // contract; nothing to project.
                }
            }
        }
        items
    }

    /// Parses a complete SSE response body and replays its events to
    /// `observer` in order, returning the final outcome once a terminal
    /// event (`response.completed`/`response.incomplete`) arrives, or an
    /// error otherwise. A body with no terminal event at all (the
    /// connection ended mid-stream) is the honest `Ambiguous` case: the
    /// provider may have accepted and billed the request while its
    /// response never fully arrived.
    fn consume_stream(
        &self,
        body: &str,
        observer: &mut dyn StreamObserver,
    ) -> Result<(SubmitOutcome, OpenAiResponseMeta), AdapterError> {
        let mut item_call_ids: HashMap<String, String> = HashMap::new();
        let mut new_calls: HashMap<String, CachedCall> = HashMap::new();

        for event in parse_sse_events(body) {
            let Some(event_type) = event.get("type").and_then(Value::as_str) else {
                continue;
            };
            match event_type {
                "response.output_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        observer.on_event(StreamEvent::TextDelta(delta.to_string()));
                    }
                }
                "response.output_item.added" => {
                    if let Some(item) = event.get("item") {
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let call_id = item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let item_id = item
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or(&call_id)
                                .to_string();
                            item_call_ids.insert(item_id, call_id.clone());
                            observer.on_event(StreamEvent::ToolCallDelta {
                                call_id: call_id.clone(),
                                capability_id: name.clone(),
                                arguments_fragment: String::new(),
                            });
                            new_calls.insert(
                                call_id,
                                CachedCall {
                                    name,
                                    arguments: String::new(),
                                },
                            );
                        }
                    }
                }
                "response.function_call_arguments.delta" => {
                    let item_id = event.get("item_id").and_then(Value::as_str).unwrap_or("");
                    let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                    if let Some(call_id) = item_call_ids.get(item_id) {
                        if let Some(cached) = new_calls.get_mut(call_id) {
                            cached.arguments.push_str(delta);
                        }
                        observer.on_event(StreamEvent::ToolCallDelta {
                            call_id: call_id.clone(),
                            capability_id: String::new(),
                            arguments_fragment: delta.to_string(),
                        });
                    }
                }
                "response.function_call_arguments.done" | "response.output_item.done" => {
                    let item_id =
                        event
                            .get("item_id")
                            .and_then(Value::as_str)
                            .unwrap_or_else(|| {
                                event
                                    .get("item")
                                    .and_then(|item| item.get("id"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                            });
                    if let Some(call_id) = item_call_ids.get(item_id).cloned() {
                        observer.on_event(StreamEvent::ToolCallComplete {
                            call_id: call_id.clone(),
                        });
                    }
                }
                "response.completed" => {
                    if let Some(response) = event.get("response") {
                        return self.finish(response, observer, new_calls);
                    }
                }
                "response.incomplete" => {
                    if let Some(response) = event.get("response") {
                        return self.finish(response, observer, new_calls);
                    }
                }
                "response.failed" => {
                    if let Some(response) = event.get("response") {
                        return Err(map_failed_response(response));
                    }
                    return Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest));
                }
                "error" => {
                    return Err(map_error_event(&event));
                }
                _ => {
                    // A projection narrows the event surface it acts on; an
                    // event type this adapter does not yet handle (audio,
                    // MCP, code interpreter, ...) is neither text nor a
                    // tool call and is silently ignored, never invented.
                }
            }
        }

        // The body ended without a terminal event: the provider may have
        // accepted, executed, and billed the request whose response never
        // fully arrived. Usage stays ambiguous/pending for this attempt,
        // never zero.
        Err(AdapterError::Ambiguous {
            reason: "OpenAI Responses stream ended without a terminal event".into(),
        })
    }

    fn finish(
        &self,
        response: &Value,
        _observer: &mut dyn StreamObserver,
        new_calls: HashMap<String, CachedCall>,
    ) -> Result<(SubmitOutcome, OpenAiResponseMeta), AdapterError> {
        let status = response.get("status").and_then(Value::as_str).unwrap_or("");
        let has_function_calls = !new_calls.is_empty();

        let stop_reason = match status {
            "completed" => {
                if has_function_calls {
                    AdapterStopReason::ToolUse
                } else {
                    AdapterStopReason::EndTurn
                }
            }
            "incomplete" => {
                let reason = response
                    .get("incomplete_details")
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str);
                match reason {
                    Some("max_output_tokens") => AdapterStopReason::MaxTokens,
                    Some("content_filter") => AdapterStopReason::ContentFilter,
                    _ => {
                        return Err(AdapterError::Ambiguous {
                            reason: format!(
                                "OpenAI response incomplete with unrecognized reason {reason:?}"
                            ),
                        });
                    }
                }
            }
            other => {
                return Err(AdapterError::Ambiguous {
                    reason: format!("OpenAI response carried unrecognized status {other:?}"),
                });
            }
        };

        let usage = response.get("usage").map(parse_usage).unwrap_or_default();
        let provider_request_id = response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let resolved_model = response
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);
        let service_tier = response
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_string);

        // Only now that the turn is a confirmed success do the replayed
        // function-call items join the durable cache — a failed/ambiguous
        // attempt must not poison the next turn's replay with calls the
        // model may reissue differently.
        let mut cache = self.call_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.extend(new_calls);

        Ok((
            SubmitOutcome {
                stop_reason,
                usage,
                provider_request_id: provider_request_id.clone(),
                provider_idempotency_key: None,
            },
            OpenAiResponseMeta {
                resolved_model,
                service_tier,
                provider_request_id,
            },
        ))
    }
}

/// Uncached input is derived as `input_tokens - cached_tokens`, matching
/// the Chat Completions convention this API inherits (`cached_tokens` is a
/// subset already counted within `input_tokens`, never additional).
fn parse_usage(usage: &Value) -> UsageCategories {
    let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    let details = usage.get("input_tokens_details");
    let cached_tokens = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64);
    let cache_write_tokens = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(Value::as_u64);
    let uncached_input_tokens = match (input_tokens, cached_tokens) {
        (Some(total), Some(cached)) => Some(total.saturating_sub(cached)),
        (Some(total), None) => Some(total),
        (None, _) => None,
    };
    let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
    let reasoning_output_tokens = usage
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64);

    UsageCategories {
        uncached_input_tokens,
        cache_read_tokens: cached_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_output_tokens,
    }
}

fn tool_definition_to_param(tool: &ToolDefinition) -> Value {
    let (required, optional) = parse_pseudo_schema(&tool.json_schema);
    let mut properties = serde_json::Map::new();
    for field in required.iter().chain(optional.iter()) {
        properties.insert(field.clone(), json!({"type": "string"}));
    }
    json!({
        "type": "function",
        "name": tool.capability_id,
        "description": format!("Familiar canonical capability {} ({})", tool.capability_id, tool.schema_version),
        "parameters": {
            "type": "object",
            "properties": Value::Object(properties),
            "required": required,
            "additionalProperties": false,
        },
        "strict": false,
    })
}

/// `raw_runtime::offered_tool_definitions` serializes each capability's
/// closed field-presence schema as `{"required":[...],"optional":[...]}` —
/// a deliberately simplified internal representation, not real JSON
/// Schema (the PRD-058 contract's stated non-goal is a general JSON Schema
/// validator). This projects it into a minimal, valid OpenAI function
/// `parameters` schema: every declared field becomes a `string` property
/// (the canonical capability schemas validate presence, not type), so
/// OpenAI's structured tool-calling accepts the definition.
fn parse_pseudo_schema(raw: &str) -> (Vec<String>, Vec<String>) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (Vec::new(), Vec::new());
    };
    let as_strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    (as_strings("required"), as_strings("optional"))
}

fn structured_output_to_format(request: &StructuredOutputRequest) -> Value {
    let schema = serde_json::from_str::<Value>(&request.json_schema)
        .unwrap_or_else(|_| json!({"type": "object"}));
    json!({
        "type": "json_schema",
        "name": request.schema_name,
        "schema": schema,
        "strict": true,
    })
}

/// OpenAI's `reasoning.effort` accepts a documented closed set
/// (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max` as of 2026-09-01);
/// this adapter relays whatever the capability profile configures rather
/// than re-validating against a list that could drift ahead of this file
/// — an unrecognized value is the provider's `invalid_request_error` to
/// raise, surfaced through the ordinary error taxonomy. `budget_tokens` is
/// an Anthropic-shaped control this API has no equivalent for and is
/// intentionally not projected.
fn reasoning_effort(control: &ReasoningControl) -> Option<String> {
    control.effort.clone()
}

fn map_failed_response(response: &Value) -> AdapterError {
    let error = response.get("error");
    let code = error.and_then(|e| e.get("code")).and_then(Value::as_str);
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("OpenAI response failed")
        .to_string();
    match code {
        Some("content_filter") | Some("content_policy_violation") => {
            AdapterError::NonRetryable(NonRetryableKind::RefusedContent)
        }
        Some(code) if code.starts_with("server_error") || code == "internal_error" => {
            AdapterError::Retryable(RetryableKind::Overloaded)
        }
        _ => AdapterError::NonRetryable(NonRetryableKind::InvalidRequest),
    }
    .tap_message(message)
}

fn map_error_event(event: &Value) -> AdapterError {
    let code = event.get("code").and_then(Value::as_str);
    let message = event
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("OpenAI stream error")
        .to_string();
    match code {
        Some("rate_limit_exceeded") => AdapterError::Retryable(RetryableKind::RateLimited {
            retry_after_ms: None,
        }),
        Some("content_filter") | Some("content_policy_violation") => {
            AdapterError::NonRetryable(NonRetryableKind::RefusedContent)
        }
        _ => AdapterError::Ambiguous { reason: message },
    }
}

/// Attaches human-readable context to an [`AdapterError`] without changing
/// its taxonomy — the loop only branches on the variant, but daemon-side
/// diagnostics benefit from the provider's own message.
trait TapMessage {
    fn tap_message(self, message: String) -> Self;
}

impl TapMessage for AdapterError {
    fn tap_message(self, message: String) -> Self {
        match self {
            AdapterError::Ambiguous { .. } => AdapterError::Ambiguous { reason: message },
            other => other,
        }
    }
}

fn map_http_error(status: StatusCode, body: &str, retry_after_ms: Option<u64>) -> AdapterError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let code = error.and_then(|e| e.get("code")).and_then(Value::as_str);
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_string();

    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        }
        StatusCode::TOO_MANY_REQUESTS => {
            AdapterError::Retryable(RetryableKind::RateLimited { retry_after_ms })
        }
        StatusCode::BAD_REQUEST => {
            if code == Some("content_filter") || code == Some("content_policy_violation") {
                AdapterError::NonRetryable(NonRetryableKind::RefusedContent)
            } else {
                AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
            }
        }
        s if s.is_server_error() => AdapterError::Retryable(RetryableKind::Overloaded),
        _ => AdapterError::NonRetryable(NonRetryableKind::InvalidRequest),
    }
    .tap_message(message)
}

fn map_transport_error(error: reqwest::Error) -> AdapterError {
    if error.is_timeout() {
        // The request may have already reached the provider and been
        // billed; a client-side timeout waiting for a response is the
        // honest ambiguous case, never a free retry.
        AdapterError::Ambiguous {
            reason: "OpenAI request timed out; provider completion is unknown".into(),
        }
    } else {
        // Connection failures and other pre-response transport errors: the
        // request never reached the provider (or never got a response
        // started), so nothing was billed — safe to classify as retryable.
        AdapterError::Retryable(RetryableKind::TransientTransport)
    }
}

/// Splits a complete SSE body into its `data:` JSON payloads, in order.
/// Frames are separated by a blank line; a frame may carry a leading
/// `event:` line (ignored — every event's own `type` field is
/// authoritative) alongside one or more `data:` lines, which are
/// concatenated per the SSE spec before parsing.
fn parse_sse_events(body: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for frame in body.split("\n\n") {
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(chunk) = line.strip_prefix("data:") {
                data.push_str(chunk.trim_start());
            }
        }
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            events.push(value);
        }
    }
    events
}

pub struct ResponsesRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
    pub structured_output: Option<&'a StructuredOutputRequest>,
    pub reasoning_control: Option<&'a ReasoningControl>,
    pub prompt_cache_key: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::{MessageContent, MessageRole, ToolResultPayload};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Collector(Vec<StreamEvent>);
    impl StreamObserver for Collector {
        fn on_event(&mut self, event: StreamEvent) {
            self.0.push(event);
        }
    }

    fn client(server: &MockServer) -> OpenAiResponsesClient {
        OpenAiResponsesClient::new(
            ApiKey::new("sk-test"),
            OpenAiResponsesConfig {
                base_url: server.uri(),
                request_timeout_secs: 5,
                service_tier: None,
            },
        )
        .unwrap()
    }

    fn request<'a>(messages: &'a [Message]) -> ResponsesRequest<'a> {
        ResponsesRequest {
            model: "gpt-5",
            messages,
            tools: &[],
            structured_output: None,
            reasoning_control: None,
            prompt_cache_key: None,
        }
    }

    fn sse(frames: &[Value]) -> String {
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect()
    }

    #[tokio::test]
    async fn streams_text_delta_and_completes() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"type": "response.output_text.delta", "delta": "hello"}),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed", "model": "gpt-5-2026-08-01",
                "output": [], "usage": {
                    "input_tokens": 100, "input_tokens_details": {"cached_tokens": 20},
                    "output_tokens": 10, "output_tokens_details": {"reasoning_tokens": 0}
                }
            }}),
        ]);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, meta) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();

        assert_eq!(collector.0, vec![StreamEvent::TextDelta("hello".into())]);
        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);
        assert_eq!(outcome.usage.uncached_input_tokens, Some(80));
        assert_eq!(outcome.usage.cache_read_tokens, Some(20));
        assert_eq!(outcome.usage.output_tokens, Some(10));
        assert_eq!(outcome.provider_request_id, Some("resp_1".into()));
        assert_eq!(meta.resolved_model, Some("gpt-5-2026-08-01".into()));
    }

    #[tokio::test]
    async fn cache_write_tokens_recorded_distinctly_when_reported() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "output": [], "model": "gpt-5",
            "usage": {
                "input_tokens": 500,
                "input_tokens_details": {"cached_tokens": 100, "cache_write_tokens": 40},
                "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0}
            }
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.usage.cache_write_tokens, Some(40));
        assert_eq!(outcome.usage.uncached_input_tokens, Some(400));
    }

    #[tokio::test]
    async fn cache_write_absent_stays_unknown_not_fabricated() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "output": [], "model": "gpt-5",
            "usage": {
                "input_tokens": 500, "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0}
            }
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.usage.cache_write_tokens, None);
    }

    #[tokio::test]
    async fn reasoning_output_tokens_recorded_distinctly() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "output": [], "model": "gpt-5",
            "usage": {
                "input_tokens": 10, "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 60, "output_tokens_details": {"reasoning_tokens": 45}
            }
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.usage.reasoning_output_tokens, Some(45));
        assert_eq!(outcome.usage.output_tokens, Some(60));
    }

    #[tokio::test]
    async fn client_side_timeout_is_ambiguous_never_a_free_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_raw(
                        sse(&[completed("resp_1", "gpt-5", json!({}))]),
                        "text/event-stream",
                    ),
            )
            .mount(&server)
            .await;
        let openai = OpenAiResponsesClient::new(
            ApiKey::new("sk-test"),
            OpenAiResponsesConfig {
                base_url: server.uri(),
                request_timeout_secs: 1,
                service_tier: None,
            },
        )
        .unwrap();
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = openai
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AdapterError::Ambiguous {
                reason: "OpenAI request timed out; provider completion is unknown".into(),
            }
        );
    }

    #[tokio::test]
    async fn truncated_body_after_status_is_ambiguous_never_a_free_retry() {
        // A raw server (not wiremock, whose `Full<Bytes>` responses cannot
        // express an in-flight cutoff) sends valid status and headers —
        // the provider has accepted, executed, and billed the request —
        // then drops the connection with the declared `Content-Length`
        // unmet. The failure must surface from `response.text()`, not
        // `send()`, and must never be a free retry.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            // Drain the request so the client isn't blocked writing it.
            let _ = socket.read(&mut buf).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Type: text/event-stream\r\n\
                      Content-Length: 999999\r\n\
                      \r\n\
                      data: {\"type\":\"response.completed\"",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();
            drop(socket);
        });

        let openai = OpenAiResponsesClient::new(
            ApiKey::new("sk-test"),
            OpenAiResponsesConfig {
                base_url: format!("http://{addr}"),
                request_timeout_secs: 5,
                service_tier: None,
            },
        )
        .unwrap();
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = openai
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        match error {
            AdapterError::Ambiguous { reason } => {
                assert!(
                    reason.contains("provider completion is unknown"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    fn completed(id: &str, model: &str, usage: Value) -> Value {
        json!({"type": "response.completed", "response": {
            "id": id, "status": "completed", "model": model, "output": [], "usage": usage,
        }})
    }

    #[tokio::test]
    async fn tool_call_streams_and_completes_with_tool_use() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
            }}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{\"path\""}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": ":\"a.rs\"}"}),
            json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed", "model": "gpt-5", "output": [
                    {"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{\"path\":\"a.rs\"}"}
                ],
                "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0}}
            }}),
        ]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let messages = vec![Message::user("read it")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        assert!(collector.0.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallComplete { call_id } if call_id == "call_1"
        )));
    }

    #[tokio::test]
    async fn function_call_replay_precedes_tool_result_on_next_turn() {
        let server = MockServer::start().await;
        let first_body = sse(&[
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
            }}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{\"path\":\"a.rs\"}"}),
            json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed", "model": "gpt-5",
                "output": [{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{\"path\":\"a.rs\"}"}],
                "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
            }}),
        ]);
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(first_body, "text/event-stream"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let second_body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_2", "status": "completed", "model": "gpt-5", "output": [],
            "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(second_body, "text/event-stream"))
            .mount(&server)
            .await;

        let openai = client(&server);
        let mut collector = Collector(Vec::new());
        let first_messages = vec![Message::user("read it")];
        openai
            .submit(&request(&first_messages), &mut collector)
            .await
            .unwrap();

        let second_messages = vec![
            Message::user("read it"),
            Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(ToolResultPayload {
                    call_id: "call_1".into(),
                    capability_name: "read-file".into(),
                    content: "file contents".into(),
                    is_error: false,
                }),
            },
        ];
        let body = openai.build_request_body(&request(&second_messages));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[1]["name"], "read-file");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_1");
    }

    #[test]
    fn cache_miss_still_emits_a_named_function_call_from_the_result() {
        // FAM-BUG-046: a fresh adapter (or a resumed transcript) has no
        // streamed call cached; omitting the function_call item sends a
        // function_call_output the Responses API rejects.
        let openai = OpenAiResponsesClient::new(
            ApiKey::new("sk-test"),
            OpenAiResponsesConfig {
                base_url: "http://127.0.0.1:1".into(),
                request_timeout_secs: 1,
                service_tier: None,
            },
        )
        .unwrap();
        let messages = vec![
            Message::user("read it"),
            Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(ToolResultPayload {
                    call_id: "call_orphan".into(),
                    capability_name: "read-file".into(),
                    content: "contents".into(),
                    is_error: false,
                }),
            },
        ];
        let body = openai.build_request_body(&request(&messages));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "read-file");
        assert_eq!(input[1]["call_id"], "call_orphan");
        assert_eq!(input[2]["type"], "function_call_output");
    }

    #[tokio::test]
    async fn partial_stream_with_no_terminal_event_is_ambiguous() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.output_text.delta", "delta": "partial"})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(error, AdapterError::Ambiguous { .. }));
        assert_eq!(collector.0, vec![StreamEvent::TextDelta("partial".into())]);
    }

    #[tokio::test]
    async fn incomplete_max_output_tokens_maps_to_max_tokens_stop() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.incomplete", "response": {
            "id": "resp_1", "status": "incomplete", "model": "gpt-5", "output": [],
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 5, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 100, "output_tokens_details": {"reasoning_tokens": 0}}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::MaxTokens);
    }

    #[tokio::test]
    async fn incomplete_content_filter_never_reported_as_token_exhaustion() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.incomplete", "response": {
            "id": "resp_1", "status": "incomplete", "model": "gpt-5", "output": [],
            "incomplete_details": {"reason": "content_filter"},
            "usage": {"input_tokens": 5, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::ContentFilter);
        assert_ne!(outcome.stop_reason, AdapterStopReason::MaxTokens);
    }

    #[tokio::test]
    async fn malformed_tool_arguments_are_not_this_adapters_concern() {
        // Malformed/unparseable accumulated arguments are refused by the
        // PRD-058 loop's TurnCollector/validate_tool_call, not by this
        // adapter: the adapter's job is only to relay raw argument text
        // faithfully. This test proves the adapter relays exactly what the
        // provider sent, including invalid JSON, without editorializing.
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
            }}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{not valid json"}),
            json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed", "model": "gpt-5",
                "output": [{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{not valid json"}],
                "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
            }}),
        ]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        let fragments: String = collector
            .0
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta {
                    arguments_fragment, ..
                } => Some(arguments_fragment.clone()),
                _ => None,
            })
            .collect();
        assert!(fragments.contains("{not valid json"));
    }

    #[tokio::test]
    async fn rate_limited_is_retryable_with_retry_after_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "3")
                    .set_body_json(json!({
                        "error": {"code": "rate_limit_exceeded", "message": "slow down"}
                    })),
            )
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AdapterError::Retryable(RetryableKind::RateLimited {
                retry_after_ms: Some(3000)
            })
        );
    }

    #[tokio::test]
    async fn server_error_is_retryable_overloaded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "error": {"code": "server_error", "message": "try again"}
            })))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::Retryable(RetryableKind::Overloaded)
        ));
    }

    #[tokio::test]
    async fn auth_failure_is_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"code": "invalid_api_key", "message": "Incorrect API key provided"}
            })))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        ));
    }

    #[tokio::test]
    async fn invalid_request_is_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"code": "invalid_request_error", "message": "bad schema"}
            })))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
        ));
    }

    #[tokio::test]
    async fn content_policy_refusal_before_generation_is_refused_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"code": "content_policy_violation", "message": "blocked"}
            })))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::NonRetryable(NonRetryableKind::RefusedContent)
        ));
    }

    #[tokio::test]
    async fn response_failed_status_is_taxonomized_from_error_code() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.failed", "response": {
            "id": "resp_1", "status": "failed", "model": "gpt-5", "output": [],
            "error": {"code": "server_error", "message": "internal error"}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::Retryable(RetryableKind::Overloaded)
        ));
    }

    #[tokio::test]
    async fn stream_error_event_is_taxonomized() {
        let server = MockServer::start().await;
        let body = sse(&[json!({
            "type": "error", "code": "rate_limit_exceeded", "message": "slow down", "sequence_number": 1
        })]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::Retryable(RetryableKind::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn missing_usage_leaves_categories_unknown_not_zero() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "model": "gpt-5", "output": []
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert!(outcome.usage.is_entirely_unknown());
    }

    #[tokio::test]
    async fn resolved_model_can_drift_from_requested_alias() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "model": "gpt-5-2026-09-01",
            "output": [], "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let requested = request(&messages);
        assert_eq!(requested.model, "gpt-5");
        let (_, meta) = client(&server)
            .submit(&requested, &mut collector)
            .await
            .unwrap();
        assert_eq!(meta.resolved_model, Some("gpt-5-2026-09-01".into()));
        assert_ne!(meta.resolved_model.unwrap(), requested.model);
    }

    #[tokio::test]
    async fn service_tier_recorded_when_present() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "model": "gpt-5", "service_tier": "flex",
            "output": [], "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}}
        }})]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (_, meta) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(meta.service_tier, Some("flex".into()));
    }

    #[test]
    fn api_key_debug_is_redacted() {
        let key = ApiKey::new("sk-super-secret");
        assert_eq!(format!("{key:?}"), "ApiKey([REDACTED])");
    }

    #[test]
    fn structured_output_maps_to_json_schema_format() {
        let structured = StructuredOutputRequest {
            schema_name: "plan".into(),
            json_schema: r#"{"type":"object","properties":{"steps":{"type":"array"}}}"#.into(),
        };
        let format = structured_output_to_format(&structured);
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["name"], "plan");
        assert_eq!(format["schema"]["properties"]["steps"]["type"], "array");
    }

    #[test]
    fn tool_definition_projects_pseudo_schema_into_valid_json_schema() {
        let tool = ToolDefinition {
            capability_id: "read-file".into(),
            schema_version: "read-file/1".into(),
            json_schema: r#"{"required":["path"],"optional":[]}"#.into(),
        };
        let param = tool_definition_to_param(&tool);
        assert_eq!(param["type"], "function");
        assert_eq!(param["name"], "read-file");
        assert_eq!(param["parameters"]["required"], json!(["path"]));
        assert_eq!(param["parameters"]["properties"]["path"]["type"], "string");
    }
}
