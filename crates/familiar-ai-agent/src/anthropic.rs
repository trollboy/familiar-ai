//! PRD-059 Anthropic Messages API adapter.
//!
//! Implements the PRD-058 `InferenceAdapter` contract
//! (`familiar_ai_llm::attempt`) on top of the wire-level client in
//! `familiar_ai_llm::anthropic_api`. This module owns the projection from
//! Familiar's provider-neutral `SubmitRequest`/`SubmitOutcome` shapes onto
//! Anthropic's Messages API — tool definitions, `tool_use`/`tool_result`
//! content blocks, streaming, cache-control placement, stop-reason mapping,
//! and usage-category normalization. Adding this adapter changes no loop,
//! routing, or accounting semantics: the loop core in
//! `crate::raw_runtime` sees only the shared contract types.
//!
//! ## The tool_use replay problem
//!
//! The loop core's [`Message`]/[`MessageContent`] carry only text and
//! `tool_result` content — there is no `tool_use` variant, because the loop
//! never needs to *read* a model's own tool call, only record that it
//! happened and feed the result back. But the Anthropic wire format
//! requires every `tool_result` block to be preceded, in the conversation
//! history, by the exact `tool_use` block it answers (same `id`, `name`,
//! `input`). This adapter therefore remembers, per attempt-independent call
//! id, every `tool_use` block it streams out of a response
//! (`tool_use_registry`), and replays it when a later request's `messages`
//! history reaches the matching `tool_result`. This is entirely
//! adapter-local state; the loop core is never made aware of it.
//!
//! The same gap applies to `thinking` blocks: the provider's replay rule
//! requires them to be passed back unchanged on the same model, but the
//! loop core has nowhere to carry them either. Any thinking block(s)
//! streamed immediately before a `tool_use` block are remembered alongside
//! that call (keyed by the same call id) and replayed immediately before
//! it — matching the order the model actually produced them in.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use familiar_ai_core::config::AuthDescriptor;
use familiar_ai_llm::anthropic_api::{
    AnthropicHttpClient, AnthropicHttpConfig, CredentialResolver, EnvCredentialResolver, SseEvent,
    WireCacheControl, WireContentBlock, WireContentBlockStart, WireDelta, WireMessage,
    WireOutputConfig, WireOutputFormat, WireRequestBody, WireThinking, WireTool, WireUsage,
};
use familiar_ai_llm::attempt::{
    AdapterError, AdapterStopReason, AttemptId, CacheControl, InferenceAdapter, Message,
    MessageContent, MessageRole, NonRetryableKind, StreamEvent, StreamObserver, SubmitOutcome,
    SubmitRequest, ToolDefinition, ToolResultPayload, UsageCategories,
};

/// Familiar's stable runtime identity for this adapter (PRD-057):
/// `[worker_registry.workers.<id>] runtime = "anthropic-api"`.
pub const RUNTIME_ID: &str = familiar_ai_llm::anthropic_api::RUNTIME_ID;

#[derive(Debug, Clone)]
struct RememberedToolUse {
    name: String,
    input: Value,
    /// Thinking blocks (text, opaque signature) that streamed immediately
    /// before this tool_use block, in production order — replayed
    /// unchanged ahead of the reconstructed tool_use block.
    preceding_thinking: Vec<(String, String)>,
}

/// Per-attempt facts the shared `SubmitOutcome` has no field for, but that
/// PRD-059 must still surface: the provider-resolved model identity (an
/// alias like `claude-sonnet-5` may resolve to a dated snapshot) and a
/// refusal's category. Keyed by [`AttemptId`], queried by the host after
/// `submit` returns; the loop itself never needs this to function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptMetadata {
    pub resolved_model: Option<String>,
    pub refusal_category: Option<String>,
}

/// Construction-time configuration. `auth`/`effort`/`thinking_enabled` are
/// the worker's capability-profile-derived defaults; a `SubmitRequest`'s own
/// `reasoning_control`, when present, overrides them for that one attempt.
pub struct AnthropicAdapterConfig {
    pub auth: AuthDescriptor,
    pub max_tokens: u64,
    pub effort: Option<String>,
    pub thinking_enabled: bool,
    pub http: AnthropicHttpConfig,
}

impl Default for AnthropicAdapterConfig {
    fn default() -> Self {
        Self {
            auth: AuthDescriptor::Env("ANTHROPIC_API_KEY".into()),
            max_tokens: 8192,
            effort: None,
            thinking_enabled: false,
            http: AnthropicHttpConfig::default(),
        }
    }
}

pub struct AnthropicAdapter {
    client: AnthropicHttpClient,
    auth: AuthDescriptor,
    credential_resolver: Box<dyn CredentialResolver>,
    max_tokens: u64,
    effort: Option<String>,
    thinking_enabled: bool,
    tool_use_registry: Mutex<HashMap<String, RememberedToolUse>>,
    attempt_metadata: Mutex<HashMap<String, AttemptMetadata>>,
}

