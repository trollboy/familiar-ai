//! PRD-063 local worker transport: an OpenAI-compatible Chat Completions
//! client shared by every externally managed local runtime (`ollama`,
//! `unsloth`). This is transport reuse only, never a claim of OpenAI
//! behavior, capabilities, pricing, or semantics — each runtime keeps its
//! own [`LocalRuntimeKind`], capability profile, and empirical identity
//! (`docs/contracts/local-worker-runtime.md`).
//!
//! This module owns only the wire protocol and HTTP transport. The PRD-058
//! [`crate::attempt::InferenceAdapter`] projection lives in
//! `familiar_ai_agent::local_worker`, the sole consumer of this client.
//! See that module's doc comment for this workspace's current production
//! dispatch status (adapter and daemon glue complete and tested; no
//! worker-selection call site wired yet, matching every other PRD-058
//! raw-runtime adapter).

use std::collections::HashMap;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use crate::attempt::{
    AdapterError, AdapterStopReason, Message, MessageContent, MessageRole, NonRetryableKind,
    RetryableKind, StreamEvent, StreamObserver, StructuredOutputRequest, SubmitOutcome,
    ToolDefinition, UsageCategories,
};

/// PRD-063's closed local `RuntimeId` vocabulary for the transport layer.
/// Mirrors `familiar_ai_core::config::LocalRuntimeKind`; kept independent
/// (no `familiar-ai-core` dependency in this crate) exactly like every
/// other raw runtime's adapter-owned `RUNTIME_ID` constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRuntimeKind {
    Unsloth,
    Ollama,
}

impl LocalRuntimeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsloth => "unsloth",
            Self::Ollama => "ollama",
        }
    }
}

pub const UNSLOTH_RUNTIME_ID: &str = "unsloth";
pub const OLLAMA_RUNTIME_ID: &str = "ollama";

fn url_scheme(base_url: &str) -> &str {
    base_url.split("://").next().unwrap_or("")
}

fn url_host(base_url: &str) -> &str {
    let without_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    let authority = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    }
}

fn host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host == "::1" {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.octets()[0] == 127,
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

/// Whether it is safe to attach a bearer credential to a request against
/// `base_url`: either the transport is TLS-protected (`https`), or the
/// endpoint is loopback — plaintext that never leaves the local machine.
/// Anything else (a plaintext LAN or remote endpoint) would send the
/// credential in cleartext over the network, so credential attachment is
/// refused rather than silently sent (`docs/contracts/local-worker-runtime.md`).
fn credential_safe_transport(base_url: &str) -> bool {
    url_scheme(base_url).eq_ignore_ascii_case("https") || host_is_loopback(url_host(base_url))
}

/// A resolved local endpoint credential, held only for the client's
/// lifetime. Loopback endpoints typically carry none (`None`); non-loopback
/// endpoints require one by BYO-Auth external reference
/// (`docs/contracts/credential-authentication.md`). Never `Display`s or
/// `Debug`s its value.
#[derive(Clone)]
pub struct LocalAuthToken(Option<String>);

impl LocalAuthToken {
    pub fn new(value: Option<String>) -> Self {
        Self(value)
    }

    fn bearer_header(&self) -> Option<String> {
        self.0.as_ref().map(|value| format!("Bearer {value}"))
    }
}

impl std::fmt::Debug for LocalAuthToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.0.is_some() {
            "LocalAuthToken(Some([REDACTED]))"
        } else {
            "LocalAuthToken(None)"
        })
    }
}

#[derive(Debug, Clone)]
pub struct LocalChatConfig {
    pub base_url: String,
    pub request_timeout_secs: u64,
}

impl Default for LocalChatConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".to_string(),
            request_timeout_secs: 120,
        }
    }
}

/// Per-response facts with no field in the PRD-058 contract: the
/// serving-resolved model identity (when the backend reports one distinct
/// from the requested identifier) and, for runtimes that report it, the
/// endpoint's own model digest — the raw input to artifact verification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalResponseMeta {
    pub resolved_model: Option<String>,
}

/// The outcome of comparing a serving endpoint's claimed model against a
/// registered PRD-062 artifact. `Unverifiable` is the honest default for
/// any runtime or condition that cannot expose a digest — it is never
/// treated as a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactVerificationOutcome {
    Verified { digest: String },
    Mismatch { claimed: String, expected: String },
    Unverifiable { reason: String },
}

