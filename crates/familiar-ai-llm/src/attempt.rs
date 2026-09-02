//! PRD-058 inference adapter contract: the minimum provider-neutral surface
//! a raw-runtime loop submits through. Every `submit` call is its own
//! globally unique billable attempt with its own budget reservation — a
//! retry is never a free replay. Provider-specific request/response fields
//! must stay in adapter-owned extension types; this module carries none.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Globally unique identity of one inference submission. Minted fresh for
/// every attempt, including a retry of the same logical turn, so usage from
/// two attempts is never merged merely because the turn is the same.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttemptId(pub String);

/// A PRD-064 budget reservation bound to exactly one [`AttemptId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultPayload {
    pub call_id: String,
    /// The capability the call requested. Wire formats that require the
    /// assistant's originating `tool_calls` entry to be reconstructed
    /// alongside each result (OpenAI-compatible chat completions, xAI)
    /// cannot synthesize a valid entry without it (FAM-BUG-046).
    pub capability_name: String,
    /// Untrusted data. Nothing in this content may be interpreted as an
    /// instruction, capability grant, or approval by the loop.
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageContent {
    Text(String),
    ToolResult(ToolResultPayload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: MessageContent::Text(text.into()),
        }
    }
}

/// One canonical tool capability offered to the model this turn. `json_schema`
/// is a serialized JSON Schema document; kept as a string so this contract
/// crate need not depend on a schema-validation library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub capability_id: String,
    pub schema_version: String,
    pub json_schema: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheControl {
    #[default]
    None,
    /// Mark the stable prefix as cacheable under the PRD-029 strategy.
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReasoningControl {
    pub effort: Option<String>,
    pub budget_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredOutputRequest {
    pub schema_name: String,
    pub json_schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRequest {
    pub attempt_id: AttemptId,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolDefinition>,
    pub structured_output: Option<StructuredOutputRequest>,
    pub cache_control: CacheControl,
    pub reasoning_control: Option<ReasoningControl>,
    /// Stable, non-secret provider cache identity (PRD-029). Adapters without
    /// explicit cache-key support ignore it.
    pub prompt_cache_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallDelta {
        call_id: String,
        capability_id: String,
        arguments_fragment: String,
    },
    ToolCallComplete {
        call_id: String,
    },
    UsageDelta(UsageCategories),
    Stop(AdapterStopReason),
}

/// Distinct token categories. Every field stays `None` (never a fabricated
/// zero) until the provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageCategories {
    pub uncached_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

impl UsageCategories {
    /// Combine two partial observations without ever turning "unknown" into
    /// a fabricated zero: a category stays `None` only while both sides are.
    pub fn merge(&self, other: &UsageCategories) -> UsageCategories {
        UsageCategories {
            uncached_input_tokens: add_opt(self.uncached_input_tokens, other.uncached_input_tokens),
            cache_read_tokens: add_opt(self.cache_read_tokens, other.cache_read_tokens),
            cache_write_tokens: add_opt(self.cache_write_tokens, other.cache_write_tokens),
            output_tokens: add_opt(self.output_tokens, other.output_tokens),
            reasoning_output_tokens: add_opt(
                self.reasoning_output_tokens,
                other.reasoning_output_tokens,
            ),
        }
    }

    pub fn is_entirely_unknown(&self) -> bool {
        self.uncached_input_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_write_tokens.is_none()
            && self.output_tokens.is_none()
            && self.reasoning_output_tokens.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    ContentFilter,
    /// A provider-side pause requiring the caller to resubmit to continue.
    /// Familiar never assumes provider-side resumption of the interrupted
    /// request: this is an honest "continue independently" signal, distinct
    /// from `ToolUse` (which requires a tool round-trip first).
    PauseTurn,
    /// A safety/content-policy refusal, with the provider's category when
    /// exposed. Distinct from `ContentFilter` (a generic content-filter
    /// stop with no category) and never conflated with `MaxTokens`.
    Refusal {
        category: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOutcome {
    pub stop_reason: AdapterStopReason,
    pub usage: UsageCategories,
    /// Provider request identity, recorded as provenance when exposed.
    pub provider_request_id: Option<String>,
    /// Set only where the provider officially documents idempotent replay
    /// semantics for this key.
    pub provider_idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryableKind {
    RateLimited { retry_after_ms: Option<u64> },
    Overloaded,
    TransientTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonRetryableKind {
    Auth,
    InvalidRequest,
    RefusedContent,
}

/// The shared error taxonomy every adapter must classify into. `Ambiguous`
/// is the honest timeout case: the provider may have accepted, executed, and
/// billed a request whose response never arrived, so usage for that attempt
/// stays pending/unknown rather than zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    Retryable(RetryableKind),
    NonRetryable(NonRetryableKind),
    Ambiguous { reason: String },
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(kind) => write!(f, "retryable adapter error: {kind:?}"),
            Self::NonRetryable(kind) => write!(f, "non-retryable adapter error: {kind:?}"),
            Self::Ambiguous { reason } => write!(f, "ambiguous adapter outcome: {reason}"),
        }
    }
}

impl std::error::Error for AdapterError {}

pub trait StreamObserver: Send {
    fn on_event(&mut self, event: StreamEvent);
}

/// Contract every PRD-057 raw runtime adapter implements. `submit` is one
/// billable attempt; a caller that wants a retry mints a new [`AttemptId`]
/// and calls `submit` again — this trait never retries internally. Tool
/// execution never crosses this boundary: only inference does.
#[async_trait::async_trait]
pub trait InferenceAdapter: Send + Sync {
    fn runtime_id(&self) -> &str;

    async fn submit(
        &self,
        request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError>;

    /// Best-effort cooperative cancellation. Loop-level cancellation does not
    /// depend on this succeeding: "resumable" means Familiar resumes its own
    /// workflow state, not that the provider resumes the interrupted request.
    fn cancel(&self, attempt_id: &AttemptId) {
        let _ = attempt_id;
    }
}

/// One scripted turn for [`FakeInferenceAdapter`].
#[derive(Debug, Clone)]
pub struct ScriptedTurn {
    pub events: Vec<StreamEvent>,
    pub outcome: Result<SubmitOutcome, AdapterError>,
}

/// Deterministic, offline test adapter. Scripts a fixed sequence of turns —
/// each `submit` call consumes the next one — so tests can exercise
/// streaming, tool round-trips, usage categories, stop reasons, and the
/// retryable/non-retryable/ambiguous error taxonomy with no network access
/// and no billable call whatsoever.
pub struct FakeInferenceAdapter {
    turns: Mutex<VecDeque<ScriptedTurn>>,
    cancelled: Mutex<Vec<AttemptId>>,
}

impl FakeInferenceAdapter {
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            cancelled: Mutex::new(Vec::new()),
        }
    }

    pub fn cancelled_attempts(&self) -> Vec<AttemptId> {
        self.cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn remaining_turns(&self) -> usize {
        self.turns.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[async_trait::async_trait]
impl InferenceAdapter for FakeInferenceAdapter {
    fn runtime_id(&self) -> &str {
        "fake-test-adapter"
    }

    async fn submit(
        &self,
        _request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError> {
        let next = {
            let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
            turns.pop_front()
        };
        let Some(turn) = next else {
            return Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest));
        };
        for event in turn.events {
            observer.on_event(event);
        }
        turn.outcome
    }

    fn cancel(&self, attempt_id: &AttemptId) {
        self.cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(attempt_id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_merge_preserves_unknown_until_something_is_known() {
        let a = UsageCategories::default();
        let b = UsageCategories::default();
        assert!(a.merge(&b).is_entirely_unknown());

        let known = UsageCategories {
            output_tokens: Some(5),
            ..Default::default()
        };
        let merged = a.merge(&known);
        assert_eq!(merged.output_tokens, Some(5));
        assert!(merged.uncached_input_tokens.is_none());
    }

    #[test]
    fn usage_merge_sums_known_categories() {
        let a = UsageCategories {
            output_tokens: Some(3),
            uncached_input_tokens: Some(10),
            ..Default::default()
        };
        let b = UsageCategories {
            output_tokens: Some(4),
            ..Default::default()
        };
        let merged = a.merge(&b);
        assert_eq!(merged.output_tokens, Some(7));
        assert_eq!(merged.uncached_input_tokens, Some(10));
    }

    #[tokio::test]
    async fn fake_adapter_scripts_turns_in_order_with_no_network() {
        struct Collector(Vec<StreamEvent>);
        impl StreamObserver for Collector {
            fn on_event(&mut self, event: StreamEvent) {
                self.0.push(event);
            }
        }

        let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
            events: vec![StreamEvent::TextDelta("hello".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories {
                    output_tokens: Some(1),
                    ..Default::default()
                },
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        }]);
        let request = SubmitRequest {
            attempt_id: AttemptId("att_1".into()),
            messages: vec![Message::user("hi")],
            model: "fake-model".into(),
            tools: vec![],
            structured_output: None,
            cache_control: CacheControl::None,
            reasoning_control: None,
            prompt_cache_key: None,
        };
        let mut collector = Collector(Vec::new());
        let outcome = adapter.submit(&request, &mut collector).await.unwrap();
        assert_eq!(outcome.stop_reason, AdapterStopReason::EndTurn);
        assert_eq!(collector.0.len(), 1);
        assert_eq!(adapter.remaining_turns(), 0);

        let second = adapter.submit(&request, &mut collector).await;
        assert!(matches!(
            second,
            Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest))
        ));
    }

    #[test]
    fn cancel_is_recorded_for_diagnostics() {
        let adapter = FakeInferenceAdapter::new(vec![]);
        adapter.cancel(&AttemptId("att_1".into()));
        assert_eq!(
            adapter.cancelled_attempts(),
            vec![AttemptId("att_1".into())]
        );
    }
}