impl AnthropicAdapter {
    pub fn new(config: AnthropicAdapterConfig) -> Result<Self, AdapterError> {
        Self::with_credential_resolver(config, Box::new(EnvCredentialResolver))
    }

    /// Use when the host has already resolved (or must resolve through a
    /// platform credential store the wire client cannot reach directly) the
    /// BYO-Auth descriptor into a credential.
    pub fn with_credential_resolver(
        config: AnthropicAdapterConfig,
        credential_resolver: Box<dyn CredentialResolver>,
    ) -> Result<Self, AdapterError> {
        Ok(Self {
            client: AnthropicHttpClient::new(config.http)?,
            auth: config.auth,
            credential_resolver,
            max_tokens: config.max_tokens,
            effort: config.effort,
            thinking_enabled: config.thinking_enabled,
            tool_use_registry: Mutex::new(HashMap::new()),
            attempt_metadata: Mutex::new(HashMap::new()),
        })
    }

    /// Facts about a completed attempt that `SubmitOutcome` has no field
    /// for. `None` until `submit` has returned for that attempt.
    pub fn attempt_metadata(&self, attempt_id: &AttemptId) -> Option<AttemptMetadata> {
        self.attempt_metadata
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&attempt_id.0)
            .cloned()
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<WireTool> {
        tools
            .iter()
            .map(|tool| WireTool {
                name: tool.capability_id.clone(),
                description: Some(format!(
                    "Familiar canonical capability (schema {})",
                    tool.schema_version
                )),
                input_schema: to_input_schema(&tool.json_schema),
                cache_control: None,
            })
            .collect()
    }

    fn wire_reasoning(&self, request: &SubmitRequest) -> (Option<WireThinking>, Option<String>) {
        let effort = request
            .reasoning_control
            .as_ref()
            .and_then(|control| control.effort.clone())
            .or_else(|| self.effort.clone());
        let thinking_requested = request.reasoning_control.is_some() || self.thinking_enabled;
        let thinking = thinking_requested.then_some(WireThinking { kind: "adaptive" });
        (thinking, effort)
    }

    /// Groups the loop's flat `Vec<Message>` into Anthropic's wire shape:
    /// the (single) system message becomes the top-level `system` array,
    /// and every contiguous run of `tool_result` messages is paired with a
    /// preceding assistant message carrying the matching `tool_use` blocks
    /// (replayed from `tool_use_registry`) — "all results for a parallel
    /// batch in one user message," per the Messages API shape.
    fn convert_messages(
        &self,
        messages: &[Message],
        cache_control: CacheControl,
    ) -> (Vec<WireContentBlock>, Vec<WireMessage>) {
        let mut system_blocks = Vec::new();
        let mut wire_messages = Vec::new();
        let mut index = 0;
        while index < messages.len() {
            match &messages[index] {
                Message {
                    role: MessageRole::System,
                    content: MessageContent::Text(text),
                } => {
                    system_blocks.push(WireContentBlock::Text {
                        text: text.clone(),
                        cache_control: None,
                    });
                    index += 1;
                }
                Message {
                    role: MessageRole::User,
                    content: MessageContent::Text(text),
                } => {
                    wire_messages.push(WireMessage {
                        role: "user",
                        content: vec![WireContentBlock::Text {
                            text: text.clone(),
                            cache_control: None,
                        }],
                    });
                    index += 1;
                }
                Message {
                    role: MessageRole::Assistant,
                    content: MessageContent::Text(text),
                } => {
                    let (tool_results, next) = self.collect_tool_results(messages, index + 1);
                    self.push_assistant_and_tool_turn(
                        &mut wire_messages,
                        Some(text.clone()),
                        &tool_results,
                    );
                    index = next;
                }
                Message {
                    role: MessageRole::Tool,
                    content: MessageContent::ToolResult(_),
                } => {
                    let (tool_results, next) = self.collect_tool_results(messages, index);
                    self.push_assistant_and_tool_turn(&mut wire_messages, None, &tool_results);
                    index = next;
                }
                // The loop core never produces a System/User message with
                // tool_result content, or any other combination; skip
                // defensively rather than fabricate a wire shape for it.
                _ => index += 1,
            }
        }
        if cache_control == CacheControl::Ephemeral {
            if let Some(WireContentBlock::Text { cache_control, .. }) = system_blocks.last_mut() {
                *cache_control = Some(WireCacheControl::ephemeral());
            }
        }
        (system_blocks, wire_messages)
    }