pub struct LocalChatClient {
    config: LocalChatConfig,
    client: Client,
    token: LocalAuthToken,
}

pub struct LocalChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
    pub structured_output: Option<&'a StructuredOutputRequest>,
}

impl LocalChatClient {
    /// Fails closed rather than attaching a bearer credential to a plaintext
    /// non-loopback endpoint (`credential_safe_transport`): the credential
    /// would otherwise be sent in cleartext over the network to a LAN or
    /// remote host.
    pub fn new(token: LocalAuthToken, config: LocalChatConfig) -> Result<Self, String> {
        if token.bearer_header().is_some() && !credential_safe_transport(&config.base_url) {
            return Err(format!(
                "refusing to attach a bearer credential to plaintext non-loopback endpoint \"{}\": use an https:// base_url or a loopback endpoint",
                config.base_url
            ));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|error| format!("failed to build local runtime HTTP client: {error}"))?;
        Ok(Self {
            config,
            client,
            token,
        })
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.token.bearer_header() {
            Some(header) => builder.header("Authorization", header),
            None => builder,
        }
    }

    /// Non-executing capability/health probe: lists models the endpoint
    /// currently serves. A successful call proves only that the endpoint is
    /// reachable and OpenAI-compatible-shaped, never that any particular
    /// model is loaded.
    pub async fn probe_health(&self) -> Result<Vec<String>, AdapterError> {
        let url = format!("{}/v1/models", self.config.base_url.trim_end_matches('/'));
        let response = self
            .authorize(self.client.get(&url))
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| AdapterError::Ambiguous {
                reason: format!("local runtime probe response could not be read: {error}"),
            })?;
        if !status.is_success() {
            return Err(map_http_error(status, &text));
        }
        let parsed: Value = serde_json::from_str(&text).map_err(|error| {
            AdapterError::NonRetryable(NonRetryableKind::InvalidRequest).tap_message(format!(
                "local runtime probe response was not valid JSON: {error}"
            ))
        })?;
        Ok(parsed
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Verifies a serving endpoint's claimed model against a registered
    /// PRD-062 artifact digest, using each runtime's own metadata exposure.
    /// Ollama's `/api/tags` reports a per-model digest; Unsloth's
    /// OpenAI-compatible surface exposes none, so it is always
    /// `Unverifiable` (matches the PRD-063 assumption log, re-verify at the
    /// next Unsloth release before assuming otherwise).
    pub async fn verify_artifact(
        &self,
        runtime: LocalRuntimeKind,
        model: &str,
        expected_digest: Option<&str>,
    ) -> ArtifactVerificationOutcome {
        if runtime != LocalRuntimeKind::Ollama {
            return ArtifactVerificationOutcome::Unverifiable {
                reason: format!("{} exposes no artifact digest metadata", runtime.as_str()),
            };
        }
        let url = format!("{}/api/tags", self.config.base_url.trim_end_matches('/'));
        let response = match self.authorize(self.client.get(&url)).send().await {
            Ok(response) => response,
            Err(error) => {
                return ArtifactVerificationOutcome::Unverifiable {
                    reason: format!("artifact digest probe failed: {error}"),
                }
            }
        };
        if !response.status().is_success() {
            return ArtifactVerificationOutcome::Unverifiable {
                reason: format!("artifact digest probe returned {}", response.status()),
            };
        }
        let Ok(text) = response.text().await else {
            return ArtifactVerificationOutcome::Unverifiable {
                reason: "artifact digest probe response could not be read".into(),
            };
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            return ArtifactVerificationOutcome::Unverifiable {
                reason: "artifact digest probe response was not valid JSON".into(),
            };
        };
        let digest = parsed
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(model))
            .and_then(|entry| entry.get("digest"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match (digest, expected_digest) {
            (Some(digest), Some(expected)) if digest == expected => {
                ArtifactVerificationOutcome::Verified { digest }
            }
            (Some(digest), Some(expected)) => ArtifactVerificationOutcome::Mismatch {
                claimed: digest,
                expected: expected.to_string(),
            },
            (Some(_), None) | (None, _) => ArtifactVerificationOutcome::Unverifiable {
                reason: "no registered artifact digest to compare against".into(),
            },
        }
    }

    /// Submits one Chat Completions request and streams its events to
    /// `observer`. Every call is exactly one HTTP request, matching the
    /// PRD-058 attempt model.
    pub async fn submit(
        &self,
        request: &LocalChatRequest<'_>,
        observer: &mut dyn StreamObserver,
    ) -> Result<(SubmitOutcome, LocalResponseMeta), AdapterError> {
        let body = build_request_body(request);
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .authorize(self.client.post(&url))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| AdapterError::Ambiguous {
                reason: format!(
                    "local runtime response body could not be read; completion is unknown: {error}"
                ),
            })?;
        if !status.is_success() {
            return Err(map_http_error(status, &text));
        }
        consume_stream(&text, observer)
    }
}

fn build_request_body(request: &LocalChatRequest<'_>) -> Value {
    let mut body = json!({
        "model": request.model,
        "messages": build_messages(request.messages),
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
            "response_format".into(),
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": structured.schema_name,
                    "schema": serde_json::from_str::<Value>(&structured.json_schema)
                        .unwrap_or_else(|_| json!({"type": "object"})),
                },
            }),
        );
    }
    body
}

