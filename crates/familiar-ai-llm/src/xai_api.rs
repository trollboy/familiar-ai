//! PRD-061 xAI Grok raw-API `InferenceAdapter`. Targets
//! `https://api.x.ai/v1/chat/completions` — xAI's most thoroughly documented
//! streaming surface (SSE chunk shape, `data: [DONE]` termination, a
//! `usage` object with verified field names, and a per-request
//! `cost_in_usd_ticks` cost figure), verified against `docs.x.ai` on
//! 2026-09-01. `/v1/responses` is also OpenAI-Responses-shaped for
//! non-streaming calls, but its streaming SSE event names could not be
//! confirmed in the consulted documentation; building against them now
//! would be resemblance-based guessing, which this PRD forbids. See
//! `docs/contracts/xai-adapter.md` for the full verification record.
//!
//! xAI owns every piece of this file: its own auth resolution (`env: NAME`
//! BYO-Auth only — no OpenAI credential path, no OpenAI capability, price,
//! or error assumption). Nothing here is shared with, or owned by, an
//! OpenAI adapter; this module has no dependency on one and none is
//! required to build, test, enable, or disable it.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use familiar_ai_core::config::{AuthDescriptor, RegistryWorkerConfig};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::attempt::{
    AdapterError, AdapterStopReason, AttemptId, InferenceAdapter, Message, MessageContent,
    MessageRole, NonRetryableKind, RetryableKind, StreamEvent, StreamObserver, SubmitOutcome,
    SubmitRequest, UsageCategories,
};

pub const XAI_RUNTIME_ID: &str = "xai-api";
pub const XAI_DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

const BYO_AUTH_REMEDY: &str =
    "xAI requires a BYO-Auth `env: NAME` descriptor (e.g. `env: XAI_API_KEY`); configure it and export the named variable — a credential value is never accepted in configuration";

#[derive(Debug, Clone)]
pub struct XaiAdapterConfig {
    pub base_url: String,
    pub auth: AuthDescriptor,
    pub request_timeout_secs: u64,
}

impl XaiAdapterConfig {
    pub fn new(auth: AuthDescriptor) -> Self {
        Self {
            base_url: XAI_DEFAULT_BASE_URL.to_owned(),
            auth,
            request_timeout_secs: 300,
        }
    }
}

/// Why a [`RegistryWorkerConfig`] could not be turned into a running
/// [`XaiAdapter`]. Every variant is a configuration problem an operator can
/// fix — never a wire/runtime failure, which surfaces through
/// [`AdapterError`] from `submit` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XaiAdapterBuildError {
    /// The worker's `runtime` (or legacy `adapter`) is not `xai-api`.
    WrongRuntime(String),
    /// An `xai-api` worker was configured with no `auth_profile`.
    MissingAuthProfile,
    /// `auth_profile` names an entry absent from `[auth_profiles.*]`.
    UnknownAuthProfile(String),
    /// The adapter's own HTTP client failed to build.
    ClientInit(String),
}