    /// Consumes the contiguous run of `Tool`-role messages starting at
    /// `start`, returning them plus the index just past the run.
    fn collect_tool_results(
        &self,
        messages: &[Message],
        start: usize,
    ) -> (Vec<ToolResultPayload>, usize) {
        let mut results = Vec::new();
        let mut index = start;
        while index < messages.len() {
            let Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(payload),
            } = &messages[index]
            else {
                break;
            };
            results.push(payload.clone());
            index += 1;
        }
        (results, index)
    }

    /// Pushes the assistant turn (text, plus any replayed `tool_use` blocks)
    /// and, when there were tool calls, the single following user turn
    /// carrying every `tool_result` block.
    fn push_assistant_and_tool_turn(
        &self,
        wire_messages: &mut Vec<WireMessage>,
        text: Option<String>,
        tool_results: &[ToolResultPayload],
    ) {
        let mut assistant_content = Vec::new();
        {
            // Thinking blocks are replayed unchanged, ahead of everything
            // else in the turn — matching the order the model produced
            // them (thinking always precedes the text/tool_use it led to).
            let registry = self
                .tool_use_registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for payload in tool_results {
                if let Some(remembered) = registry.get(&payload.call_id) {
                    for (thinking, signature) in &remembered.preceding_thinking {
                        assistant_content.push(WireContentBlock::Thinking {
                            thinking: thinking.clone(),
                            signature: signature.clone(),
                        });
                    }
                }
            }
        }
        if let Some(text) = text {
            assistant_content.push(WireContentBlock::Text {
                text,
                cache_control: None,
            });
        }
        {
            let registry = self
                .tool_use_registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for payload in tool_results {
                match registry.get(&payload.call_id) {
                    Some(remembered) => assistant_content.push(WireContentBlock::ToolUse {
                        id: payload.call_id.clone(),
                        name: remembered.name.clone(),
                        input: remembered.input.clone(),
                    }),
                    // No remembered entry — either the call was refused for
                    // malformed arguments (never registered) or the id is
                    // otherwise unknown to this adapter instance (e.g. a
                    // fresh instance resuming persisted history). Every
                    // `tool_result` must still be preceded by a `tool_use`
                    // of the same id or the Messages API rejects the whole
                    // request; synthesize one from the payload itself
                    // rather than leave it orphaned.
                    None => assistant_content.push(WireContentBlock::ToolUse {
                        id: payload.call_id.clone(),
                        name: payload.capability_name.clone(),
                        input: serde_json::json!({}),
                    }),
                }
            }
        }
        if !assistant_content.is_empty() {
            wire_messages.push(WireMessage {
                role: "assistant",
                content: assistant_content,
            });
        }
        if !tool_results.is_empty() {
            let user_content = tool_results
                .iter()
                .map(|payload| WireContentBlock::ToolResult {
                    tool_use_id: payload.call_id.clone(),
                    content: payload.content.clone(),
                    is_error: payload.is_error.then_some(true),
                })
                .collect();
            wire_messages.push(WireMessage {
                role: "user",
                content: user_content,
            });
        }
    }

    fn build_request_body(&self, request: &SubmitRequest) -> WireRequestBody {
        let (system, messages) = self.convert_messages(&request.messages, request.cache_control);
        let tools = self.wire_tools(&request.tools);
        let (thinking, effort) = self.wire_reasoning(request);
        let format = request.structured_output.as_ref().map(|structured| {
            let schema: Value =
                serde_json::from_str(&structured.json_schema).unwrap_or(Value::Null);
            WireOutputFormat {
                kind: "json_schema",
                schema,
            }
        });
        let output_config = if effort.is_some() || format.is_some() {
            Some(WireOutputConfig { effort, format })
        } else {
            None
        };
        WireRequestBody {
            model: request.model.clone(),
            max_tokens: self.max_tokens,
            stream: true,
            system: (!system.is_empty()).then_some(system),
            messages,
            tools: (!tools.is_empty()).then_some(tools),
            thinking,
            output_config,
        }
    }
}

/// A minimal, permissive JSON Schema document naming `required`/`optional`
/// properties as unconstrained. Used only when `json_schema` is not already
/// a real JSON Schema object (the loop core's own tool projection currently
/// emits `{"required":[...],"optional":[...]}`, not a schema document) —
/// when it already carries `type`/`properties`, it is passed through
/// unchanged.
fn to_input_schema(json_schema: &str) -> Value {
    let parsed: Value = serde_json::from_str(json_schema).unwrap_or(Value::Null);
    let Value::Object(map) = &parsed else {
        return serde_json::json!({"type": "object", "properties": {}});
    };
    if map.contains_key("type") && map.contains_key("properties") {
        return parsed;
    }
    let names = |key: &str| -> Vec<String> {
        map.get(key)
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let required = names("required");
    let optional = names("optional");
    let mut properties = serde_json::Map::new();
    for name in required.iter().chain(optional.iter()) {
        properties.insert(name.clone(), serde_json::json!({}));
    }
    serde_json::json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
    })
}

fn map_stop_reason(wire: Option<&str>, category: Option<String>) -> AdapterStopReason {
    match wire {
        Some("end_turn") => AdapterStopReason::EndTurn,
        Some("tool_use") => AdapterStopReason::ToolUse,
        Some("max_tokens") => AdapterStopReason::MaxTokens,
        Some("stop_sequence") => AdapterStopReason::StopSequence,
        Some("pause_turn") => AdapterStopReason::PauseTurn,
        Some("refusal") => AdapterStopReason::Refusal { category },
        // An unrecognized or absent reason on an otherwise-successful stream
        // (the wire client already turns "no stop reason at all" into
        // `AdapterError::Ambiguous` before this is ever called) is treated
        // as a normal end rather than inventing a ceiling or refusal that
        // did not happen.
        _ => AdapterStopReason::EndTurn,
    }
}

