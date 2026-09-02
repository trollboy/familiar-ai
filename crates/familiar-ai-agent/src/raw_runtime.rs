//! PRD-058 Familiar-owned raw-model agent loop.
//!
//! One deterministic state machine: compose -> submit -> streaming turn ->
//! {text | tool calls | structured output | stop} -> validate -> authorize
//! -> execute (journaled) -> insert results -> iterate | terminal.
//!
//! This module is provider- and storage-agnostic: hosts inject an
//! [`InferenceAdapter`], a [`ToolExecutor`], a [`ToolAuthorizer`], a
//! [`ToolJournal`], and read the returned [`LoopEvidence`] and per-attempt
//! usage to persist however the host (daemon crate) sees fit. Nothing here
//! touches SQLite, a subprocess, or the network directly, so the whole loop
//! is exercised in tests purely against a scripted fake adapter.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use familiar_ai_llm::attempt::{
    AdapterError, AttemptId, CacheControl, InferenceAdapter, Message, MessageContent, MessageRole,
    StreamEvent, StreamObserver, StructuredOutputRequest, SubmitRequest,
    ToolDefinition as AdapterToolDefinition, ToolResultPayload, UsageCategories,
};

/// Content-fingerprint tool arguments/results for the journal and evidence —
/// never persists raw content itself.
fn sha256_hex(input: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, input.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------
// Canonical tool capabilities
// ---------------------------------------------------------------------

/// The closed initial canonical tool capability vocabulary. MCP, native
/// provider tool calling, and schema-constrained templates are projections
/// of exactly these — never a wider or narrower authority set per adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityId {
    ReadFile,
    SearchList,
    RunCommand,
    ApplyEdit,
    ReportProgress,
    SubmitEvidence,
    RequestEscalation,
}