impl std::fmt::Display for XaiAdapterBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongRuntime(runtime) => {
                write!(f, "worker runtime '{runtime}' is not '{XAI_RUNTIME_ID}'")
            }
            Self::MissingAuthProfile => {
                write!(f, "an {XAI_RUNTIME_ID} worker requires auth_profile")
            }
            Self::UnknownAuthProfile(profile) => write!(
                f,
                "auth profile '{profile}' is missing; configure [auth_profiles.{profile}] with a BYO-Auth descriptor"
            ),
            Self::ClientInit(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for XaiAdapterBuildError {}

/// Builds a real, running [`XaiAdapter`] from an operator's
/// `[worker_registry.workers.*]` entry (`runtime = "xai-api"`) and the
/// top-level `[auth_profiles.*]` table — the link that makes a configured
/// Grok worker reachable, rather than an `XaiAdapter` only ever being
/// constructed by hand in a test. `auth_profile` is resolved to an
/// [`AuthDescriptor`] here, at the config boundary; the credential value
/// itself is still resolved fresh from that descriptor's source on every
/// `submit`, per [`XaiAdapter::resolve_api_key`].
pub fn build_xai_adapter_from_config(
    worker: &RegistryWorkerConfig,
    auth_profiles: &BTreeMap<String, AuthDescriptor>,
) -> Result<XaiAdapter, XaiAdapterBuildError> {
    let runtime = worker
        .runtime_id()
        .map_err(XaiAdapterBuildError::WrongRuntime)?;
    if runtime != XAI_RUNTIME_ID {
        return Err(XaiAdapterBuildError::WrongRuntime(runtime.to_owned()));
    }
    let profile_name = worker
        .auth_profile
        .as_deref()
        .ok_or(XaiAdapterBuildError::MissingAuthProfile)?;
    let auth = auth_profiles
        .get(profile_name)
        .cloned()
        .ok_or_else(|| XaiAdapterBuildError::UnknownAuthProfile(profile_name.to_owned()))?;
    XaiAdapter::new(XaiAdapterConfig::new(auth)).map_err(XaiAdapterBuildError::ClientInit)
}

/// xAI's raw-API adapter. Holds only the external credential *reference*
/// (an [`AuthDescriptor`]) — the credential value itself is resolved fresh
/// from its source on every `submit`, never cached across calls and never
/// written to configuration, a prompt, a tool, an accounting row, or a log.
pub struct XaiAdapter {
    config: XaiAdapterConfig,
    client: Client,
    cancelled: Mutex<Vec<AttemptId>>,
    /// The most recently observed provider-resolved model identity (the
    /// response's own `model` field), captured separately from the
    /// requested alias in [`SubmitRequest::model`] so a moving alias is
    /// never frozen into canonical worker identity. Diagnostic only.
    last_resolved_model: Mutex<Option<String>>,
    /// The most recently observed exact vendor cost, in xAI's own "ticks"
    /// unit (10,000,000,000 ticks = 1 USD, per `docs.x.ai`), preserved
    /// losslessly rather than pre-converted so a later reconciliation stage
    /// can interpret it precisely. `None` when the provider did not report
    /// one for that attempt.
    last_cost_usd_ticks: Mutex<Option<u64>>,
}

impl XaiAdapter {
    pub fn new(config: XaiAdapterConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|error| format!("failed to build xAI HTTP client: {error}"))?;
        Ok(Self {
            config,
            client,
            cancelled: Mutex::new(Vec::new()),
            last_resolved_model: Mutex::new(None),
            last_cost_usd_ticks: Mutex::new(None),
        })
    }

    pub fn cancelled_attempts(&self) -> Vec<AttemptId> {
        self.cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// The provider-resolved model identity xAI actually served on the last
    /// `submit`, if it differed from — or simply confirmed — the requested
    /// alias. Never used as canonical worker identity; a caller wiring this
    /// adapter into PRD-057 spec identity keeps the *configured* alias
    /// canonical and treats this purely as observational telemetry.
    pub fn last_resolved_model(&self) -> Option<String> {
        self.last_resolved_model
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// The exact `cost_in_usd_ticks` xAI reported for the last attempt, if
    /// any. This is a *per-request* vendor-reported figure, distinct from —
    /// and not a substitute for — an authoritative organization billing or
    /// administrative cost API, which xAI does not expose today.
    pub fn last_cost_usd_ticks(&self) -> Option<u64> {
        *self
            .last_cost_usd_ticks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn resolve_api_key(&self) -> Result<String, AdapterError> {
        match &self.config.auth {
            AuthDescriptor::Env(name) => std::env::var(name).map_err(|_| {
                tracing::warn!(target: "xai_api", env_name = %name, "{BYO_AUTH_REMEDY}");
                AdapterError::NonRetryable(NonRetryableKind::Auth)
            }),
            _ => {
                tracing::warn!(target: "xai_api", "{BYO_AUTH_REMEDY}");
                Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
            }
        }
    }
}

// ---------------------------------------------------------------------
// Wire request shapes (chat/completions, OpenAI-SDK-compatible per
// docs.x.ai; xAI-owned defaults and extensions only — no OpenAI semantics).
// ---------------------------------------------------------------------

fn wire_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn wire_message(message: &Message) -> Value {
    match &message.content {
        MessageContent::Text(text) => json!({
            "role": wire_role(message.role),
            "content": text,
        }),
        MessageContent::ToolResult(result) => json!({
            "role": "tool",
            "tool_call_id": result.call_id,
            "content": result.content,
        }),
    }
}

/// Builds the wire `messages` array, inserting the assistant `tool_calls`
/// message the `/v1/chat/completions` surface requires directly before each
/// run of `role: "tool"` messages. `crate::attempt::MessageContent` has no
/// variant carrying an assistant-issued tool call's name/arguments alongside
/// its `ToolResultPayload` (only `call_id` round-trips from the loop that
/// built this transcript), so the reconstructed `tool_calls` entries carry
/// ids only — that is what the wire format actually requires: every
/// `tool_call_id` must answer a directly preceding assistant `tool_calls`
/// entry with a matching id.
fn wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut wire = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        let MessageContent::ToolResult(_) = &message.content else {
            wire.push(wire_message(message));
            index += 1;
            continue;
        };

        let run_start = index;
        let mut run_end = index;
        while let Some(Message {
            content: MessageContent::ToolResult(result),
            ..
        }) = messages.get(run_end)
        {
            let _ = result;
            run_end += 1;
        }
        let tool_calls: Vec<Value> = messages[run_start..run_end]
            .iter()
            .map(|message| match &message.content {
                // The name is the capability the result itself carries; an
                // empty name is a transcript xAI rejects (FAM-BUG-046).
                MessageContent::ToolResult(result) => json!({
                    "id": result.call_id,
                    "type": "function",
                    "function": {
                        "name": result.capability_name,
                        "arguments": "{}",
                    },
                }),
                MessageContent::Text(_) => unreachable!("run contains only ToolResult messages"),
            })
            .collect();

        let merges_into_previous = run_start > 0
            && messages[run_start - 1].role == MessageRole::Assistant
            && matches!(messages[run_start - 1].content, MessageContent::Text(_));
        if merges_into_previous {
            if let Some(previous) = wire.last_mut() {
                previous["tool_calls"] = Value::Array(tool_calls);
            }
        } else {
            wire.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": tool_calls,
            }));
        }

        for result_message in &messages[run_start..run_end] {
            wire.push(wire_message(result_message));
        }
        index = run_end;
    }
    wire
}

