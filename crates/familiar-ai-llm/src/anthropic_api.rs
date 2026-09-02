//! PRD-059 Anthropic Messages API wire client.
//!
//! This module owns Anthropic's wire shapes: request/response JSON, SSE
//! streaming frames, non-billable probes (token counting, model metadata),
//! and the closed provider error taxonomy from `crate::attempt`. It knows
//! nothing about Familiar's canonical tool capabilities or the raw-runtime
//! loop — that projection lives in `familiar_ai_agent::anthropic`, which is
//! the actual `InferenceAdapter` implementation built on top of this client.
//!
//! No network call in this module is ever made from a test: every test here
//! runs against a `wiremock::MockServer`.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familiar_ai_core::config::AuthDescriptor;

use crate::attempt::{AdapterError, NonRetryableKind, RetryableKind};

/// Familiar's stable runtime identity for this adapter (PRD-057).
pub const RUNTIME_ID: &str = "anthropic-api";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

// ---------------------------------------------------------------------
// Credential resolution — resolved at call time, never persisted
// ---------------------------------------------------------------------

/// Resolves a BYO-Auth descriptor into the credential bytes needed for one
/// request. Resolution happens immediately before a request is built;
/// nothing here is cached, logged, or persisted beyond the call that needs
/// it.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, auth: &AuthDescriptor) -> Result<String, String>;
}

/// The exact operator remedy for a missing or empty `env: NAME` credential.
pub fn missing_env_remedy(name: &str) -> String {
    format!(
        "anthropic-api credential is missing — export `{name}` with a valid Anthropic API key (BYO-Auth: `env: {name}`)"
    )
}

/// The exact operator remedy when the configured BYO-Auth descriptor cannot
/// supply an Anthropic API key on its own (e.g. `cli-login`, `ssh-agent`,
/// `none`, or a `credential-store` reference with no resolver configured).
pub fn unsupported_auth_remedy(auth: &AuthDescriptor) -> String {
    format!(
        "anthropic-api requires an API key; the configured auth descriptor ({auth:?}) cannot supply one on its own — use an `env: NAME` descriptor, or supply a CredentialResolver that can resolve it"
    )
}

/// Resolves only `env: NAME` descriptors — how Anthropic API keys are
/// supplied in practice. Any other descriptor fails closed with a remedy
/// naming what's needed instead of guessing.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvCredentialResolver;

impl CredentialResolver for EnvCredentialResolver {
    fn resolve(&self, auth: &AuthDescriptor) -> Result<String, String> {
        match auth {
            AuthDescriptor::Env(name) => std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| missing_env_remedy(name)),
            other => Err(unsupported_auth_remedy(other)),
        }
    }
}

/// Wraps an already-resolved credential value — for a caller (the daemon,
/// or a test) that has already performed BYO-Auth resolution, including
/// descriptor kinds this crate cannot resolve on its own (a platform
/// credential store). The value lives only in memory for the life of this
/// resolver; it is never written to configuration, logs, or a database row.
#[derive(Clone)]
pub struct StaticCredentialResolver(pub String);

impl CredentialResolver for StaticCredentialResolver {
    fn resolve(&self, _auth: &AuthDescriptor) -> Result<String, String> {
        if self.0.is_empty() {
            Err("resolved credential is empty".into())
        } else {
            Ok(self.0.clone())
        }
    }
}