fn to_usage_categories(usage: &WireUsage) -> UsageCategories {
    UsageCategories {
        uncached_input_tokens: usage.input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        cache_write_tokens: usage.cache_creation_input_tokens,
        output_tokens: usage.output_tokens,
        // Anthropic bills reasoning/thinking tokens inside `output_tokens`
        // and exposes no separate category — never fabricate a split the
        // provider does not report.
        reasoning_output_tokens: None,
    }
}

#[async_trait]
impl InferenceAdapter for AnthropicAdapter {
    fn runtime_id(&self) -> &str {
        RUNTIME_ID
    }

    async fn submit(
        &self,
        request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError> {
        let api_key = self.credential_resolver.resolve(&self.auth).map_err(|remedy| {
            tracing::error!(remedy = %remedy, "anthropic-api credential resolution failed closed");
            AdapterError::NonRetryable(NonRetryableKind::Auth)
        })?;
        let body = self.build_request_body(request);

        // Index -> (call_id, capability name) for tool_use blocks currently
        // open on the stream; call_id -> accumulated partial JSON so a
        // completed block's full input can be parsed and remembered.
        let mut open_tool_use: HashMap<usize, (String, String)> = HashMap::new();
        let mut tool_use_inputs: HashMap<String, String> = HashMap::new();
        // Same idea for thinking blocks, plus a signature captured
        // separately via `signature_delta`. Finalized (thinking, signature)
        // pairs queue in `pending_thinking` until the next tool_use block
        // completes, at which point they are attached to it and cleared —
        // replaying thinking immediately ahead of the call it led to.
        let mut open_thinking: HashMap<usize, String> = HashMap::new();
        let mut thinking_signatures: HashMap<usize, String> = HashMap::new();
        let mut pending_thinking: Vec<(String, String)> = Vec::new();

        let stream_outcome = self
            .client
            .stream_messages(&api_key, &body, &mut |event: &SseEvent| match event {
                SseEvent::ContentBlockStart {
                    index,
                    content_block,
                } => match content_block {
                    WireContentBlockStart::ToolUse { id, name, .. } => {
                        // `content_block.input` on `content_block_start` is
                        // always the empty-object placeholder `{}` on the
                        // real API — the actual arguments arrive only via
                        // `input_json_delta` fragments below. Treating it as
                        // fragment data would double-count and corrupt the
                        // accumulated JSON.
                        open_tool_use.insert(*index, (id.clone(), name.clone()));
                        tool_use_inputs.insert(id.clone(), String::new());
                    }
                    WireContentBlockStart::Text { text } => {
                        if !text.is_empty() {
                            observer.on_event(StreamEvent::TextDelta(text.clone()));
                        }
                    }
                    WireContentBlockStart::Thinking { thinking } => {
                        open_thinking.insert(*index, thinking.clone());
                    }
                    _ => {}
                },
                SseEvent::ContentBlockDelta { index, delta } => match delta {
                    WireDelta::TextDelta { text } => {
                        observer.on_event(StreamEvent::TextDelta(text.clone()));
                    }
                    WireDelta::InputJsonDelta { partial_json } => {
                        if let Some((call_id, name)) = open_tool_use.get(index) {
                            tool_use_inputs
                                .entry(call_id.clone())
                                .or_default()
                                .push_str(partial_json);
                            observer.on_event(StreamEvent::ToolCallDelta {
                                call_id: call_id.clone(),
                                capability_id: name.clone(),
                                arguments_fragment: partial_json.clone(),
                            });
                        }
                    }
                    WireDelta::ThinkingDelta { thinking } => {
                        open_thinking.entry(*index).or_default().push_str(thinking);
                    }
                    WireDelta::SignatureDelta { signature } => {
                        thinking_signatures.insert(*index, signature.clone());
                    }
                    _ => {}
                },
                SseEvent::ContentBlockStop { index } => {
                    if let Some((call_id, name)) = open_tool_use.remove(index) {
                        let accumulated = tool_use_inputs.remove(&call_id).unwrap_or_default();
                        if let Ok(parsed) = serde_json::from_str::<Value>(&accumulated) {
                            self.tool_use_registry
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(
                                    call_id.clone(),
                                    RememberedToolUse {
                                        name,
                                        input: parsed,
                                        preceding_thinking: std::mem::take(&mut pending_thinking),
                                    },
                                );
                        }
                        // A malformed/incomplete tool-input JSON stream is
                        // not remembered here, but `ToolCallComplete` still
                        // fires — the loop core's own `TurnCollector`
                        // attempts to parse the accumulated fragments and
                        // marks the call `malformed_json` if they don't
                        // parse, which is where refusal-without-execution
                        // actually happens (PRD-058 validation).
                        observer.on_event(StreamEvent::ToolCallComplete { call_id });
                    } else if let Some(thinking) = open_thinking.remove(index) {
                        let signature = thinking_signatures.remove(index).unwrap_or_default();
                        pending_thinking.push((thinking, signature));
                    }
                }
                SseEvent::MessageDelta { usage, .. } => {
                    observer.on_event(StreamEvent::UsageDelta(to_usage_categories(usage)));
                }
                _ => {}
            })
            .await?;

        if let Some(resolved_model) = &stream_outcome.resolved_model {
            self.attempt_metadata
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    request.attempt_id.0.clone(),
                    AttemptMetadata {
                        resolved_model: Some(resolved_model.clone()),
                        refusal_category: stream_outcome.stop_details_category.clone(),
                    },
                );
        }

        let stop_reason = map_stop_reason(
            stream_outcome.stop_reason.as_deref(),
            stream_outcome.stop_details_category.clone(),
        );
        Ok(SubmitOutcome {
            stop_reason,
            usage: to_usage_categories(&stream_outcome.usage),
            provider_request_id: stream_outcome.provider_request_id,
            // Anthropic documents no official idempotency-key replay
            // guarantee for `/v1/messages`; never claim one.
            provider_idempotency_key: None,
        })
    }