fn build_messages(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::with_capacity(messages.len());
    for message in messages {
        match (&message.role, &message.content) {
            (MessageRole::System, MessageContent::Text(text)) => {
                items.push(json!({"role": "system", "content": text}));
            }
            (MessageRole::User, MessageContent::Text(text)) => {
                items.push(json!({"role": "user", "content": text}));
            }
            (MessageRole::Assistant, MessageContent::Text(text)) => {
                items.push(json!({"role": "assistant", "content": text}));
            }
            (_, MessageContent::ToolCalls(calls)) => {
                items.push(json!({
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": calls.iter().map(|call| json!({
                        "id": call.call_id,
                        "type": "function",
                        "function": {"name": call.capability_name, "arguments": call.arguments},
                    })).collect::<Vec<_>>(),
                }));
            }
            (MessageRole::Tool, MessageContent::ToolResult(result)) => {
                let content = if result.is_error {
                    format!("ERROR: {}", result.content)
                } else {
                    result.content.clone()
                };
                items.push(json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": content,
                }));
            }
            _ => {}
        }
    }
    items
}

fn tool_definition_to_param(tool: &ToolDefinition) -> Value {
    let (required, optional) = parse_pseudo_schema(&tool.json_schema);
    let mut properties = serde_json::Map::new();
    for field in required.iter().chain(optional.iter()) {
        properties.insert(field.clone(), json!({"type": "string"}));
    }
    json!({
        "type": "function",
        "function": {
            "name": tool.capability_id,
            "description": format!("Familiar canonical capability {} ({})", tool.capability_id, tool.schema_version),
            "parameters": {
                "type": "object",
                "properties": Value::Object(properties),
                "required": required,
                "additionalProperties": false,
            },
        },
    })
}

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

/// One accumulated tool call across streamed deltas, keyed by the chunk
/// array index the backend assigns it (the OpenAI-compatible chat
/// completions streaming shape identifies a tool call by index, not by a
/// stable id, until the id itself streams in).
#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    name: String,
}

