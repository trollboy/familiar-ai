//! PRD-059 integration coverage: the real raw-runtime loop
//! (`familiar_ai_agent::raw_runtime::run_loop`) driven end-to-end through
//! `AnthropicAdapter` against a `wiremock::MockServer`. Complements the
//! adapter-local unit tests in `crates/familiar-ai-agent/src/anthropic.rs`
//! (wire projection, tool_use replay, stop-reason mapping in isolation) by
//! exercising the full loop: composition, validation, authorization,
//! journaling, and iteration against a real (mocked) HTTP transport.
//!
//! No test in this file performs, or is able to perform, a live or billable
//! call — every request lands on a `wiremock::MockServer`.

use familiar_ai_agent::anthropic::{AnthropicAdapter, AnthropicAdapterConfig};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, ScopeAuthorizer, StablePrefix, StopReason,
    ToolExecutor, ValidatedCall, VolatileTask,
};
use familiar_ai_core::config::AuthDescriptor;
use familiar_ai_llm::anthropic_api::{AnthropicHttpConfig, StaticCredentialResolver};
use familiar_ai_llm::attempt::AttemptId;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_anthropic".into(),
    }
}

fn prefix() -> StablePrefix {
    StablePrefix {
        bytes: "stable repository context".into(),
        version: "prefix-v1".into(),
    }
}

fn task() -> VolatileTask {
    VolatileTask {
        bytes: "review this diff".into(),
    }
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_config(model: &str, capabilities: Vec<CapabilityId>) -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:anthropic-test".into(),
        worker_empirical_version: "wver-sha256:anthropic-test".into(),
        model: model.into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 10,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        },
        offered_capabilities: capabilities,
        structured_output: None,
        authority: authority(),
    }
}

fn adapter_against(server: &MockServer) -> AnthropicAdapter {
    AnthropicAdapter::with_credential_resolver(
        AnthropicAdapterConfig {
            auth: AuthDescriptor::None,
            http: AnthropicHttpConfig {
                base_url: server.uri(),
                request_timeout_secs: 5,
                ..AnthropicHttpConfig::default()
            },
            ..AnthropicAdapterConfig::default()
        },
        Box::new(StaticCredentialResolver("sk-test".into())),
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

/// Executes `read-file` by returning fixed content; never touches the
/// filesystem or a subprocess.
#[derive(Default)]
struct StubExecutor;
impl ToolExecutor for StubExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        Ok(ExecutionOutcome {
            result_text: format!("stub result for {}", call.call_id),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}

fn authorizer_for(capabilities: Vec<CapabilityId>) -> ScopeAuthorizer {
    ScopeAuthorizer {
        granted_capabilities: capabilities,
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    }
}

#[tokio::test]
async fn full_loop_completes_end_to_end_against_wiremock() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":50}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"All good."}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let mut executor = StubExecutor;
    let authorizer = authorizer_for(vec![CapabilityId::ReadFile]);
    let config = base_config("claude-test-model", vec![CapabilityId::ReadFile]);
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(outcome.final_text.as_deref(), Some("All good."));
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.attempts[0].usage.uncached_input_tokens, Some(50));
    assert_eq!(outcome.attempts[0].usage.output_tokens, Some(6));
    assert!(!outcome.attempts[0].ambiguous);
}

#[tokio::test]
async fn tool_round_trip_executes_and_then_completes() {
    let server = MockServer::start().await;
    let first = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":30}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read-file","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let second = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-test-model","usage":{"input_tokens":45}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Read it, done."}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
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

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let mut executor = StubExecutor;
    let authorizer = authorizer_for(vec![CapabilityId::ReadFile]);
    let config = base_config("claude-test-model", vec![CapabilityId::ReadFile]);
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(outcome.final_text.as_deref(), Some("Read it, done."));
    // Two separate submissions, each its own attempt with its own usage.
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.attempts[0].attempt_id, AttemptId("att_1".into()));
    assert_eq!(outcome.attempts[1].attempt_id, AttemptId("att_2".into()));
    assert_eq!(outcome.evidence.calls.len(), 1);
}

#[tokio::test]
async fn wall_clock_timeout_stops_honestly_with_ambiguous_usage() {
    let server = MockServer::start().await;
    // Delay the response well past the loop's wall-clock ceiling.
    let body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream")
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let mut executor = StubExecutor;
    let authorizer = authorizer_for(vec![CapabilityId::ReadFile]);
    let mut config = base_config("claude-test-model", vec![CapabilityId::ReadFile]);
    config.ceilings.max_wall_clock_ms = Some(20);
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::Timeout);
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.attempts[0].ambiguous);
    assert!(outcome.attempts[0].usage.is_entirely_unknown());
}

#[tokio::test]
async fn model_alias_resolves_to_a_distinct_dated_identity() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-sonnet-5-20260615","usage":{"input_tokens":10}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let mut executor = StubExecutor;
    let authorizer = authorizer_for(vec![]);
    // The requested identifier is the alias, not the dated snapshot.
    let config = base_config("claude-sonnet-5", vec![]);
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    let attempt_id = &outcome.attempts[0].attempt_id;
    let metadata = adapter.attempt_metadata(attempt_id).unwrap();
    // The alias requested and the provider-resolved identity are both
    // available, and they differ — the alias is never frozen into the
    // canonical worker identity.
    assert_eq!(config.model, "claude-sonnet-5");
    assert_eq!(
        metadata.resolved_model.as_deref(),
        Some("claude-sonnet-5-20260615")
    );
    assert_ne!(
        metadata.resolved_model.as_deref(),
        Some(config.model.as_str())
    );
}

#[tokio::test]
async fn a_retry_after_provider_failure_is_a_separate_attempt() {
    let server = MockServer::start().await;
    // First attempt: a non-retryable-looking but distinct provider error.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let mut executor = StubExecutor;
    let authorizer = authorizer_for(vec![]);
    let config = base_config("claude-test-model", vec![]);
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    // A 500 is Retryable(TransientTransport); the loop itself does not retry
    // internally (per the inference adapter contract) — it stops honestly
    // and a caller decides whether to invoke the loop again with a fresh
    // attempt id. Confirm the honest stop, not a fabricated completion.
    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: familiar_ai_agent::raw_runtime::ProviderFailureTaxonomy::Retryable
        }
    );
    assert!(outcome.attempts.is_empty());

    // A second, independent invocation with a fresh attempt id succeeds —
    // demonstrating a retry is a new attempt, never a free replay.
    let second_body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-test-model","usage":{"input_tokens":5}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(second_body, "text/event-stream"))
        .mount(&server)
        .await;

    let mut journal2 = InMemoryToolJournal::default();
    let mut executor2 = StubExecutor;
    let retry_outcome = run_loop(
        &adapter,
        &mut executor2,
        &authorizer,
        &mut journal2,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;
    assert_eq!(
        retry_outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    // The failed submission recorded no usage at all (nothing to merge or
    // replay); the retry is a wholly independent attempt with its own,
    // freshly-observed usage.
    assert!(outcome.attempts.is_empty());
    assert_eq!(retry_outcome.attempts.len(), 1);
    assert_eq!(retry_outcome.attempts[0].usage.output_tokens, Some(1));
}