    fn cancel(&self, attempt_id: &AttemptId) {
        // Best-effort only: there is no Anthropic-side cancel endpoint, and
        // Familiar never assumes provider-side resumption of an interrupted
        // request. Actual cancellation happens by the caller dropping the
        // in-flight `submit` future (see `raw_runtime::run_loop`'s
        // `tokio::time::timeout`); this just drops local bookkeeping.
        self.attempt_metadata
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&attempt_id.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_llm::attempt::{
        CacheControl, Message, ReasoningControl, StructuredOutputRequest, ToolDefinition,
    };
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Collector {
        events: Vec<StreamEvent>,
    }
    impl StreamObserver for Collector {
        fn on_event(&mut self, event: StreamEvent) {
            self.events.push(event);
        }
    }

    fn adapter_for(server: &MockServer, api_key: &str) -> AnthropicAdapter {
        AnthropicAdapter::with_credential_resolver(
            AnthropicAdapterConfig {
                auth: AuthDescriptor::None,
                http: AnthropicHttpConfig {
                    base_url: server.uri(),
                    ..AnthropicHttpConfig::default()
                },
                ..AnthropicAdapterConfig::default()
            },
            Box::new(familiar_ai_llm::anthropic_api::StaticCredentialResolver(
                api_key.to_string(),
            )),
        )
        .unwrap()
    }

    fn sse_body(frames: &[&str]) -> String {
        frames
            .iter()
            .map(|f| format!("data: {f}\n\n"))
            .collect::<Vec<_>>()
            .join("")
    }

    fn base_request(
        attempt: &str,
        tools: Vec<ToolDefinition>,
        history: Vec<Message>,
    ) -> SubmitRequest {
        let mut messages = vec![
            Message::system("stable prefix"),
            Message::user("do the task"),
        ];
        messages.extend(history);
        SubmitRequest {
            attempt_id: AttemptId(attempt.into()),
            messages,
            model: "claude-test-model".into(),
            tools,
            structured_output: None,
            cache_control: CacheControl::Ephemeral,
            reasoning_control: None,
            prompt_cache_key: Some("prefix-v1".into()),
        }
    }

    #[tokio::test]
    async fn runtime_id_is_anthropic_api() {
        let server = MockServer::start().await;
        let adapter = adapter_for(&server, "sk-test");
        assert_eq!(adapter.runtime_id(), "anthropic-api");
    }

    #[tokio::test]
    async fn streams_text_and_reports_usage_categories() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model-20260101","usage":{"input_tokens":100,"cache_read_input_tokens":40,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello there"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();

        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);
        assert_eq!(outcome.usage.uncached_input_tokens, Some(100));
        assert_eq!(outcome.usage.cache_read_tokens, Some(40));
        assert_eq!(outcome.usage.cache_write_tokens, Some(0));
        assert_eq!(outcome.usage.output_tokens, Some(12));
        assert_eq!(outcome.usage.reasoning_output_tokens, None);
        let text_events: Vec<_> = collector
            .events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text_events.join(""), "Hello there");

        let metadata = adapter.attempt_metadata(&request.attempt_id).unwrap();
        assert_eq!(
            metadata.resolved_model.as_deref(),
            Some("claude-test-model-20260101")
        );
    }

    #[tokio::test]
    async fn projects_tool_definitions_and_streams_tool_call() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let tools = vec![ToolDefinition {
            capability_id: "read-file".into(),
            schema_version: "read-file/1".into(),
            json_schema: r#"{"required":["path"],"optional":[]}"#.into(),
        }];
        let request = base_request("att_1", tools, vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();

        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        let mut call_id = None;
        let mut capability_id = None;
        let mut fragment = String::new();
        let mut completed = false;
        for event in &collector.events {
            match event {
                StreamEvent::ToolCallDelta {
                    call_id: id,
                    capability_id: cap,
                    arguments_fragment,
                } => {
                    call_id = Some(id.clone());
                    capability_id = Some(cap.clone());
                    fragment.push_str(arguments_fragment);
                }
                StreamEvent::ToolCallComplete { call_id: id } => {
                    assert_eq!(call_id.as_deref(), Some(id.as_str()));
                    completed = true;
                }
                _ => {}
            }
        }
        assert_eq!(capability_id.as_deref(), Some("read-file"));
        assert_eq!(fragment, r#"{"path":"a.txt"}"#);
        assert!(completed);

        // The wire request itself must have projected the capability as a
        // real Anthropic tool definition (name + input_schema), not the
        // loop's opaque placeholder string.
        let received = server.received_requests().await.unwrap();
        let sent_body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        let tool = &sent_body["tools"][0];
        assert_eq!(tool["name"], "read-file");
        assert_eq!(tool["input_schema"]["type"], "object");
        assert!(tool["input_schema"]["properties"]["path"].is_object());
        assert_eq!(tool["input_schema"]["required"][0], "path");
    }

    #[tokio::test]
    async fn replays_tool_use_block_on_the_next_turn() {
        let server = MockServer::start().await;
        // First turn: model calls read-file.
        let first = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        // Second turn: model finishes after seeing the tool result.
        let second = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-test-model","usage":{"input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(first, "text/event-stream"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(second, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let tools = vec![ToolDefinition {
            capability_id: "read-file".into(),
            schema_version: "read-file/1".into(),
            json_schema: r#"{"required":["path"],"optional":[]}"#.into(),
        }];
        let first_request = base_request("att_1", tools.clone(), vec![]);
        let mut collector = Collector { events: vec![] };
        adapter
            .submit(&first_request, &mut collector)
            .await
            .unwrap();

        // Now the loop appends the tool_result to history and submits again
        // with a fresh attempt id — the adapter must reconstruct the
        // assistant's tool_use block from what it remembered.
        let history = vec![Message {
            role: MessageRole::Tool,
            content: MessageContent::ToolResult(ToolResultPayload {
                call_id: "toolu_1".into(),
                capability_name: "read-file".into(),
                content: "file contents".into(),
                is_error: false,
            }),
        }];
        let second_request = base_request("att_2", tools, history);
        let mut collector2 = Collector { events: vec![] };
        let outcome = adapter
            .submit(&second_request, &mut collector2)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);

        let received = server.received_requests().await.unwrap();
        let second_sent: serde_json::Value = serde_json::from_slice(&received[1].body).unwrap();
        let messages = second_sent["messages"].as_array().unwrap();
        // Expect: user(task), assistant(tool_use), user(tool_result)
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant tool_use turn must be reconstructed");
        let tool_use_block = assistant_msg["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("assistant turn must carry the replayed tool_use block");
        assert_eq!(tool_use_block["id"], "toolu_1");
        assert_eq!(tool_use_block["name"], "read-file");
        assert_eq!(tool_use_block["input"]["path"], "a.txt");

        let tool_result_msg = messages
            .iter()
            .find(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_array()
                        .is_some_and(|c| c.iter().any(|b| b["type"] == "tool_result"))
            })
            .expect("tool_result must land in a user message");
        let tool_result_block = &tool_result_msg["content"][0];
        assert_eq!(tool_result_block["tool_use_id"], "toolu_1");
        assert_eq!(tool_result_block["content"], "file contents");
    }

    #[tokio::test]
    async fn thinking_block_preceding_a_tool_call_is_replayed_on_the_next_turn() {
        let server = MockServer::start().await;
        let first = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"I should read the file first."}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc123"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        let second = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-test-model","usage":{"input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(first, "text/event-stream"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(second, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let tools = vec![ToolDefinition {
            capability_id: "read-file".into(),
            schema_version: "read-file/1".into(),
            json_schema: r#"{"required":["path"],"optional":[]}"#.into(),
        }];
        let first_request = base_request("att_1", tools.clone(), vec![]);
        let mut collector = Collector { events: vec![] };
        adapter
            .submit(&first_request, &mut collector)
            .await
            .unwrap();

        let history = vec![Message {
            role: MessageRole::Tool,
            content: MessageContent::ToolResult(ToolResultPayload {
                call_id: "toolu_1".into(),
                capability_name: "read-file".into(),
                content: "file contents".into(),
                is_error: false,
            }),
        }];
        let second_request = base_request("att_2", tools, history);
        let mut collector2 = Collector { events: vec![] };
        adapter
            .submit(&second_request, &mut collector2)
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        let second_sent: serde_json::Value = serde_json::from_slice(&received[1].body).unwrap();
        let assistant_msg = second_sent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant turn must be reconstructed");
        let content = assistant_msg["content"].as_array().unwrap();
        // Thinking must precede the tool_use block it led to, and its
        // signature must round-trip verbatim.
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "I should read the file first.");
        assert_eq!(content[0]["signature"], "sig-abc123");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[tokio::test]
    async fn parallel_tool_calls_return_in_one_user_message() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_2","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"b.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let tools = vec![ToolDefinition {
            capability_id: "read-file".into(),
            schema_version: "read-file/1".into(),
            json_schema: r#"{"required":["path"],"optional":[]}"#.into(),
        }];
        let request = base_request("att_1", tools.clone(), vec![]);
        let mut collector = Collector { events: vec![] };
        adapter.submit(&request, &mut collector).await.unwrap();

        let call_ids: Vec<_> = collector
            .events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallComplete { call_id } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(call_ids, vec!["toolu_1", "toolu_2"]);

        let history = vec![
            Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(ToolResultPayload {
                    call_id: "toolu_1".into(),
                    capability_name: "read-file".into(),
                    content: "a-contents".into(),
                    is_error: false,
                }),
            },
            Message {
                role: MessageRole::Tool,
                content: MessageContent::ToolResult(ToolResultPayload {
                    call_id: "toolu_2".into(),
                    capability_name: "read-file".into(),
                    content: "b-contents".into(),
                    is_error: false,
                }),
            },
        ];
        let (_, wire_messages) = adapter.convert_messages(&history, CacheControl::None);
        // Both results must land in a single user message.
        let user_messages: Vec<_> = wire_messages.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_messages.len(), 1);
        assert_eq!(user_messages[0].content.len(), 2);
    }

    #[tokio::test]
    async fn malformed_tool_arguments_surface_as_a_fragment_never_executed_here() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{not-json"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::ToolUse);
        // The adapter forwards exactly what streamed — never invents valid
        // JSON. The loop core's own TurnCollector is what marks this
        // `malformed_json: true` and refuses it without execution.
        let completed = collector
            .events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallComplete { .. }));
        assert!(completed);
        assert!(adapter
            .tool_use_registry
            .lock()
            .unwrap()
            .get("toolu_1")
            .is_none());
    }

    #[tokio::test]
    async fn refused_malformed_call_never_produces_an_orphan_tool_result_on_the_next_turn() {
        let server = MockServer::start().await;
        let first = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{not-json"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        let second = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-test-model","usage":{"input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(first, "text/event-stream"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(second, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let first_request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        adapter
            .submit(&first_request, &mut collector)
            .await
            .unwrap();
        // The malformed call was never registered.
        assert!(adapter
            .tool_use_registry
            .lock()
            .unwrap()
            .get("toolu_1")
            .is_none());

        // The loop core still refuses the call and appends a tool_result
        // for it on the next turn, exactly as it would for any other
        // refused call.
        let history = vec![Message {
            role: MessageRole::Tool,
            content: MessageContent::ToolResult(ToolResultPayload {
                call_id: "toolu_1".into(),
                capability_name: "read-file".into(),
                content: "error: malformed_json".into(),
                is_error: true,
            }),
        }];
        let second_request = base_request("att_2", vec![], history);
        let mut collector2 = Collector { events: vec![] };
        adapter
            .submit(&second_request, &mut collector2)
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        let second_sent: serde_json::Value = serde_json::from_slice(&received[1].body).unwrap();
        let messages = second_sent["messages"].as_array().unwrap();

        // Every tool_result must be paired with a preceding tool_use of the
        // same id — never orphaned, and never directly following another
        // user message.
        for (index, message) in messages.iter().enumerate() {
            let Some(content) = message["content"].as_array() else {
                continue;
            };
            let has_tool_result = content.iter().any(|b| b["type"] == "tool_result");
            if !has_tool_result {
                continue;
            }
            assert_eq!(message["role"], "user");
            let preceding = messages
                .get(
                    index
                        .checked_sub(1)
                        .expect("tool_result must not be the first message"),
                )
                .expect("tool_result must have a preceding message");
            assert_eq!(
                preceding["role"], "assistant",
                "tool_result must be preceded by an assistant message"
            );
            for result_block in content.iter().filter(|b| b["type"] == "tool_result") {
                let call_id = result_block["tool_use_id"].as_str().unwrap();
                let paired = preceding["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["type"] == "tool_use" && b["id"] == call_id);
                assert!(
                    paired,
                    "tool_result for {call_id} must be preceded by a tool_use of the same id"
                );
            }
        }
    }

    #[tokio::test]
    async fn refusal_stops_honestly_without_claiming_completion_or_ceiling() {
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

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();
        assert_eq!(
            outcome.stop_reason,
            AdapterStopReason::Refusal {
                category: Some("cyber".into())
            }
        );
        assert_ne!(outcome.stop_reason, AdapterStopReason::MaxTokens);
        let metadata = adapter.attempt_metadata(&request.attempt_id).unwrap();
        assert_eq!(metadata.refusal_category.as_deref(), Some("cyber"));
    }

    #[tokio::test]
    async fn pause_turn_is_distinct_from_tool_use_and_max_tokens() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":50}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::PauseTurn);
    }

    #[tokio::test]
    async fn missing_usage_stays_unknown_never_zero() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();
        assert!(outcome.usage.is_entirely_unknown());
    }

    #[tokio::test]
    async fn rate_limit_is_retryable_with_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "5")
                    .set_body_json(serde_json::json!({"type":"error","error":{"type":"rate_limit_error","message":"slow down"}})),
            )
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let result = adapter.submit(&request, &mut collector).await;
        match result {
            Err(AdapterError::Retryable(kind)) => {
                assert!(matches!(
                    kind,
                    familiar_ai_llm::attempt::RetryableKind::RateLimited {
                        retry_after_ms: Some(5000)
                    }
                ));
            }
            other => panic!("expected retryable rate-limit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_failure_fails_closed_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"type":"error","error":{"type":"authentication_error","message":"bad key"}})))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-bad");
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let result = adapter.submit(&request, &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
    }

    #[tokio::test]
    async fn missing_credential_fails_closed_with_exact_remedy() {
        let server = MockServer::start().await;
        let missing_env = "PRD059_TEST_ANTHROPIC_KEY_ABSENT_FOR_TEST";
        std::env::remove_var(missing_env);
        let adapter = AnthropicAdapter::new(AnthropicAdapterConfig {
            auth: AuthDescriptor::Env(missing_env.into()),
            http: AnthropicHttpConfig {
                base_url: server.uri(),
                ..AnthropicHttpConfig::default()
            },
            ..AnthropicAdapterConfig::default()
        })
        .unwrap();
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        let result = adapter.submit(&request, &mut collector).await;
        assert!(matches!(
            result,
            Err(AdapterError::NonRetryable(NonRetryableKind::Auth))
        ));
        // No request was ever sent — auth resolution failed before submit.
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cache_control_ephemeral_marks_last_system_block() {
        let server = MockServer::start().await;
        let adapter = adapter_for(&server, "sk-test");
        let (system, _) =
            adapter.convert_messages(&[Message::system("stable prefix")], CacheControl::Ephemeral);
        match &system[0] {
            WireContentBlock::Text { cache_control, .. } => {
                assert_eq!(*cache_control, Some(WireCacheControl::ephemeral()));
            }
            _ => panic!("expected a text block"),
        }
    }

    #[tokio::test]
    async fn cache_control_none_leaves_system_uncached() {
        let server = MockServer::start().await;
        let adapter = adapter_for(&server, "sk-test");
        let (system, _) =
            adapter.convert_messages(&[Message::system("stable prefix")], CacheControl::None);
        match &system[0] {
            WireContentBlock::Text { cache_control, .. } => {
                assert_eq!(*cache_control, None);
            }
            _ => panic!("expected a text block"),
        }
    }

    #[tokio::test]
    async fn structured_output_projects_to_output_config_format() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = adapter_for(&server, "sk-test");
        let mut request = base_request("att_1", vec![], vec![]);
        request.structured_output = Some(StructuredOutputRequest {
            schema_name: "answer".into(),
            json_schema: r#"{"type":"object","properties":{"x":{"type":"string"}}}"#.into(),
        });
        let mut collector = Collector { events: vec![] };
        adapter.submit(&request, &mut collector).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(sent["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            sent["output_config"]["format"]["schema"]["properties"]["x"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn effort_and_thinking_are_configured_from_capability_profile() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = AnthropicAdapter::with_credential_resolver(
            AnthropicAdapterConfig {
                auth: AuthDescriptor::None,
                effort: Some("high".into()),
                thinking_enabled: true,
                http: AnthropicHttpConfig {
                    base_url: server.uri(),
                    ..AnthropicHttpConfig::default()
                },
                ..AnthropicAdapterConfig::default()
            },
            Box::new(familiar_ai_llm::anthropic_api::StaticCredentialResolver(
                "sk-test".into(),
            )),
        )
        .unwrap();
        let request = base_request("att_1", vec![], vec![]);
        let mut collector = Collector { events: vec![] };
        adapter.submit(&request, &mut collector).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(sent["thinking"]["type"], "adaptive");
        assert_eq!(sent["output_config"]["effort"], "high");
    }

    #[tokio::test]
    async fn per_request_reasoning_control_overrides_default_effort() {
        let server = MockServer::start().await;
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;

        let adapter = AnthropicAdapter::with_credential_resolver(
            AnthropicAdapterConfig {
                auth: AuthDescriptor::None,
                effort: Some("low".into()),
                http: AnthropicHttpConfig {
                    base_url: server.uri(),
                    ..AnthropicHttpConfig::default()
                },
                ..AnthropicAdapterConfig::default()
            },
            Box::new(familiar_ai_llm::anthropic_api::StaticCredentialResolver(
                "sk-test".into(),
            )),
        )
        .unwrap();
        let mut request = base_request("att_1", vec![], vec![]);
        request.reasoning_control = Some(ReasoningControl {
            effort: Some("max".into()),
            budget_tokens: None,
        });
        let mut collector = Collector { events: vec![] };
        adapter.submit(&request, &mut collector).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(sent["output_config"]["effort"], "max");
    }

    #[tokio::test]
    async fn cancel_is_best_effort_and_never_panics() {
        let server = MockServer::start().await;
        let adapter = adapter_for(&server, "sk-test");
        adapter.cancel(&AttemptId("att_1".into()));
    }
}