/// Parses a complete SSE (`data: {...}` / `data: [DONE]`) response body and
/// replays its events to `observer` in order. A body with no terminal
/// `finish_reason` at all is the honest `Ambiguous` case: the backend may
/// have accepted and executed the request whose response never fully
/// arrived.
fn consume_stream(
    body: &str,
    observer: &mut dyn StreamObserver,
) -> Result<(SubmitOutcome, LocalResponseMeta), AdapterError> {
    let mut pending: HashMap<u64, PendingCall> = HashMap::new();
    let mut finish_reason: Option<String> = None;
    let mut usage = UsageCategories::default();
    let mut resolved_model: Option<String> = None;
    let mut saw_any_chunk = false;

    for frame in body.split("\n\n") {
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(chunk) = line.strip_prefix("data:") {
                data.push_str(chunk.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            saw_any_chunk = true;
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        saw_any_chunk = true;
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            resolved_model = Some(model.to_string());
        }
        if let Some(reported) = chunk.get("usage") {
            usage = parse_usage(reported);
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            continue;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                observer.on_event(StreamEvent::TextDelta(text.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let entry = pending.entry(index).or_default();
                let mut announced = false;
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if entry.id.is_none() {
                        entry.id = Some(id.to_string());
                        announced = true;
                    }
                }
                let function = call.get("function");
                if let Some(name) = function.and_then(|f| f.get("name")).and_then(Value::as_str) {
                    if entry.name.is_empty() {
                        entry.name = name.to_string();
                        announced = true;
                    }
                }
                let call_id = entry.id.clone().unwrap_or_else(|| format!("call_{index}"));
                if announced {
                    observer.on_event(StreamEvent::ToolCallDelta {
                        call_id: call_id.clone(),
                        capability_id: entry.name.clone(),
                        arguments_fragment: String::new(),
                    });
                }
                if let Some(arguments) = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    if !arguments.is_empty() {
                        observer.on_event(StreamEvent::ToolCallDelta {
                            call_id,
                            capability_id: String::new(),
                            arguments_fragment: arguments.to_string(),
                        });
                    }
                }
            }
        }
    }

    if !saw_any_chunk {
        return Err(AdapterError::Ambiguous {
            reason: "local runtime stream carried no parsable chunks".into(),
        });
    }

    let Some(finish_reason) = finish_reason else {
        return Err(AdapterError::Ambiguous {
            reason: "local runtime stream ended without a finish_reason".into(),
        });
    };

    for (index, call) in &pending {
        let call_id = call.id.clone().unwrap_or_else(|| format!("call_{index}"));
        observer.on_event(StreamEvent::ToolCallComplete { call_id });
    }

    let stop_reason = match finish_reason.as_str() {
        "stop" => AdapterStopReason::EndTurn,
        "tool_calls" => AdapterStopReason::ToolUse,
        "length" => AdapterStopReason::MaxTokens,
        "content_filter" => AdapterStopReason::ContentFilter,
        other => {
            return Err(AdapterError::Ambiguous {
                reason: format!("local runtime reported unrecognized finish_reason {other:?}"),
            })
        }
    };

    Ok((
        SubmitOutcome {
            stop_reason,
            usage,
            provider_request_id: None,
            provider_idempotency_key: None,
        },
        LocalResponseMeta { resolved_model },
    ))
}

/// `prompt_tokens`/`completion_tokens` are the OpenAI-compatible chat
/// completions convention every local backend in scope implements;
/// `prompt_tokens_details.cached_tokens` is reported by some backends and
/// left `None` (never fabricated) when absent, matching the subtraction
/// convention used for every other OpenAI-shaped usage object in this
/// workspace.
fn parse_usage(usage: &Value) -> UsageCategories {
    let prompt_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64);
    let uncached_input_tokens = match (prompt_tokens, cached_tokens) {
        (Some(total), Some(cached)) => Some(total.saturating_sub(cached)),
        (Some(total), None) => Some(total),
        (None, _) => None,
    };
    UsageCategories {
        uncached_input_tokens,
        cache_read_tokens: cached_tokens,
        cache_write_tokens: None,
        output_tokens: usage.get("completion_tokens").and_then(Value::as_u64),
        reasoning_output_tokens: usage
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64),
    }
}

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

fn map_http_error(status: StatusCode, body: &str) -> AdapterError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_string();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        }
        StatusCode::TOO_MANY_REQUESTS => AdapterError::Retryable(RetryableKind::RateLimited {
            retry_after_ms: None,
        }),
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => {
            AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
        }
        s if s.is_server_error() => AdapterError::Retryable(RetryableKind::Overloaded),
        _ => AdapterError::NonRetryable(NonRetryableKind::InvalidRequest),
    }
    .tap_message(message)
}