fn wire_tool(tool: &crate::attempt::ToolDefinition) -> Value {
    let parameters: Value =
        serde_json::from_str(&tool.json_schema).unwrap_or_else(|_| json!({"type": "object"}));
    json!({
        "type": "function",
        "function": {
            "name": tool.capability_id,
            "parameters": parameters,
        },
    })
}

fn build_request_body(request: &SubmitRequest) -> Value {
    let mut body = json!({
        "model": request.model,
        "messages": wire_messages(&request.messages),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(wire_tool).collect());
    }
    if let Some(structured) = &request.structured_output {
        // Probed, not documentation-verified: xAI's overview page documents
        // structured outputs at a high level only. If this shape is wrong
        // for a given deployment, xAI returns 400, which this adapter
        // reports as `NonRetryable(InvalidRequest)` rather than silently
        // degrading to unstructured text.
        let schema: Value = serde_json::from_str(&structured.json_schema)
            .unwrap_or_else(|_| json!({"type": "object"}));
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": structured.schema_name,
                "schema": schema,
            },
        });
    }
    body
}

// ---------------------------------------------------------------------
// Wire response shapes
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WireChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Debug, Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireFunction>,
}

#[derive(Debug, Deserialize)]
struct WireFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Verified 2026-09-01 against `docs.x.ai`'s API reference. Fields not
/// present here (e.g. any cache-*write*-token count) are not documented by
/// xAI anywhere consulted and must stay unknown, never zero.
#[derive(Debug, Deserialize)]
struct WireUsage {
    // Kept for documentation completeness of the verified wire shape; not
    // used for mapping (a bare total could include cached tokens, so only
    // the `prompt_tokens_details` breakdown below is trusted — see
    // `map_usage`).
    #[allow(dead_code)]
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<WirePromptDetails>,
    #[serde(default)]
    completion_tokens_details: Option<WireCompletionDetails>,
    #[serde(default)]
    cost_in_usd_ticks: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WirePromptDetails {
    #[serde(default)]
    text_tokens: Option<u64>,
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WireCompletionDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

fn map_usage(wire: &WireUsage) -> UsageCategories {
    let (output_tokens, reasoning_output_tokens) = match (
        wire.completion_tokens,
        wire.completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens),
    ) {
        (Some(total), Some(reasoning)) => (Some(total.saturating_sub(reasoning)), Some(reasoning)),
        (Some(total), None) => (Some(total), None),
        (None, _) => (None, None),
    };
    UsageCategories {
        // Only the documented `prompt_tokens_details.text_tokens` breakdown
        // is trusted as "uncached"; a bare `prompt_tokens` total with no
        // breakdown could include cached tokens and stays unknown rather
        // than being guessed.
        uncached_input_tokens: wire
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.text_tokens),
        cache_read_tokens: wire
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens),
        cache_write_tokens: None,
        output_tokens,
        reasoning_output_tokens,
    }
}