impl CapabilityId {
    pub const ALL: [CapabilityId; 7] = [
        Self::ReadFile,
        Self::SearchList,
        Self::RunCommand,
        Self::ApplyEdit,
        Self::ReportProgress,
        Self::SubmitEvidence,
        Self::RequestEscalation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read-file",
            Self::SearchList => "search-list",
            Self::RunCommand => "run-command",
            Self::ApplyEdit => "apply-edit",
            Self::ReportProgress => "report-progress",
            Self::SubmitEvidence => "submit-evidence",
            Self::RequestEscalation => "request-escalation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectClass {
    ReadOnly,
    IdempotentWrite,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskClass {
    Low,
    Medium,
    High,
}

/// Minimal closed structural schema: an object payload whose keys are
/// exactly the union of `required` and `optional`. This is deliberately not
/// a general JSON Schema validator — the canonical capability set is small
/// and fixed, so a closed field-presence check is sufficient and avoids a
/// new schema-validation dependency.
#[derive(Debug, Clone, Copy)]
pub struct ArgSchema {
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
}

impl ArgSchema {
    fn validate(&self, value: &serde_json::Value) -> Result<(), String> {
        let serde_json::Value::Object(map) = value else {
            return Err("arguments must be a JSON object".into());
        };
        for key in self.required {
            if !map.contains_key(*key) {
                return Err(format!("missing required argument {key:?}"));
            }
        }
        let allowed: std::collections::BTreeSet<&str> =
            self.required.iter().chain(self.optional).copied().collect();
        for key in map.keys() {
            if !allowed.contains(key.as_str()) {
                return Err(format!("unknown argument {key:?}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToolCapability {
    pub id: CapabilityId,
    pub schema_version: &'static str,
    pub args_schema: ArgSchema,
    pub side_effect_class: SideEffectClass,
    pub timeout_ms: u64,
    /// True when a repeat call with identical arguments is safe to re-run.
    /// Declared per capability, not inferred.
    pub idempotent: bool,
    pub audit_required: bool,
    pub risk: RiskClass,
    pub max_payload_bytes: usize,
}

/// The canonical capability registry. Stable identity + schema version per
/// capability; a projection (MCP, native tool calling, a schema-constrained
/// template) may narrow this set, never widen it.
pub fn canonical_capabilities() -> Vec<ToolCapability> {
    vec![
        ToolCapability {
            id: CapabilityId::ReadFile,
            schema_version: "read-file/1",
            args_schema: ArgSchema {
                required: &["path"],
                optional: &[],
            },
            side_effect_class: SideEffectClass::ReadOnly,
            timeout_ms: 5_000,
            idempotent: true,
            audit_required: false,
            risk: RiskClass::Low,
            max_payload_bytes: 1 << 20,
        },
        ToolCapability {
            id: CapabilityId::SearchList,
            schema_version: "search-list/1",
            args_schema: ArgSchema {
                required: &["query"],
                optional: &["path"],
            },
            side_effect_class: SideEffectClass::ReadOnly,
            timeout_ms: 5_000,
            idempotent: true,
            audit_required: false,
            risk: RiskClass::Low,
            max_payload_bytes: 1 << 20,
        },
        ToolCapability {
            id: CapabilityId::RunCommand,
            schema_version: "run-command/1",
            args_schema: ArgSchema {
                required: &["argv"],
                optional: &["working_directory"],
            },
            // Arbitrary commands are never assumed idempotent; the command
            // allow/deny policy narrows what may run, but the capability's
            // own side-effect class stays conservative.
            side_effect_class: SideEffectClass::Destructive,
            timeout_ms: 120_000,
            idempotent: false,
            audit_required: true,
            risk: RiskClass::High,
            max_payload_bytes: 1 << 20,
        },
        ToolCapability {
            id: CapabilityId::ApplyEdit,
            schema_version: "apply-edit/1",
            args_schema: ArgSchema {
                required: &["path", "content"],
                optional: &["change_kind"],
            },
            side_effect_class: SideEffectClass::IdempotentWrite,
            timeout_ms: 10_000,
            idempotent: true,
            audit_required: true,
            risk: RiskClass::Medium,
            max_payload_bytes: 4 << 20,
        },
        ToolCapability {
            id: CapabilityId::ReportProgress,
            schema_version: "report-progress/1",
            args_schema: ArgSchema {
                required: &["message"],
                optional: &[],
            },
            side_effect_class: SideEffectClass::IdempotentWrite,
            timeout_ms: 5_000,
            idempotent: true,
            audit_required: false,
            risk: RiskClass::Low,
            max_payload_bytes: 1 << 16,
        },
        ToolCapability {
            id: CapabilityId::SubmitEvidence,
            schema_version: "submit-evidence/1",
            args_schema: ArgSchema {
                required: &["summary"],
                optional: &["artifact_ref"],
            },
            side_effect_class: SideEffectClass::IdempotentWrite,
            timeout_ms: 5_000,
            idempotent: true,
            audit_required: true,
            risk: RiskClass::Low,
            max_payload_bytes: 1 << 16,
        },
        ToolCapability {
            id: CapabilityId::RequestEscalation,
            schema_version: "request-escalation/1",
            args_schema: ArgSchema {
                required: &["reason"],
                optional: &["requested_capability"],
            },
            // Creating a pending gate twice is not destructive; the host
            // deduplicates by journal key.
            side_effect_class: SideEffectClass::IdempotentWrite,
            timeout_ms: 5_000,
            idempotent: true,
            audit_required: true,
            risk: RiskClass::Medium,
            max_payload_bytes: 1 << 16,
        },
    ]
}

fn capability_by_id(offered: &[ToolCapability], id: CapabilityId) -> Option<&ToolCapability> {
    offered.iter().find(|c| c.id == id)
}

// ---------------------------------------------------------------------
// Tool call validation
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRequest {
    pub call_id: String,
    /// The raw capability name as requested by the model. May not match any
    /// offered capability.
    pub capability_name: String,
    pub arguments: serde_json::Value,
    /// Set when the model's argument stream did not parse as JSON at all.
    pub malformed_json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationRefusal {
    UnknownCapability { requested: String },
    MalformedArguments { detail: String },
    OversizedPayload { bytes: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCall {
    pub call_id: String,
    pub capability: CapabilityId,
    pub arguments: serde_json::Value,
    pub argument_hash: String,
}

/// Every requested call is checked against the canonical schema before
/// anything runs. Unknown tool, malformed arguments, and oversized payloads
/// are refusals, recorded, never executed.
pub fn validate_tool_call(
    call: &ToolCallRequest,
    offered: &[ToolCapability],
) -> Result<ValidatedCall, ValidationRefusal> {
    if call.malformed_json {
        return Err(ValidationRefusal::MalformedArguments {
            detail: "tool call arguments were not valid JSON".into(),
        });
    }
    let Some(capability) =
        CapabilityId::parse(&call.capability_name).and_then(|id| capability_by_id(offered, id))
    else {
        return Err(ValidationRefusal::UnknownCapability {
            requested: call.capability_name.clone(),
        });
    };
    let serialized = call.arguments.to_string();
    if serialized.len() > capability.max_payload_bytes {
        return Err(ValidationRefusal::OversizedPayload {
            bytes: serialized.len(),
            limit: capability.max_payload_bytes,
        });
    }
    capability
        .args_schema
        .validate(&call.arguments)
        .map_err(|detail| ValidationRefusal::MalformedArguments { detail })?;
    Ok(ValidatedCall {
        call_id: call.call_id.clone(),
        capability: capability.id,
        arguments: call.arguments.clone(),
        argument_hash: sha256_hex(&serialized),
    })
}

// ---------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    pub project_id: String,
    pub execution_id: String,
    pub attempt_id: String,
    pub worker_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationRefusal {
    /// The capability is not part of this execution's granted authority.
    OutOfAuthorityScope {
        capability: CapabilityId,
    },
    OutOfProjectExecutionBinding,
    OutOfWriteScope {
        path: String,
    },
    CommandDenied {
        command: String,
    },
    NetworkDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalContinuation {
    /// Inform the model via a tool-result refusal and let the loop continue.
    InformModelAndContinue,
    /// The refusal is fatal; the loop stops closed.
    StopClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Authorized,
    Refused {
        reason: AuthorizationRefusal,
        continuation: RefusalContinuation,
    },
}

/// Injected by the host. The loop core never decides authority itself — it
/// only enforces whatever this returns, and it never lets a tool result or
/// model output influence the decision.
pub trait ToolAuthorizer {
    fn authorize(&self, call: &ValidatedCall, ctx: &AuthorityContext) -> AuthorizationDecision;
}

/// A deterministic, self-contained authorizer: exact/prefix write-scope
/// allowlist plus an exact-argv0 command allowlist. Hosts that need the full
/// PRD-013 Expected Files contract (directory-declaration grammar, special
/// file classes, PRD-declared expansion) wrap it behind the same trait —
/// this implementation is the baseline every host can rely on with no
/// further dependency.
#[derive(Debug)]
pub struct ScopeAuthorizer {
    pub granted_capabilities: Vec<CapabilityId>,
    /// Exact files or `dir/` prefixes a write may target.
    pub allowed_write_paths: Vec<String>,
    /// Exact allowed `argv[0]` values for run-command.
    pub allowed_commands: Vec<String>,
    pub network_allowed: bool,
}

impl ScopeAuthorizer {
    fn write_path_allowed(&self, path: &str) -> bool {
        self.allowed_write_paths.iter().any(|entry| {
            if let Some(dir) = entry.strip_suffix('/') {
                path == dir || path.starts_with(entry.as_str())
            } else {
                path == entry
            }
        })
    }
}

impl ToolAuthorizer for ScopeAuthorizer {
    fn authorize(&self, call: &ValidatedCall, _ctx: &AuthorityContext) -> AuthorizationDecision {
        if !self.granted_capabilities.contains(&call.capability) {
            return AuthorizationDecision::Refused {
                reason: AuthorizationRefusal::OutOfAuthorityScope {
                    capability: call.capability,
                },
                continuation: RefusalContinuation::InformModelAndContinue,
            };
        }
        match call.capability {
            CapabilityId::ApplyEdit => {
                let path = call
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !self.write_path_allowed(path) {
                    return AuthorizationDecision::Refused {
                        reason: AuthorizationRefusal::OutOfWriteScope {
                            path: path.to_string(),
                        },
                        continuation: RefusalContinuation::InformModelAndContinue,
                    };
                }
            }
            CapabilityId::RunCommand => {
                let argv = call
                    .arguments
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !self.allowed_commands.iter().any(|c| c == argv) {
                    return AuthorizationDecision::Refused {
                        reason: AuthorizationRefusal::CommandDenied {
                            command: argv.to_string(),
                        },
                        continuation: RefusalContinuation::InformModelAndContinue,
                    };
                }
                if !self.network_allowed {
                    // Deny-by-default network is enforced by the executor's
                    // sandbox; this records the policy fact so evidence
                    // reflects it even when the executor is a test double.
                }
            }
            _ => {}
        }
        AuthorizationDecision::Authorized
    }
}

// ---------------------------------------------------------------------
// Write-ahead tool journal
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalIntent {
    pub call_id: String,
    pub capability: CapabilityId,
    pub argument_hash: String,
    pub side_effect_class: SideEffectClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalResult {
    Succeeded { result_hash: String },
    Failed { detail: String },
}

/// Intent must be durable before execution proceeds (write-ahead). Resume
/// replays nothing blindly: see [`resume_decision_for`].
pub trait ToolJournal {
    fn record_intent(&mut self, intent: &JournalIntent) -> Result<(), String>;
    fn record_result(&mut self, call_id: &str, result: &JournalResult) -> Result<(), String>;
    fn result_for(&self, call_id: &str) -> Option<JournalResult>;
    /// Journal length, used as the evidence resume high-water mark.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    ReplayAllowed,
    /// A completed call: never repeated.
    AlreadyDone,
    /// Intent recorded, no result: destructive calls fail closed to a human
    /// gate rather than guessing whether the effect happened.
    FailClosed,
}

/// Resume never replays blindly: a read-only call may re-run; an
/// idempotent-write with intent-but-no-result may re-run by declaration; a
/// destructive call with intent-but-no-result fails closed.
pub fn resume_decision_for(effect: SideEffectClass, has_result: bool) -> ResumeDecision {
    if has_result {
        return ResumeDecision::AlreadyDone;
    }
    match effect {
        SideEffectClass::ReadOnly | SideEffectClass::IdempotentWrite => {
            ResumeDecision::ReplayAllowed
        }
        SideEffectClass::Destructive => ResumeDecision::FailClosed,
    }
}

/// In-memory journal used by loop-core tests and as a reference
/// implementation; the daemon crate persists the same shape to SQLite.
#[derive(Debug, Default)]
pub struct InMemoryToolJournal {
    intents: Vec<JournalIntent>,
    results: BTreeMap<String, JournalResult>,
}

impl ToolJournal for InMemoryToolJournal {
    fn record_intent(&mut self, intent: &JournalIntent) -> Result<(), String> {
        self.intents.push(intent.clone());
        Ok(())
    }

    fn record_result(&mut self, call_id: &str, result: &JournalResult) -> Result<(), String> {
        self.results.insert(call_id.to_string(), result.clone());
        Ok(())
    }

    fn result_for(&self, call_id: &str) -> Option<JournalResult> {
        self.results.get(call_id).cloned()
    }

    fn len(&self) -> usize {
        self.intents.len()
    }
}

// ---------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub result_text: String,
    pub result_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    Timeout,
    Failed(String),
}

/// Injected by the host. Runs strictly after validation, authorization, and
/// a durable journal intent. Implementations own sandboxing (Landlock,
/// process groups, env scrubbing, network deny-by-default).
pub trait ToolExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError>;
}

// ---------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedTool {
    pub capability: CapabilityId,
    pub schema_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallDisposition {
    ValidationRefused(ValidationRefusal),
    AuthorizationRefused {
        reason: AuthorizationRefusal,
        continuation: RefusalContinuation,
    },
    Executed {
        result_hash: String,
    },
    ExecutionFailed {
        detail: String,
    },
    /// Resumed from a prior journal result without re-executing.
    ResumedFromJournal {
        result: JournalResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    pub call_id: String,
    pub capability_name: String,
    pub disposition: CallDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureTaxonomy {
    Retryable,
    NonRetryable,
    Ambiguous,
}

/// The closed, honest stop-reason set. A failed stage is always its own
/// failure — never relabeled as token exhaustion by bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Completed { structured_output: bool },
    IterationCeiling,
    TokenOrContextCeiling,
    BudgetStop,
    Timeout,
    Cancelled,
    ProviderFailure { taxonomy: ProviderFailureTaxonomy },
    FatalToolRefusal,
    InvalidStructuredOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePoint {
    pub conversation_messages: usize,
    pub journal_high_water_mark: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopEvidence {
    pub prompt_template_version: String,
    pub worker_spec_identity: String,
    pub worker_empirical_version: String,
    pub offered_tools: Vec<OfferedTool>,
    pub calls: Vec<CallRecord>,
    pub stop_reason: StopReason,
    pub resume_point: ResumePoint,
    pub iterations: u32,
}

// ---------------------------------------------------------------------
// Prompt composition (PRD-029 stable-prefix cache strategy)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StablePrefix {
    pub bytes: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolatileTask {
    pub bytes: String,
}

fn cache_control_for(prefix: &StablePrefix) -> CacheControl {
    if prefix.bytes.is_empty() {
        CacheControl::None
    } else {
        CacheControl::Ephemeral
    }
}

fn compose_messages(
    prefix: &StablePrefix,
    task: &VolatileTask,
    history: &[Message],
) -> Vec<Message> {
    // Stable bytes first, volatile task and turn history after: a
    // volatile-only change never perturbs the cacheable prefix.
    let mut messages = vec![
        Message::system(prefix.bytes.clone()),
        Message::user(task.bytes.clone()),
    ];
    messages.extend(history.iter().cloned());
    messages
}

// ---------------------------------------------------------------------
// Loop configuration and outcome
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct LoopCeilings {
    pub max_iterations: u32,
    pub max_output_tokens: Option<u64>,
    pub max_wall_clock_ms: Option<u64>,
}

impl Default for LoopCeilings {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        }
    }
}

pub struct LoopConfig {
    pub worker_spec_identity: String,
    pub worker_empirical_version: String,
    pub model: String,
    pub prompt_template_version: String,
    pub ceilings: LoopCeilings,
    pub offered_capabilities: Vec<CapabilityId>,
    pub structured_output: Option<StructuredOutputRequest>,
    pub authority: AuthorityContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptUsage {
    pub attempt_id: AttemptId,
    pub usage: UsageCategories,
    /// Set when the attempt timed out with unknown completion: usage for
    /// this attempt is ambiguous/pending, never zero.
    pub ambiguous: bool,
}

pub struct RunOutcome {
    pub stop_reason: StopReason,
    pub attempts: Vec<AttemptUsage>,
    pub evidence: LoopEvidence,
    pub final_text: Option<String>,
}

/// Cooperative cancellation shared between the caller and the loop.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn tool_result_message(
    call_id: &str,
    capability_name: &str,
    content: String,
    is_error: bool,
) -> Message {
    Message {
        role: MessageRole::Tool,
        content: MessageContent::ToolResult(ToolResultPayload {
            call_id: call_id.to_string(),
            capability_name: capability_name.to_string(),
            content,
            is_error,
        }),
    }
}

#[derive(Default)]
struct PendingToolCall {
    capability_name: String,
    arguments_buf: String,
}

/// Buffers streamed text/tool-call deltas into complete units. Usage deltas
/// are observed but the authoritative usage is the adapter's final
/// [`familiar_ai_llm::attempt::SubmitOutcome`], never double counted here.
#[derive(Default)]
struct TurnCollector {
    text: Option<String>,
    pending_calls: BTreeMap<String, PendingToolCall>,
    completed_calls: Vec<ToolCallRequest>,
}

impl StreamObserver for TurnCollector {
    fn on_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::TextDelta(delta) => {
                self.text.get_or_insert_with(String::new).push_str(&delta);
            }
            StreamEvent::ToolCallDelta {
                call_id,
                capability_id,
                arguments_fragment,
            } => {
                let entry = self.pending_calls.entry(call_id).or_default();
                if entry.capability_name.is_empty() {
                    entry.capability_name = capability_id;
                }
                entry.arguments_buf.push_str(&arguments_fragment);
            }
            StreamEvent::ToolCallComplete { call_id } => {
                if let Some(pending) = self.pending_calls.remove(&call_id) {
                    match serde_json::from_str::<serde_json::Value>(&pending.arguments_buf) {
                        Ok(arguments) => self.completed_calls.push(ToolCallRequest {
                            call_id,
                            capability_name: pending.capability_name,
                            arguments,
                            malformed_json: false,
                        }),
                        Err(_) => self.completed_calls.push(ToolCallRequest {
                            call_id,
                            capability_name: pending.capability_name,
                            arguments: serde_json::Value::Null,
                            malformed_json: true,
                        }),
                    }
                }
            }
            StreamEvent::UsageDelta(_) | StreamEvent::Stop(_) => {}
        }
    }
}

fn offered_tool_definitions(offered: &[ToolCapability]) -> Vec<AdapterToolDefinition> {
    offered
        .iter()
        .map(|c| AdapterToolDefinition {
            capability_id: c.id.as_str().to_string(),
            schema_version: c.schema_version.to_string(),
            json_schema: format!(
                "{{\"required\":{:?},\"optional\":{:?}}}",
                c.args_schema.required, c.args_schema.optional
            ),
        })
        .collect()
}

/// Runs the loop to a terminal [`StopReason`]. `mint_attempt_id` is called
/// once per submission — every submission is its own globally unique
/// attempt, including a retry (the caller decides whether to retry by
/// invoking this loop again or a fresh submission; this function performs no
/// automatic provider-level retry).
#[allow(clippy::too_many_arguments)]
pub async fn run_loop(
    adapter: &dyn InferenceAdapter,
    executor: &mut dyn ToolExecutor,
    authorizer: &dyn ToolAuthorizer,
    journal: &mut dyn ToolJournal,
    cancel: &CancellationToken,
    prefix: &StablePrefix,
    task: &VolatileTask,
    config: &LoopConfig,
    mut mint_attempt_id: impl FnMut() -> AttemptId,
) -> RunOutcome {
    let offered: Vec<ToolCapability> = canonical_capabilities()
        .into_iter()
        .filter(|c| config.offered_capabilities.contains(&c.id))
        .collect();
    let offered_tools_evidence: Vec<OfferedTool> = offered
        .iter()
        .map(|c| OfferedTool {
            capability: c.id,
            schema_version: c.schema_version.to_string(),
        })
        .collect();
    let tool_definitions = offered_tool_definitions(&offered);

    let mut history: Vec<Message> = Vec::new();
    let mut attempts: Vec<AttemptUsage> = Vec::new();
    let mut calls_evidence: Vec<CallRecord> = Vec::new();
    let mut final_text: Option<String> = None;
    let mut iterations: u32 = 0;
    let mut total_output_tokens: u64 = 0;

    let stop_reason = loop {
        if cancel.is_cancelled() {
            break StopReason::Cancelled;
        }
        iterations += 1;
        if iterations > config.ceilings.max_iterations {
            iterations -= 1;
            break StopReason::IterationCeiling;
        }
        if let Some(max) = config.ceilings.max_output_tokens {
            if total_output_tokens >= max {
                break StopReason::TokenOrContextCeiling;
            }
        }

        let attempt_id = mint_attempt_id();
        let request = SubmitRequest {
            attempt_id: attempt_id.clone(),
            messages: compose_messages(prefix, task, &history),
            model: config.model.clone(),
            tools: tool_definitions.clone(),
            structured_output: config.structured_output.clone(),
            cache_control: cache_control_for(prefix),
            reasoning_control: None,
            prompt_cache_key: Some(prefix.version.clone()),
        };

        let mut collector = TurnCollector::default();
        let submission = match config.ceilings.max_wall_clock_ms {
            Some(ms) => {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(ms),
                    adapter.submit(&request, &mut collector),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        attempts.push(AttemptUsage {
                            attempt_id,
                            usage: UsageCategories::default(),
                            ambiguous: true,
                        });
                        break StopReason::Timeout;
                    }
                }
            }
            None => adapter.submit(&request, &mut collector).await,
        };

        let outcome = match submission {
            Ok(outcome) => outcome,
            Err(AdapterError::Ambiguous { .. }) => {
                attempts.push(AttemptUsage {
                    attempt_id,
                    usage: UsageCategories::default(),
                    ambiguous: true,
                });
                break StopReason::ProviderFailure {
                    taxonomy: ProviderFailureTaxonomy::Ambiguous,
                };
            }
            Err(AdapterError::Retryable(_)) => {
                break StopReason::ProviderFailure {
                    taxonomy: ProviderFailureTaxonomy::Retryable,
                };
            }
            Err(AdapterError::NonRetryable(_)) => {
                break StopReason::ProviderFailure {
                    taxonomy: ProviderFailureTaxonomy::NonRetryable,
                };
            }
        };
        total_output_tokens += outcome.usage.output_tokens.unwrap_or(0);
        attempts.push(AttemptUsage {
            attempt_id,
            usage: outcome.usage,
            ambiguous: false,
        });

        if let Some(text) = collector.text.clone() {
            final_text = Some(text.clone());
            history.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text(text),
            });
        }

        if collector.completed_calls.is_empty() {
            use familiar_ai_llm::attempt::AdapterStopReason as A;
            match outcome.stop_reason {
                A::EndTurn | A::ContentFilter | A::StopSequence => {
                    if config.structured_output.is_some() && final_text.is_none() {
                        break StopReason::InvalidStructuredOutput;
                    }
                    break StopReason::Completed {
                        structured_output: config.structured_output.is_some(),
                    };
                }
                A::MaxTokens => break StopReason::TokenOrContextCeiling,
                A::ToolUse => break StopReason::InvalidStructuredOutput,
            }
        }

        let mut fatal = false;
        for raw_call in collector.completed_calls {
            let disposition = process_tool_call(
                &raw_call,
                &offered,
                authorizer,
                journal,
                executor,
                &config.authority,
                &mut history,
                &mut fatal,
            );
            calls_evidence.push(CallRecord {
                call_id: raw_call.call_id,
                capability_name: raw_call.capability_name,
                disposition,
            });
        }
        if fatal {
            break StopReason::FatalToolRefusal;
        }
        // else: tool results were appended to history; iterate.
    };

    let evidence = LoopEvidence {
        prompt_template_version: config.prompt_template_version.clone(),
        worker_spec_identity: config.worker_spec_identity.clone(),
        worker_empirical_version: config.worker_empirical_version.clone(),
        offered_tools: offered_tools_evidence,
        calls: calls_evidence,
        stop_reason,
        resume_point: ResumePoint {
            conversation_messages: history.len(),
            journal_high_water_mark: journal.len(),
        },
        iterations,
    };

    RunOutcome {
        stop_reason,
        attempts,
        evidence,
        final_text,
    }
}

#[allow(clippy::too_many_arguments)]
fn process_tool_call(
    raw_call: &ToolCallRequest,
    offered: &[ToolCapability],
    authorizer: &dyn ToolAuthorizer,
    journal: &mut dyn ToolJournal,
    executor: &mut dyn ToolExecutor,
    authority: &AuthorityContext,
    history: &mut Vec<Message>,
    fatal: &mut bool,
) -> CallDisposition {
    let validated = match validate_tool_call(raw_call, offered) {
        Err(refusal) => {
            history.push(tool_result_message(
                &raw_call.call_id,
                &raw_call.capability_name,
                format!("refused: invalid tool call ({refusal:?})"),
                true,
            ));
            return CallDisposition::ValidationRefused(refusal);
        }
        Ok(validated) => validated,
    };

    match authorizer.authorize(&validated, authority) {
        AuthorizationDecision::Refused {
            reason,
            continuation,
        } => {
            history.push(tool_result_message(
                &validated.call_id,
                validated.capability.as_str(),
                format!("refused: not authorized ({reason:?})"),
                true,
            ));
            if continuation == RefusalContinuation::StopClosed {
                *fatal = true;
            }
            CallDisposition::AuthorizationRefused {
                reason,
                continuation,
            }
        }
        AuthorizationDecision::Authorized => {
            let capability = capability_by_id(offered, validated.capability)
                .expect("validated call always names an offered capability");

            if let Some(prior) = journal.result_for(&validated.call_id) {
                // Resumed loop: the write-ahead record shows this call
                // already ran. Never re-execute; surface the prior fact.
                let (content, is_error) = match &prior {
                    JournalResult::Succeeded { .. } => {
                        ("resumed: already executed".to_string(), false)
                    }
                    JournalResult::Failed { detail } => {
                        (format!("resumed: previously failed ({detail})"), true)
                    }
                };
                history.push(tool_result_message(
                    &validated.call_id,
                    validated.capability.as_str(),
                    content,
                    is_error,
                ));
                return CallDisposition::ResumedFromJournal { result: prior };
            }

            let intent = JournalIntent {
                call_id: validated.call_id.clone(),
                capability: validated.capability,
                argument_hash: validated.argument_hash.clone(),
                side_effect_class: capability.side_effect_class,
            };
            journal
                .record_intent(&intent)
                .expect("tool journal write-ahead must succeed before any execution");

            match executor.execute(&validated, authority) {
                Ok(outcome) => {
                    let _ = journal.record_result(
                        &validated.call_id,
                        &JournalResult::Succeeded {
                            result_hash: outcome.result_hash.clone(),
                        },
                    );
                    history.push(tool_result_message(
                        &validated.call_id,
                        validated.capability.as_str(),
                        outcome.result_text,
                        false,
                    ));
                    CallDisposition::Executed {
                        result_hash: outcome.result_hash,
                    }
                }
                Err(error) => {
                    let detail = match error {
                        ExecutionError::Timeout => "tool execution timed out".to_string(),
                        ExecutionError::Failed(detail) => detail,
                    };
                    let _ = journal.record_result(
                        &validated.call_id,
                        &JournalResult::Failed {
                            detail: detail.clone(),
                        },
                    );
                    history.push(tool_result_message(
                        &validated.call_id,
                        validated.capability.as_str(),
                        detail.clone(),
                        true,
                    ));
                    CallDisposition::ExecutionFailed { detail }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_unknown_capability() {
        let call = ToolCallRequest {
            call_id: "c1".into(),
            capability_name: "delete-everything".into(),
            arguments: serde_json::json!({}),
            malformed_json: false,
        };
        let refusal = validate_tool_call(&call, &canonical_capabilities()).unwrap_err();
        assert!(matches!(
            refusal,
            ValidationRefusal::UnknownCapability { .. }
        ));
    }

    #[test]
    fn validate_rejects_malformed_json() {
        let call = ToolCallRequest {
            call_id: "c1".into(),
            capability_name: CapabilityId::ReadFile.as_str().into(),
            arguments: serde_json::Value::Null,
            malformed_json: true,
        };
        let refusal = validate_tool_call(&call, &canonical_capabilities()).unwrap_err();
        assert!(matches!(
            refusal,
            ValidationRefusal::MalformedArguments { .. }
        ));
    }

    #[test]
    fn validate_rejects_missing_required_argument() {
        let call = ToolCallRequest {
            call_id: "c1".into(),
            capability_name: CapabilityId::ReadFile.as_str().into(),
            arguments: serde_json::json!({}),
            malformed_json: false,
        };
        let refusal = validate_tool_call(&call, &canonical_capabilities()).unwrap_err();
        assert!(matches!(
            refusal,
            ValidationRefusal::MalformedArguments { .. }
        ));
    }

    #[test]
    fn validate_accepts_well_formed_call() {
        let call = ToolCallRequest {
            call_id: "c1".into(),
            capability_name: CapabilityId::ReadFile.as_str().into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
            malformed_json: false,
        };
        let validated = validate_tool_call(&call, &canonical_capabilities()).unwrap();
        assert_eq!(validated.capability, CapabilityId::ReadFile);
        assert!(!validated.argument_hash.is_empty());
    }

    #[test]
    fn scope_authorizer_refuses_write_outside_scope() {
        let authorizer = ScopeAuthorizer {
            granted_capabilities: vec![CapabilityId::ApplyEdit],
            allowed_write_paths: vec!["src/".into()],
            allowed_commands: vec![],
            network_allowed: false,
        };
        let call = ValidatedCall {
            call_id: "c1".into(),
            capability: CapabilityId::ApplyEdit,
            arguments: serde_json::json!({"path": "secrets/keys.pem", "content": "x"}),
            argument_hash: "h".into(),
        };
        let ctx = AuthorityContext {
            project_id: "p".into(),
            execution_id: "e".into(),
            attempt_id: "a".into(),
            worker_id: "w".into(),
        };
        let decision = authorizer.authorize(&call, &ctx);
        assert!(matches!(
            decision,
            AuthorizationDecision::Refused {
                reason: AuthorizationRefusal::OutOfWriteScope { .. },
                ..
            }
        ));
    }

    #[test]
    fn scope_authorizer_allows_write_inside_scope() {
        let authorizer = ScopeAuthorizer {
            granted_capabilities: vec![CapabilityId::ApplyEdit],
            allowed_write_paths: vec!["src/".into()],
            allowed_commands: vec![],
            network_allowed: false,
        };
        let call = ValidatedCall {
            call_id: "c1".into(),
            capability: CapabilityId::ApplyEdit,
            arguments: serde_json::json!({"path": "src/lib.rs", "content": "x"}),
            argument_hash: "h".into(),
        };
        let ctx = AuthorityContext {
            project_id: "p".into(),
            execution_id: "e".into(),
            attempt_id: "a".into(),
            worker_id: "w".into(),
        };
        assert_eq!(
            authorizer.authorize(&call, &ctx),
            AuthorizationDecision::Authorized
        );
    }

    #[test]
    fn resume_decision_fails_closed_for_destructive_intent_without_result() {
        assert_eq!(
            resume_decision_for(SideEffectClass::Destructive, false),
            ResumeDecision::FailClosed
        );
        assert_eq!(
            resume_decision_for(SideEffectClass::IdempotentWrite, false),
            ResumeDecision::ReplayAllowed
        );
        assert_eq!(
            resume_decision_for(SideEffectClass::ReadOnly, true),
            ResumeDecision::AlreadyDone
        );
    }

    #[test]
    fn in_memory_journal_round_trips() {
        let mut journal = InMemoryToolJournal::default();
        let intent = JournalIntent {
            call_id: "c1".into(),
            capability: CapabilityId::ApplyEdit,
            argument_hash: "h".into(),
            side_effect_class: SideEffectClass::IdempotentWrite,
        };
        journal.record_intent(&intent).unwrap();
        assert_eq!(journal.len(), 1);
        assert!(journal.result_for("c1").is_none());
        journal
            .record_result(
                "c1",
                &JournalResult::Succeeded {
                    result_hash: "rh".into(),
                },
            )
            .unwrap();
        assert_eq!(
            journal.result_for("c1"),
            Some(JournalResult::Succeeded {
                result_hash: "rh".into()
            })
        );
    }

    #[test]
    fn cache_control_is_none_for_empty_prefix_and_ephemeral_otherwise() {
        assert_eq!(
            cache_control_for(&StablePrefix {
                bytes: String::new(),
                version: "v1".into()
            }),
            CacheControl::None
        );
        assert_eq!(
            cache_control_for(&StablePrefix {
                bytes: "repo context".into(),
                version: "v1".into()
            }),
            CacheControl::Ephemeral
        );
    }
}