// ---------------------------------------------------------------------
// Request wire shapes
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct WireCacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl WireCacheControl {
    pub fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<WireCacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// Replayed unchanged on the same model per the provider's replay
    /// rules — the signature is opaque and must round-trip verbatim.
    Thinking { thinking: String, signature: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: Vec<WireContentBlock>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<WireCacheControl>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct WireThinking {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireOutputFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema: Value,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct WireOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<WireOutputFormat>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireRequestBody {
    pub model: String,
    pub max_tokens: u64,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<WireContentBlock>>,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<WireOutputConfig>,
}

// ---------------------------------------------------------------------
// Streaming (SSE) response shapes
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct WireUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireMessageStart {
    #[allow(dead_code)]
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub usage: WireUsage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireContentBlockStart {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireStopDetails {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<WireStopDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireErrorBody {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub kind: String,
    #[allow(dead_code)]
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    MessageStart {
        message: WireMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: WireContentBlockStart,
    },
    ContentBlockDelta {
        index: usize,
        delta: WireDelta,
    },
    ContentBlockStop {
        #[allow(dead_code)]
        index: usize,
    },
    MessageDelta {
        delta: WireMessageDelta,
        #[serde(default)]
        usage: WireUsage,
    },
    MessageStop,
    Ping,
    Error {
        #[allow(dead_code)]
        error: WireErrorBody,
    },
    #[serde(other)]
    Unknown,
}

/// Terminal facts accumulated from one streamed `/v1/messages` response.
/// `usage` holds the latest-known value per category — never a sum of
/// deltas — matching the agent-loop contract's "adapter's final reported
/// total, never derived by summing deltas."
#[derive(Debug, Clone, Default)]
pub struct StreamOutcome {
    pub stop_reason: Option<String>,
    pub stop_details_category: Option<String>,
    pub usage: WireUsage,
    pub resolved_model: Option<String>,
    pub provider_request_id: Option<String>,
}

fn merge_usage(prev: WireUsage, new: &WireUsage) -> WireUsage {
    WireUsage {
        input_tokens: new.input_tokens.or(prev.input_tokens),
        output_tokens: new.output_tokens.or(prev.output_tokens),
        cache_creation_input_tokens: new
            .cache_creation_input_tokens
            .or(prev.cache_creation_input_tokens),
        cache_read_input_tokens: new.cache_read_input_tokens.or(prev.cache_read_input_tokens),
    }
}

fn apply_event(outcome: &mut StreamOutcome, event: &SseEvent) {
    match event {
        SseEvent::MessageStart { message } => {
            outcome.resolved_model = Some(message.model.clone());
            outcome.usage = merge_usage(std::mem::take(&mut outcome.usage), &message.usage);
        }
        SseEvent::MessageDelta { delta, usage } => {
            if delta.stop_reason.is_some() {
                outcome.stop_reason = delta.stop_reason.clone();
            }
            if let Some(details) = &delta.stop_details {
                outcome.stop_details_category = details.category.clone();
            }
            outcome.usage = merge_usage(std::mem::take(&mut outcome.usage), usage);
        }
        _ => {}
    }
}

/// Extracts complete `\n\n`-delimited SSE frames from `buf` and decodes each
/// as UTF-8, removing the consumed bytes from `buf`. Bytes after the last
/// complete frame boundary — including a multi-byte UTF-8 character
/// truncated mid-sequence by a chunk boundary — are left in `buf` for the
/// next call, so a code point split across two `bytes_stream()` chunks is
/// only ever decoded once every one of its bytes has arrived. `\n\n` is a
/// two-byte ASCII marker, so it can never appear as (part of) a UTF-8
/// continuation byte, and locating it in the raw byte buffer is therefore
/// safe independent of where multi-byte characters fall.
fn drain_sse_frames(buf: &mut Vec<u8>) -> Result<Vec<String>, AdapterError> {
    let mut frames = Vec::new();
    while let Some(frame_end) = buf.windows(2).position(|w| w == b"\n\n") {
        let frame_bytes: Vec<u8> = buf.drain(..frame_end + 2).collect();
        let frame = std::str::from_utf8(&frame_bytes[..frame_end])
            .map_err(|error| AdapterError::Ambiguous {
                reason: format!("SSE frame was not valid UTF-8: {error}"),
            })?
            .to_string();
        frames.push(frame);
    }
    Ok(frames)
}

fn parse_sse_frame(frame: &str) -> Option<SseEvent> {
    let data: String = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str::<SseEvent>(&data).ok()
}

// ---------------------------------------------------------------------
// Error taxonomy classification
// ---------------------------------------------------------------------

fn parse_error_type(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .get("error")?
        .get("type")?
        .as_str()
        .map(str::to_string)
}

fn classify_transport_error(err: reqwest::Error) -> AdapterError {
    if err.is_timeout() {
        AdapterError::Ambiguous {
            reason: "request timed out before a response was received".into(),
        }
    } else if err.is_connect() {
        AdapterError::Retryable(RetryableKind::TransientTransport)
    } else if err.is_body() || err.is_decode() {
        // The response had already started (or the provider may have begun
        // processing) when the stream broke — usage may be partially known
        // server-side, so this is ambiguous, never a fabricated zero.
        AdapterError::Ambiguous {
            reason: format!("stream ended unexpectedly: {err}"),
        }
    } else {
        AdapterError::Retryable(RetryableKind::TransientTransport)
    }
}

fn classify_http_status(
    status: StatusCode,
    retry_after_secs: Option<u64>,
    body: &str,
) -> AdapterError {
    let error_type = parse_error_type(body);
    if error_type.as_deref() == Some("overloaded_error") {
        return AdapterError::Retryable(RetryableKind::Overloaded);
    }
    match status {
        StatusCode::TOO_MANY_REQUESTS => AdapterError::Retryable(RetryableKind::RateLimited {
            retry_after_ms: retry_after_secs.map(|secs| secs * 1000),
        }),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        }
        StatusCode::BAD_REQUEST
        | StatusCode::NOT_FOUND
        | StatusCode::UNPROCESSABLE_ENTITY
        | StatusCode::PAYLOAD_TOO_LARGE => {
            AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
        }
        s if s.as_u16() == 529 => AdapterError::Retryable(RetryableKind::Overloaded),
        s if s.is_server_error() => AdapterError::Retryable(RetryableKind::TransientTransport),
        _ => AdapterError::NonRetryable(NonRetryableKind::InvalidRequest),
    }
}

// ---------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AnthropicHttpConfig {
    pub base_url: String,
    pub anthropic_version: String,
    pub request_timeout_secs: u64,
}

impl Default for AnthropicHttpConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            anthropic_version: ANTHROPIC_VERSION.into(),
            request_timeout_secs: 120,
        }
    }
}

pub struct AnthropicHttpClient {
    client: Client,
    base_url: String,
    anthropic_version: String,
}

impl AnthropicHttpClient {
    pub fn new(config: AnthropicHttpConfig) -> Result<Self, AdapterError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|error| {
                tracing::error!(%error, "anthropic-api HTTP client construction failed");
                AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)
            })?;
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            anthropic_version: config.anthropic_version,
        })
    }

    /// Streams one `POST /v1/messages` request, invoking `on_event` for
    /// every SSE frame as it arrives. Returns the terminal `StreamOutcome`,
    /// or an `AdapterError` classified per the closed taxonomy. A stream
    /// that ends without ever reporting a stop reason (a dropped connection
    /// mid-response) is reported as `Ambiguous`, never a fabricated
    /// `end_turn`.
    pub async fn stream_messages(
        &self,
        api_key: &str,
        body: &WireRequestBody,
        on_event: &mut (dyn FnMut(&SseEvent) + Send),
    ) -> Result<StreamOutcome, AdapterError> {
        let url = format!("{}/v1/messages", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.anthropic_version)
            .json(body)
            .send()
            .await
            .map_err(classify_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let body_text = response.text().await.unwrap_or_default();
            return Err(classify_http_status(status, retry_after, &body_text));
        }

        let provider_request_id = response
            .headers()
            .get("request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let mut outcome = StreamOutcome {
            provider_request_id,
            ..Default::default()
        };
        let mut byte_buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(classify_transport_error)?;
            byte_buf.extend_from_slice(&chunk);
            for frame in drain_sse_frames(&mut byte_buf)? {
                if let Some(event) = parse_sse_frame(&frame) {
                    apply_event(&mut outcome, &event);
                    on_event(&event);
                }
            }
        }

        if outcome.stop_reason.is_none() {
            return Err(AdapterError::Ambiguous {
                reason: "stream ended before a stop reason was received".into(),
            });
        }
        Ok(outcome)
    }

    /// Non-billable preflight sizing probe (`POST /v1/messages/count_tokens`,
    /// PRD-047 discipline).
    pub async fn count_tokens(
        &self,
        api_key: &str,
        model: &str,
        messages: &[WireMessage],
        system: Option<&[WireContentBlock]>,
        tools: Option<&[WireTool]>,
    ) -> Result<u64, AdapterError> {
        #[derive(Serialize)]
        struct CountTokensRequest<'a> {
            model: &'a str,
            messages: &'a [WireMessage],
            #[serde(skip_serializing_if = "Option::is_none")]
            system: Option<&'a [WireContentBlock]>,
            #[serde(skip_serializing_if = "Option::is_none")]
            tools: Option<&'a [WireTool]>,
        }
        #[derive(Deserialize)]
        struct CountTokensResponse {
            input_tokens: u64,
        }

        let url = format!("{}/v1/messages/count_tokens", self.base_url);
        let request = CountTokensRequest {
            model,
            messages,
            system,
            tools,
        };
        let response = self
            .client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.anthropic_version)
            .json(&request)
            .send()
            .await
            .map_err(classify_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(classify_http_status(status, None, &body_text));
        }
        let parsed: CountTokensResponse =
            response
                .json()
                .await
                .map_err(|error| AdapterError::Ambiguous {
                    reason: format!("count_tokens response did not parse: {error}"),
                })?;
        Ok(parsed.input_tokens)
    }

    /// Non-billable capability/context-window probe (`GET /v1/models/{id}`,
    /// PRD-047 discipline). Probe failure must leave capabilities unknown —
    /// callers propagate `Err` rather than defaulting any field.
    pub async fn retrieve_model(
        &self,
        api_key: &str,
        model_id: &str,
    ) -> Result<ModelMetadata, AdapterError> {
        let url = format!("{}/v1/models/{model_id}", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.anthropic_version)
            .send()
            .await
            .map_err(classify_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(classify_http_status(status, None, &body_text));
        }
        response
            .json()
            .await
            .map_err(|error| AdapterError::Ambiguous {
                reason: format!("models response did not parse: {error}"),
            })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelMetadata {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(base_url: String) -> AnthropicHttpConfig {
        AnthropicHttpConfig {
            base_url,
            anthropic_version: ANTHROPIC_VERSION.into(),
            request_timeout_secs: 5,
        }
    }

    fn text_request() -> WireRequestBody {
        WireRequestBody {
            model: "claude-test-model".into(),
            max_tokens: 1024,
            stream: true,
            system: Some(vec![WireContentBlock::Text {
                text: "system prompt".into(),
                cache_control: Some(WireCacheControl::ephemeral()),
            }]),
            messages: vec![WireMessage {
                role: "user",
                content: vec![WireContentBlock::Text {
                    text: "hello".into(),
                    cache_control: None,
                }],
            }],
            tools: None,
            thinking: None,
            output_config: None,
        }
    }

    fn sse_body(frames: &[&str]) -> String {
        frames
            .iter()
            .map(|f| format!("data: {f}\n\n"))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn drain_sse_frames_holds_back_a_multi_byte_character_split_across_chunks() {
        let text_with_emoji = "hi 🎉 there";
        let frame_json = format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text_with_emoji}"}}}}"#
        );
        let full_frame = format!("data: {frame_json}\n\n");
        let bytes = full_frame.into_bytes();

        // Split the 4-byte emoji sequence (0xF0 0x9F 0x8E 0x89) in half, so
        // neither chunk on its own holds a complete UTF-8 code point.
        let emoji_bytes = "🎉".as_bytes();
        let emoji_pos = bytes
            .windows(emoji_bytes.len())
            .position(|w| w == emoji_bytes)
            .unwrap();
        let split_at = emoji_pos + 2;
        let (chunk1, chunk2) = bytes.split_at(split_at);

        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(chunk1);
        let frames = drain_sse_frames(&mut buf).unwrap();
        assert!(
            frames.is_empty(),
            "no frame should be emitted before the full frame (including the \
             remaining emoji bytes and the trailing \\n\\n) has arrived"
        );

        buf.extend_from_slice(chunk2);
        let frames = drain_sse_frames(&mut buf).unwrap();
        assert_eq!(frames.len(), 1);
        let event = parse_sse_frame(&frames[0]).expect("frame parses as an SseEvent");
        match event {
            SseEvent::ContentBlockDelta {
                delta: WireDelta::TextDelta { text },
                ..
            } => assert_eq!(text, text_with_emoji),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn streams_text_deltas_and_final_usage() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model-resolved","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":", world"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "text/event-stream")
                    .insert_header("request-id", "req_abc123"),
            )
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let mut text = String::new();
        let outcome = client
            .stream_messages("sk-test", &text_request(), &mut |event| {
                if let SseEvent::ContentBlockDelta {
                    delta: WireDelta::TextDelta { text: t },
                    ..
                } = event
                {
                    text.push_str(t);
                }
            })
            .await
            .unwrap();

        assert_eq!(text, "Hello, world");
        assert_eq!(outcome.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(
            outcome.resolved_model.as_deref(),
            Some("claude-test-model-resolved")
        );
        assert_eq!(outcome.usage.input_tokens, Some(10));
        assert_eq!(outcome.usage.output_tokens, Some(5));
        assert_eq!(outcome.provider_request_id.as_deref(), Some("req_abc123"));
    }

    #[tokio::test]
    async fn streams_tool_use_blocks() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let mut fragments = String::new();
        let outcome = client
            .stream_messages("sk-test", &text_request(), &mut |event| {
                if let SseEvent::ContentBlockDelta {
                    delta: WireDelta::InputJsonDelta { partial_json },
                    ..
                } = event
                {
                    fragments.push_str(partial_json);
                }
            })
            .await
            .unwrap();

        assert_eq!(fragments, r#"{"path":"a.txt"}"#);
        assert_eq!(outcome.stop_reason.as_deref(), Some("tool_use"));
    }

    #[tokio::test]
    async fn refusal_stop_reason_carries_category() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"category":"cyber"}},"usage":{"output_tokens":0}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let outcome = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason.as_deref(), Some("refusal"));
        assert_eq!(outcome.stop_details_category.as_deref(), Some("cyber"));
    }

    #[tokio::test]
    async fn pause_turn_stop_reason_is_preserved() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":5}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":50}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let outcome = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason.as_deref(), Some("pause_turn"));
    }

    #[tokio::test]
    async fn stream_ending_without_stop_reason_is_ambiguous() {
        let server = MockServer::start().await;
        // Connection closes mid-stream — no message_delta/message_stop ever arrives.
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let mut observed_text = String::new();
        let result = client
            .stream_messages("sk-test", &text_request(), &mut |event| {
                if let SseEvent::ContentBlockDelta {
                    delta: WireDelta::TextDelta { text },
                    ..
                } = event
                {
                    observed_text.push_str(text);
                }
            })
            .await;

        assert!(matches!(result, Err(AdapterError::Ambiguous { .. })));
        // The observer still saw whatever streamed before the interruption —
        // "a partial or interrupted stream preserves observed usage" applies
        // to what reaches the observer, even though the attempt itself is
        // recorded ambiguous, never a fabricated completion.
        assert_eq!(observed_text, "partial");
    }

    #[tokio::test]
    async fn malformed_sse_frame_is_skipped_not_fatal() {
        let server = MockServer::start().await;
        let body = "data: not-json-at-all\n\n".to_string()
            + &sse_body(&[
                r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{}}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
                r#"{"type":"message_stop"}"#,
            ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let outcome = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn rate_limited_reports_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "30")
                    .set_body_json(json!({"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}})),
            )
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await;
        assert!(matches!(
            result,
            Err(AdapterError::Retryable(RetryableKind::RateLimited {
                retry_after_ms: Some(30_000)
            }))
        ));
    }

    #[tokio::test]
    async fn overloaded_maps_to_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(529).set_body_json(
                json!({"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}),
            ))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await;
        assert!(matches!(
            result,
            Err(AdapterError::Retryable(RetryableKind::Overloaded))
        ));
    }

    #[tokio::test]
    async fn server_error_maps_to_retryable_transport() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await;
        assert!(matches!(
            result,
            Err(AdapterError::Retryable(RetryableKind::TransientTransport))
        ));
    }

    #[tokio::test]
    async fn auth_failure_is_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"type":"error","error":{"type":"authentication_error","message":"invalid key"}})))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client
            .stream_messages("sk-bad", &text_request(), &mut |_| {})
            .await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
    }

    #[tokio::test]
    async fn invalid_request_is_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"type":"error","error":{"type":"invalid_request_error","message":"bad request"}})))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client
            .stream_messages("sk-test", &text_request(), &mut |_| {})
            .await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest))
        ));
    }

    #[tokio::test]
    async fn count_tokens_returns_input_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages/count_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 42})))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let request = text_request();
        let count = client
            .count_tokens("sk-test", &request.model, &request.messages, None, None)
            .await
            .unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn retrieve_model_returns_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models/claude-test-model"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "claude-test-model",
                "display_name": "Claude Test Model",
                "max_input_tokens": 1000000,
                "max_tokens": 128000
            })))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let metadata = client
            .retrieve_model("sk-test", "claude-test-model")
            .await
            .unwrap();
        assert_eq!(metadata.max_input_tokens, Some(1_000_000));
        assert_eq!(metadata.max_tokens, Some(128_000));
    }

    #[tokio::test]
    async fn retrieve_model_failure_leaves_capabilities_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models/nonexistent"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"type":"error","error":{"type":"not_found_error","message":"no such model"}})))
            .mount(&server)
            .await;

        let client = AnthropicHttpClient::new(config(server.uri())).unwrap();
        let result = client.retrieve_model("sk-test", "nonexistent").await;
        assert!(result.is_err());
    }

    #[test]
    fn env_resolver_fails_closed_with_remedy_for_missing_var() {
        let resolver = EnvCredentialResolver;
        let name = "PRD059_TEST_ANTHROPIC_KEY_DOES_NOT_EXIST";
        std::env::remove_var(name);
        let result = resolver.resolve(&AuthDescriptor::Env(name.into()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains(name));
    }

    #[test]
    fn env_resolver_rejects_non_env_descriptors() {
        let resolver = EnvCredentialResolver;
        let result = resolver.resolve(&AuthDescriptor::None);
        assert!(result.is_err());
    }

    #[test]
    fn static_resolver_returns_wrapped_value() {
        let resolver = StaticCredentialResolver("sk-static".into());
        let result = resolver.resolve(&AuthDescriptor::None).unwrap();
        assert_eq!(result, "sk-static");
    }
}