fn map_finish_reason(finish_reason: &str) -> AdapterStopReason {
    match finish_reason {
        "tool_calls" => AdapterStopReason::ToolUse,
        "length" => AdapterStopReason::MaxTokens,
        "content_filter" => AdapterStopReason::ContentFilter,
        // "stop" and any other value xAI might introduce: the honest
        // closed-set fallback is a normal end of turn.
        _ => AdapterStopReason::EndTurn,
    }
}

fn map_status_error(status: StatusCode, retry_after_ms: Option<u64>) -> AdapterError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        }
        StatusCode::TOO_MANY_REQUESTS => {
            AdapterError::Retryable(RetryableKind::RateLimited { retry_after_ms })
        }
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => {
            AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
        }
        status if status.is_server_error() => AdapterError::Retryable(RetryableKind::Overloaded),
        _ => AdapterError::Retryable(RetryableKind::TransientTransport),
    }
}

fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds * 1_000)
}

#[async_trait]
impl InferenceAdapter for XaiAdapter {
    fn runtime_id(&self) -> &str {
        XAI_RUNTIME_ID
    }

    async fn submit(
        &self,
        request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError> {
        let api_key = self.resolve_api_key()?;
        let url = format!("{}/chat/completions", self.config.base_url);
        let body = build_request_body(request);

        let mut response = self
            .client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
            // Nothing has been billed if the request never reached the
            // provider (connect failure, DNS, TLS, or a timeout before any
            // response arrived) — safe to classify as retryable transport,
            // never ambiguous.
            .map_err(|_| AdapterError::Retryable(RetryableKind::TransientTransport))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = retry_after_ms(response.headers());
            return Err(map_status_error(status, retry_after));
        }

        let mut buffer = String::new();
        let mut provider_request_id: Option<String> = None;
        let mut usage = UsageCategories::default();
        let mut stop_reason: Option<AdapterStopReason> = None;
        let mut saw_any_chunk = false;

        loop {
            let next = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    AdapterError::Ambiguous {
                        reason: "xAI stream timed out mid-response; completion status unknown"
                            .into(),
                    }
                } else {
                    AdapterError::Ambiguous {
                        reason: format!("xAI stream ended with a transport error: {error}"),
                    }
                }
            })?;
            let Some(chunk) = next else {
                // Connection closed without an explicit `[DONE]` sentinel:
                // the response is incomplete and its true completion state
                // is unknown, never assumed successful.
                if stop_reason.is_none() {
                    return Err(AdapterError::Ambiguous {
                        reason: "xAI stream closed before a `[DONE]` sentinel or finish_reason was observed".into(),
                    });
                }
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline_at) = buffer.find('\n') {
                let line = buffer[..newline_at].trim_end_matches('\r').to_owned();
                buffer.drain(..=newline_at);
                let Some(data) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    let stop_reason = stop_reason.unwrap_or(AdapterStopReason::EndTurn);
                    return Ok(finalize(stop_reason, usage, provider_request_id));
                }
                saw_any_chunk = true;
                let parsed: WireChunk = match serde_json::from_str(data) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        // A malformed event body from the wire is a
                        // provider-side protocol failure, not tool-argument
                        // malformation (that case is forwarded verbatim
                        // below): the attempt's completion state becomes
                        // ambiguous rather than silently skipped.
                        return Err(AdapterError::Ambiguous {
                            reason: "xAI stream sent a non-JSON event body".into(),
                        });
                    }
                };
                if provider_request_id.is_none() {
                    provider_request_id = parsed.id.clone();
                }
                if let Some(model) = &parsed.model {
                    *self
                        .last_resolved_model
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(model.clone());
                }
                for choice in &parsed.choices {
                    if let Some(text) = &choice.delta.content {
                        if !text.is_empty() {
                            observer.on_event(StreamEvent::TextDelta(text.clone()));
                        }
                    }
                    // xAI delivers a function call whole in a single chunk,
                    // not argument-streamed across multiple deltas — a
                    // verified capability difference from delta-streamed
                    // providers. Each tool call in this chunk is therefore
                    // complete on arrival: emit the delta and its
                    // completion together rather than accumulating
                    // fragments across chunks.
                    for tool_call in &choice.delta.tool_calls {
                        let call_id = tool_call.id.clone().unwrap_or_default();
                        let (capability_id, arguments_fragment) = match &tool_call.function {
                            Some(function) => (
                                function.name.clone().unwrap_or_default(),
                                function.arguments.clone().unwrap_or_default(),
                            ),
                            None => (String::new(), String::new()),
                        };
                        observer.on_event(StreamEvent::ToolCallDelta {
                            call_id: call_id.clone(),
                            capability_id,
                            arguments_fragment,
                        });
                        observer.on_event(StreamEvent::ToolCallComplete { call_id });
                    }
                    if let Some(finish_reason) = &choice.finish_reason {
                        stop_reason = Some(map_finish_reason(finish_reason));
                    }
                }
                if let Some(wire_usage) = &parsed.usage {
                    usage = map_usage(wire_usage);
                    observer.on_event(StreamEvent::UsageDelta(usage));
                    *self
                        .last_cost_usd_ticks
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = wire_usage.cost_in_usd_ticks;
                }
            }
        }

        if !saw_any_chunk {
            return Err(AdapterError::Ambiguous {
                reason: "xAI stream closed with no data before completion".into(),
            });
        }
        Ok(finalize(
            stop_reason.unwrap_or(AdapterStopReason::EndTurn),
            usage,
            provider_request_id,
        ))
    }

    fn cancel(&self, attempt_id: &AttemptId) {
        self.cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(attempt_id.clone());
    }
}