fn map_transport_error(error: reqwest::Error) -> AdapterError {
    if error.is_timeout() {
        // The endpoint may have already begun executing the request; a
        // client-side timeout is the honest ambiguous case, never a free
        // retry.
        AdapterError::Ambiguous {
            reason: "local runtime request timed out; completion is unknown".into(),
        }
    } else {
        // Connection refused/reset before any response started (the
        // process crashed or the endpoint disappeared): nothing was
        // executed, so retryable is the honest classification.
        AdapterError::Retryable(RetryableKind::TransientTransport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::{MessageContent, MessageRole, ToolResultPayload};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Collector(Vec<StreamEvent>);
    impl StreamObserver for Collector {
        fn on_event(&mut self, event: StreamEvent) {
            self.0.push(event);
        }
    }

    fn client(server: &MockServer) -> LocalChatClient {
        LocalChatClient::new(
            LocalAuthToken::new(None),
            LocalChatConfig {
                base_url: server.uri(),
                request_timeout_secs: 5,
            },
        )
        .unwrap()
    }

    fn sse(frames: &[Value]) -> String {
        let mut body: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn request<'a>(messages: &'a [Message]) -> LocalChatRequest<'a> {
        LocalChatRequest {
            model: "llama3",
            messages,
            tools: &[],
            structured_output: None,
        }
    }

    #[tokio::test]
    async fn health_probe_lists_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "llama3"}, {"id": "qwen3"}]
            })))
            .mount(&server)
            .await;
        let models = client(&server).probe_health().await.unwrap();
        assert_eq!(models, vec!["llama3".to_string(), "qwen3".to_string()]);
    }

    #[tokio::test]
    async fn streams_text_and_completes() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"choices": [{"delta": {"content": "hel"}}]}),
            json!({"choices": [{"delta": {"content": "lo"}, "finish_reason": null}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 2}}),
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let (outcome, _) = client(&server)
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);
        assert_eq!(outcome.usage.uncached_input_tokens, Some(10));
        assert_eq!(outcome.usage.output_tokens, Some(2));
        assert_eq!(
            collector.0,
            vec![
                StreamEvent::TextDelta("hel".into()),
                StreamEvent::TextDelta("lo".into())
            ]
        );
    }

    #[tokio::test]
    async fn tool_call_round_trip_streams_and_completes() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "read-file", "arguments": ""}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"a.rs\"}"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
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
        assert!(collector.0.iter().any(
            |event| matches!(event, StreamEvent::ToolCallComplete { call_id } if call_id == "call_1")
        ));
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
        assert!(fragments.contains("a.rs"));
    }

    #[test]
    fn tool_result_replays_as_tool_role_message() {
        let messages = vec![
            Message::user("read it"),
            Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(ToolResultPayload {
                    call_id: "call_1".into(),
                    capability_name: "read-file".into(),
                    content: "contents".into(),
                    is_error: false,
                }),
            },
        ];
        let body = build_request_body(&request(&messages));
        let out = body["messages"].as_array().unwrap();
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_1");
        assert_eq!(out[1]["content"], "contents");
    }

    #[tokio::test]
    async fn structured_output_maps_to_json_schema_response_format() {
        let structured = StructuredOutputRequest {
            schema_name: "plan".into(),
            json_schema: r#"{"type":"object"}"#.into(),
        };
        let messages = vec![Message::user("hi")];
        let request = LocalChatRequest {
            model: "llama3",
            messages: &messages,
            tools: &[],
            structured_output: Some(&structured),
        };
        let body = build_request_body(&request);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "plan");
    }

    #[tokio::test]
    async fn timeout_is_ambiguous_never_a_free_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_raw(
                        sse(&[json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})]),
                        "text/event-stream",
                    ),
            )
            .mount(&server)
            .await;
        let local = LocalChatClient::new(
            LocalAuthToken::new(None),
            LocalChatConfig {
                base_url: server.uri(),
                request_timeout_secs: 1,
            },
        )
        .unwrap();
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = local
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AdapterError::Ambiguous {
                reason: "local runtime request timed out; completion is unknown".into(),
            }
        );
    }

    #[tokio::test]
    async fn connection_refused_is_retryable_crash_or_disappearance() {
        // No server is bound at this loopback port: simulates a crashed or
        // never-started local runtime process. Nothing was executed, so
        // this is honestly retryable, never ambiguous billing.
        let local = LocalChatClient::new(
            LocalAuthToken::new(None),
            LocalChatConfig {
                base_url: "http://127.0.0.1:1".into(),
                request_timeout_secs: 2,
            },
        )
        .unwrap();
        let messages = vec![Message::user("hi")];
        let mut collector = Collector(Vec::new());
        let error = local
            .submit(&request(&messages), &mut collector)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AdapterError::Retryable(RetryableKind::TransientTransport)
        ));
    }

    #[tokio::test]
    async fn partial_stream_with_no_finish_reason_is_ambiguous() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\": [{\"delta\": {\"content\": \"partial\"}}]}\n\n";
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
    async fn auth_failure_is_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid credentials"}
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
    async fn server_error_is_retryable_overloaded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "error": {"message": "loading model"}
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
    async fn missing_usage_leaves_categories_unknown_not_zero() {
        let server = MockServer::start().await;
        let body = sse(&[json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})]);
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
    async fn artifact_digest_verified_for_ollama_matching_digest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"name": "llama3", "digest": "sha256:abc"}]
            })))
            .mount(&server)
            .await;
        let outcome = client(&server)
            .verify_artifact(LocalRuntimeKind::Ollama, "llama3", Some("sha256:abc"))
            .await;
        assert_eq!(
            outcome,
            ArtifactVerificationOutcome::Verified {
                digest: "sha256:abc".into()
            }
        );
    }

    #[tokio::test]
    async fn artifact_digest_mismatch_is_never_silently_accepted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"name": "llama3", "digest": "sha256:different"}]
            })))
            .mount(&server)
            .await;
        let outcome = client(&server)
            .verify_artifact(LocalRuntimeKind::Ollama, "llama3", Some("sha256:abc"))
            .await;
        assert_eq!(
            outcome,
            ArtifactVerificationOutcome::Mismatch {
                claimed: "sha256:different".into(),
                expected: "sha256:abc".into(),
            }
        );
    }

    #[tokio::test]
    async fn unsloth_artifact_verification_is_always_unverifiable() {
        let server = MockServer::start().await;
        let outcome = client(&server)
            .verify_artifact(LocalRuntimeKind::Unsloth, "any-model", Some("sha256:abc"))
            .await;
        assert!(matches!(
            outcome,
            ArtifactVerificationOutcome::Unverifiable { .. }
        ));
    }

    #[tokio::test]
    async fn absent_registered_digest_is_unverifiable_not_a_default_match() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"name": "llama3", "digest": "sha256:abc"}]
            })))
            .mount(&server)
            .await;
        let outcome = client(&server)
            .verify_artifact(LocalRuntimeKind::Ollama, "llama3", None)
            .await;
        assert!(matches!(
            outcome,
            ArtifactVerificationOutcome::Unverifiable { .. }
        ));
    }

    #[test]
    fn auth_token_debug_is_redacted() {
        let token = LocalAuthToken::new(Some("secret".into()));
        assert_eq!(format!("{token:?}"), "LocalAuthToken(Some([REDACTED]))");
    }

    #[test]
    fn credential_is_refused_over_plaintext_non_loopback_endpoints() {
        let result = LocalChatClient::new(
            LocalAuthToken::new(Some("secret".into())),
            LocalChatConfig {
                base_url: "http://gpu-box.lan:11434".into(),
                request_timeout_secs: 5,
            },
        );
        assert!(result.is_err(), "plaintext LAN endpoint must be refused");
    }

    #[test]
    fn credential_is_accepted_over_https_non_loopback_endpoints() {
        let result = LocalChatClient::new(
            LocalAuthToken::new(Some("secret".into())),
            LocalChatConfig {
                base_url: "https://gpu-box.lan:11434".into(),
                request_timeout_secs: 5,
            },
        );
        assert!(result.is_ok(), "https LAN endpoint must be accepted");
    }

    #[test]
    fn credential_is_accepted_over_plaintext_loopback_endpoints() {
        let result = LocalChatClient::new(
            LocalAuthToken::new(Some("secret".into())),
            LocalChatConfig {
                base_url: "http://127.0.0.1:11434".into(),
                request_timeout_secs: 5,
            },
        );
        assert!(
            result.is_ok(),
            "plaintext loopback endpoint must be accepted"
        );
    }

    #[test]
    fn no_credential_is_accepted_over_any_transport() {
        let result = LocalChatClient::new(
            LocalAuthToken::new(None),
            LocalChatConfig {
                base_url: "http://gpu-box.lan:11434".into(),
                request_timeout_secs: 5,
            },
        );
        assert!(
            result.is_ok(),
            "no credential means nothing can leak, so any transport is accepted"
        );
    }
}