fn finalize(
    stop_reason: AdapterStopReason,
    usage: UsageCategories,
    provider_request_id: Option<String>,
) -> SubmitOutcome {
    SubmitOutcome {
        stop_reason,
        usage,
        provider_request_id,
        // No officially documented request-level idempotency guarantee was
        // found in the consulted xAI documentation; never fabricated.
        provider_idempotency_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::{CacheControl, ToolDefinition};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn adapter_for(server: &MockServer) -> XaiAdapter {
        std::env::set_var("XAI_TEST_KEY", "sk-test-key");
        XaiAdapter::new(XaiAdapterConfig {
            base_url: server.uri(),
            auth: AuthDescriptor::Env("XAI_TEST_KEY".into()),
            request_timeout_secs: 5,
        })
        .unwrap()
    }

    fn base_request(attempt: &str) -> SubmitRequest {
        SubmitRequest {
            attempt_id: AttemptId(attempt.into()),
            messages: vec![Message::user("hi")],
            model: "grok-4".into(),
            tools: vec![],
            structured_output: None,
            cache_control: CacheControl::None,
            reasoning_control: None,
            prompt_cache_key: None,
        }
    }

    struct Collector(Vec<StreamEvent>);
    impl StreamObserver for Collector {
        fn on_event(&mut self, event: StreamEvent) {
            self.0.push(event);
        }
    }

    fn sse_body(lines: &[&str]) -> String {
        let mut body = String::new();
        for line in lines {
            body.push_str("data: ");
            body.push_str(line);
            body.push_str("\n\n");
        }
        body
    }

    #[tokio::test]
    async fn text_only_stream_reports_end_turn_and_verified_usage_fields() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_1","model":"grok-4-0709","choices":[{"index":0,"delta":{"content":"hel"}}]}"#,
            r#"{"id":"req_1","model":"grok-4-0709","choices":[{"index":0,"delta":{"content":"lo"}}]}"#,
            r#"{"id":"req_1","model":"grok-4-0709","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"id":"req_1","model":"grok-4-0709","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"text_tokens":10,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0},"cost_in_usd_ticks":123456}}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let outcome = adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);
        assert_eq!(outcome.usage.uncached_input_tokens, Some(10));
        assert_eq!(outcome.usage.cache_read_tokens, Some(0));
        assert_eq!(outcome.usage.output_tokens, Some(2));
        assert_eq!(outcome.usage.reasoning_output_tokens, Some(0));
        assert_eq!(outcome.provider_request_id, Some("req_1".into()));
        assert_eq!(outcome.provider_idempotency_key, None);
        assert_eq!(adapter.last_cost_usd_ticks(), Some(123_456));
        let text: String = collector
            .0
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn tool_call_arrives_whole_in_a_single_chunk() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_2","model":"grok-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read-file","arguments":"{\"path\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let outcome = adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        // Whole-chunk delivery: the delta and its completion arrive
        // together, never split across separate submit() turns.
        assert_eq!(collector.0.len(), 2);
        assert_eq!(
            collector.0[0],
            StreamEvent::ToolCallDelta {
                call_id: "call_abc".into(),
                capability_id: "read-file".into(),
                arguments_fragment: "{\"path\":\"a.txt\"}".into(),
            }
        );
        assert_eq!(
            collector.0[1],
            StreamEvent::ToolCallComplete {
                call_id: "call_abc".into(),
            }
        );
    }

    #[tokio::test]
    async fn parallel_tool_calls_in_one_chunk_are_each_whole() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_3","model":"grok-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read-file","arguments":"{}"}},{"index":1,"id":"call_2","function":{"name":"search-list","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();
        assert_eq!(collector.0.len(), 4);
    }

    #[tokio::test]
    async fn malformed_tool_call_arguments_are_forwarded_verbatim_not_rejected() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_4","model":"grok-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_bad","function":{"name":"apply-edit","arguments":"{not valid json"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let outcome = adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        assert!(matches!(
            &collector.0[0],
            StreamEvent::ToolCallDelta { arguments_fragment, .. } if arguments_fragment == "{not valid json"
        ));
    }

    #[tokio::test]
    async fn partial_stream_with_no_done_sentinel_is_ambiguous_not_a_success() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_5","model":"grok-4","choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(result, Err(AdapterError::Ambiguous { .. })));
    }

    #[tokio::test]
    async fn nonzero_reasoning_tokens_are_split_from_output_and_reported_separately() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_r","model":"grok-4","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            r#"{"id":"req_r","model":"grok-4","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":50,"prompt_tokens_details":{"text_tokens":20,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":35},"cost_in_usd_ticks":987654}}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let outcome = adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();

        // 50 total completion tokens, 35 of them reasoning: the non-reasoning
        // remainder (15) lands in output_tokens, the reasoning count is
        // reported separately rather than folded into either bucket or
        // dropped.
        assert_eq!(outcome.usage.output_tokens, Some(15));
        assert_eq!(outcome.usage.reasoning_output_tokens, Some(35));
    }

    #[tokio::test]
    async fn missing_usage_stays_unknown_not_zero() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_6","model":"grok-4","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let outcome = adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();
        assert!(outcome.usage.is_entirely_unknown());
    }

    #[tokio::test]
    async fn alias_drift_exposes_both_requested_and_resolved_identity() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_7","model":"grok-4-0709","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let request = base_request("att_1");
        assert_eq!(request.model, "grok-4");
        adapter.submit(&request, &mut collector).await.unwrap();
        // The requested alias never changes; the resolved identity is
        // exposed separately and is never written back into canonical
        // worker identity by this adapter.
        assert_eq!(request.model, "grok-4");
        assert_eq!(adapter.last_resolved_model(), Some("grok-4-0709".into()));
    }

    #[tokio::test]
    async fn missing_env_var_fails_closed_with_the_byo_auth_remedy() {
        let server = MockServer::start().await;
        std::env::remove_var("XAI_MISSING_KEY_TEST");
        let adapter = XaiAdapter::new(XaiAdapterConfig {
            base_url: server.uri(),
            auth: AuthDescriptor::Env("XAI_MISSING_KEY_TEST".into()),
            request_timeout_secs: 5,
        })
        .unwrap();
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
    }

    #[tokio::test]
    async fn non_env_auth_descriptor_fails_closed() {
        let server = MockServer::start().await;
        let adapter = XaiAdapter::new(XaiAdapterConfig {
            base_url: server.uri(),
            auth: AuthDescriptor::None,
            request_timeout_secs: 5,
        })
        .unwrap();
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
    }

    #[tokio::test]
    async fn http_401_fails_closed_as_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
    }

    #[tokio::test]
    async fn http_429_is_retryable_with_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::Retryable(RetryableKind::RateLimited {
                retry_after_ms: Some(2000)
            }))
        ));
    }

    #[tokio::test]
    async fn http_500_is_retryable_overloaded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::Retryable(RetryableKind::Overloaded))
        ));
    }

    #[tokio::test]
    async fn http_400_is_non_retryable_invalid_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        let result = adapter.submit(&base_request("att_1"), &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest))
        ));
    }

    #[tokio::test]
    async fn every_submission_is_its_own_http_attempt_no_dedup() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_x","model":"grok-4","choices":[{"index":0,"delta":{"content":"x"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.clone(), "text/event-stream"),
            )
            .expect(2)
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();
        adapter
            .submit(&base_request("att_2"), &mut collector)
            .await
            .unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn bearer_token_header_is_sent_and_never_the_raw_env_name() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"id":"req_y","model":"grok-4","choices":[{"index":0,"delta":{"content":"x"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer sk-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let adapter = adapter_for(&server);
        let mut collector = Collector(Vec::new());
        adapter
            .submit(&base_request("att_1"), &mut collector)
            .await
            .unwrap();
    }

    #[test]
    fn cancel_is_recorded_for_diagnostics_only() {
        let adapter =
            XaiAdapter::new(XaiAdapterConfig::new(AuthDescriptor::Env("X".into()))).unwrap();
        adapter.cancel(&AttemptId("att_1".into()));
        assert_eq!(
            adapter.cancelled_attempts(),
            vec![AttemptId("att_1".into())]
        );
    }

    #[test]
    fn tool_definition_translates_capability_id_to_function_name() {
        let tool = ToolDefinition {
            capability_id: "apply-edit".into(),
            schema_version: "1".into(),
            json_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
        };
        let wire = wire_tool(&tool);
        assert_eq!(wire["function"]["name"], "apply-edit");
        assert_eq!(wire["function"]["parameters"]["type"], "object");
    }

    // -------------------------------------------------------------------
    // wire_messages: proves a `role: "tool"` message is never serialized
    // without a directly preceding assistant `tool_calls` entry naming its
    // `tool_call_id` — the wire requirement `/v1/chat/completions` enforces
    // for every second-and-later turn of a tool-using loop.
    // -------------------------------------------------------------------

    fn tool_result(call_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: MessageContent::ToolResult(crate::attempt::ToolResultPayload {
                call_id: call_id.into(),
                capability_name: "read-file".into(),
                content: content.into(),
                is_error: false,
            }),
        }
    }

    #[test]
    fn tool_result_run_gets_a_preceding_assistant_tool_calls_message() {
        let messages = vec![Message::user("do it"), tool_result("call_abc", "ok")];
        let wire = wire_messages(&messages);

        assert_eq!(wire.len(), 3);
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[1]["role"], "assistant");
        assert_eq!(wire[1]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(wire[1]["tool_calls"][0]["type"], "function");
        // FAM-BUG-046: an empty function name is a transcript xAI rejects.
        assert_eq!(wire[1]["tool_calls"][0]["function"]["name"], "read-file");
        assert_eq!(wire[2]["role"], "tool");
        assert_eq!(wire[2]["tool_call_id"], "call_abc");
    }

    #[test]
    fn parallel_tool_results_share_one_preceding_assistant_message_with_all_ids() {
        let messages = vec![
            Message::user("do it"),
            tool_result("call_1", "ok"),
            tool_result("call_2", "ok"),
        ];
        let wire = wire_messages(&messages);

        assert_eq!(wire.len(), 4);
        assert_eq!(wire[1]["role"], "assistant");
        let ids: Vec<&str> = wire[1]["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["call_1", "call_2"]);
        assert_eq!(wire[2]["tool_call_id"], "call_1");
        assert_eq!(wire[3]["tool_call_id"], "call_2");
    }

    #[test]
    fn assistant_text_turn_merges_with_the_synthesized_tool_calls_entry() {
        let messages = vec![
            Message::user("do it"),
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text("on it".into()),
            },
            tool_result("call_abc", "ok"),
        ];
        let wire = wire_messages(&messages);

        // The assistant's text turn and its tool call are one message on
        // the wire, not two consecutive assistant messages.
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[1]["role"], "assistant");
        assert_eq!(wire[1]["content"], "on it");
        assert_eq!(wire[1]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(wire[2]["role"], "tool");
    }

    #[test]
    fn multi_turn_transcript_never_leaves_a_tool_message_without_its_precursor() {
        let messages = vec![
            Message::user("do it"),
            tool_result("call_1", "first"),
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text("done".into()),
            },
            tool_result("call_2", "second"),
        ];
        let wire = wire_messages(&messages);

        for (index, message) in wire.iter().enumerate() {
            if message["role"] == "tool" {
                let preceding = &wire[index - 1];
                assert_eq!(preceding["role"], "assistant");
                let ids: Vec<&str> = preceding["tool_calls"]
                    .as_array()
                    .expect("preceding assistant message must carry tool_calls")
                    .iter()
                    .map(|call| call["id"].as_str().unwrap())
                    .collect();
                assert!(ids.contains(&message["tool_call_id"].as_str().unwrap()));
            }
        }
    }

    #[test]
    fn runtime_id_is_xai_api_never_openai() {
        let adapter =
            XaiAdapter::new(XaiAdapterConfig::new(AuthDescriptor::Env("X".into()))).unwrap();
        assert_eq!(adapter.runtime_id(), "xai-api");
    }

    // -------------------------------------------------------------------
    // build_xai_adapter_from_config: proves an XaiAdapter is reachable from
    // an operator's `[worker_registry.workers.*]` / `[auth_profiles.*]`
    // configuration types, not only from a hand-built XaiAdapterConfig.
    // -------------------------------------------------------------------

    fn worker_config(runtime: &str, auth_profile: Option<&str>) -> RegistryWorkerConfig {
        RegistryWorkerConfig {
            adapter: None,
            provider: "xai".into(),
            model: "grok-4".into(),
            runtime: Some(runtime.to_owned()),
            model_artifact: None,
            auth_profile: auth_profile.map(str::to_owned),
            capability_profile: None,
            runtime_config: None,
            executable: None,
            capabilities: vec![],
            fresh_process_isolation: false,
            context_tokens: 0,
            estimated_cost_microusd: 0,
            available: true,
            effort: None,
            permission_mode: None,
            extra_args: vec![],
        }
    }

    #[test]
    fn xai_worker_is_constructible_starting_from_registry_configuration() {
        let worker = worker_config(XAI_RUNTIME_ID, Some("xai_main"));
        let auth_profiles = BTreeMap::from([(
            "xai_main".to_owned(),
            AuthDescriptor::Env("XAI_API_KEY".into()),
        )]);

        let adapter = build_xai_adapter_from_config(&worker, &auth_profiles).unwrap();
        assert_eq!(adapter.runtime_id(), XAI_RUNTIME_ID);
    }

    #[test]
    fn non_xai_runtime_worker_is_rejected_not_silently_adapted() {
        let worker = worker_config("codex", Some("xai_main"));
        let auth_profiles = BTreeMap::from([(
            "xai_main".to_owned(),
            AuthDescriptor::Env("XAI_API_KEY".into()),
        )]);

        let error = build_xai_adapter_from_config(&worker, &auth_profiles)
            .err()
            .unwrap();
        assert_eq!(error, XaiAdapterBuildError::WrongRuntime("codex".into()));
    }

    #[test]
    fn xai_worker_with_no_auth_profile_is_rejected() {
        let worker = worker_config(XAI_RUNTIME_ID, None);
        let error = build_xai_adapter_from_config(&worker, &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(error, XaiAdapterBuildError::MissingAuthProfile);
    }

    #[test]
    fn xai_worker_naming_an_unconfigured_auth_profile_is_rejected() {
        let worker = worker_config(XAI_RUNTIME_ID, Some("does_not_exist"));
        let error = build_xai_adapter_from_config(&worker, &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(
            error,
            XaiAdapterBuildError::UnknownAuthProfile("does_not_exist".into())
        );
    }
}
