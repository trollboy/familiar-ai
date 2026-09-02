//! PRD-060 integration coverage: the OpenAI Responses API adapter driven
//! through the real PRD-058 `run_loop`, against a `wiremock` fake HTTP
//! server. No test in this file performs, or could perform, a live or
//! billable call — every fixture is deterministic and offline.

use familiar_ai_agent::openai::{OpenAiInferenceAdapter, OpenAiResponsesConfig};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, ProviderFailureTaxonomy, ScopeAuthorizer,
    StablePrefix, StopReason, ToolExecutor, ValidatedCall, VolatileTask,
};
use familiar_ai_llm::attempt::AttemptId;
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
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
        bytes: "do the thing".into(),
    }
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_config() -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "gpt-5".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 10,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        },
        offered_capabilities: vec![CapabilityId::ReadFile, CapabilityId::ApplyEdit],
        structured_output: None,
        authority: authority(),
    }
}

fn permissive_authorizer() -> ScopeAuthorizer {
    ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ReadFile, CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    }
}

/// Records every call it receives so tests can assert a refused call is
/// never executed.
#[derive(Default)]
struct SpyExecutor {
    calls: Vec<ValidatedCall>,
}

impl ToolExecutor for SpyExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        self.calls.push(call.clone());
        Ok(ExecutionOutcome {
            result_text: "ok".into(),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}

async fn adapter_for(server: &MockServer) -> OpenAiInferenceAdapter {
    OpenAiInferenceAdapter::new(
        "sk-test",
        OpenAiResponsesConfig {
            base_url: server.uri(),
            request_timeout_secs: 5,
            service_tier: None,
        },
    )
    .unwrap()
}

fn sse(frames: &[Value]) -> String {
    frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

async fn mount_sse(server: &MockServer, frames: &[Value]) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse(frames), "text/event-stream"))
        .mount(server)
        .await;
}

fn completed(id: &str, model: &str, output: Value, usage: Value) -> Value {
    json!({"type": "response.completed", "response": {
        "id": id, "status": "completed", "model": model, "output": output, "usage": usage,
    }})
}

fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
    json!({
        "input_tokens": input, "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": output, "output_tokens_details": {"reasoning_tokens": reasoning},
    })
}

#[tokio::test]
async fn streaming_text_completes_the_loop() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"type": "response.output_text.delta", "delta": "done"}),
            completed("resp_1", "gpt-5", json!([]), usage(10, 0, 3, 0)),
        ],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(outcome.final_text.as_deref(), Some("done"));
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.attempts[0].usage.output_tokens, Some(3));
}

#[tokio::test]
async fn tool_call_round_trips_through_the_full_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"type": "response.output_item.added", "item": {
                    "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
                }}),
                json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{\"path\":\"src/lib.rs\"}"}),
                json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
                completed(
                    "resp_1",
                    "gpt-5",
                    json!([{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{\"path\":\"src/lib.rs\"}"}]),
                    usage(20, 0, 5, 0),
                ),
            ]),
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"type": "response.output_text.delta", "delta": "done"}),
                completed("resp_2", "gpt-5", json!([]), usage(30, 0, 2, 0)),
            ]),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(executor.calls.len(), 1);
    assert_eq!(executor.calls[0].capability, CapabilityId::ReadFile);
    assert_eq!(outcome.attempts.len(), 2);
}

#[tokio::test]
async fn unknown_capability_is_refused_without_execution() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "delete-everything"
            }}),
            json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
            completed(
                "resp_1",
                "gpt-5",
                json!([{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "delete-everything", "arguments": "{}"}]),
                usage(5, 0, 1, 0),
            ),
        ],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();
    let mut config = base_config();
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(executor.calls.is_empty());
    assert_eq!(outcome.stop_reason, StopReason::IterationCeiling);
}

#[tokio::test]
async fn malformed_arguments_are_refused_without_execution() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
            }}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{not valid json"}),
            json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
            completed(
                "resp_1",
                "gpt-5",
                json!([{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{not valid json"}]),
                usage(5, 0, 1, 0),
            ),
        ],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();
    let mut config = base_config();
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(executor.calls.is_empty());
    assert_eq!(outcome.stop_reason, StopReason::IterationCeiling);
}

#[tokio::test]
async fn partial_interrupted_stream_preserves_ambiguous_usage_and_stops() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[json!({"type": "response.output_text.delta", "delta": "still thinking"})],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: ProviderFailureTaxonomy::Ambiguous
        }
    );
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.attempts[0].ambiguous);
    assert!(outcome.attempts[0].usage.is_entirely_unknown());
}

#[tokio::test]
async fn rate_limit_stops_with_retryable_provider_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"code": "rate_limit_exceeded", "message": "slow down"}
        })))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: ProviderFailureTaxonomy::Retryable
        }
    );
}

#[tokio::test]
async fn auth_failure_stops_closed_and_records_no_fabricated_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "invalid_api_key", "message": "Incorrect API key provided"}
        })))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: ProviderFailureTaxonomy::NonRetryable
        }
    );
    // A non-retryable failure never fabricates a billable attempt record.
    assert!(outcome.attempts.is_empty());
}

#[tokio::test]
async fn missing_usage_records_unknown_never_zero() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed", "model": "gpt-5", "output": []
        }})],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.attempts.len(), 1);
    assert!(!outcome.attempts[0].ambiguous);
    assert!(outcome.attempts[0].usage.is_entirely_unknown());
}

#[tokio::test]
async fn cached_input_and_reasoning_categories_flow_through_to_attempt_usage() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[completed(
            "resp_1",
            "gpt-5",
            json!([]),
            usage(1000, 400, 60, 45),
        )],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    let usage = &outcome.attempts[0].usage;
    assert_eq!(usage.uncached_input_tokens, Some(600));
    assert_eq!(usage.cache_read_tokens, Some(400));
    assert_eq!(usage.output_tokens, Some(60));
    assert_eq!(usage.reasoning_output_tokens, Some(45));
}

#[tokio::test]
async fn resolved_model_can_drift_from_the_requested_alias() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[completed(
            "resp_1",
            "gpt-5-2026-09-01",
            json!([]),
            usage(1, 0, 1, 0),
        )],
    )
    .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();
    let mut config = base_config();
    config.model = "gpt-5".into();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    let attempt_id = &outcome.attempts[0].attempt_id;
    let meta = adapter.response_meta(attempt_id).unwrap();
    assert_eq!(meta.resolved_model.as_deref(), Some("gpt-5-2026-09-01"));
    assert_ne!(meta.resolved_model.unwrap(), config.model);
}

#[tokio::test]
async fn every_submission_is_its_own_attempt_hitting_the_provider_once_each() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"type": "response.output_item.added", "item": {
                    "type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file"
                }}),
                json!({"type": "response.function_call_arguments.delta", "item_id": "item_1", "delta": "{\"path\":\"src/lib.rs\"}"}),
                json!({"type": "response.function_call_arguments.done", "item_id": "item_1"}),
                completed(
                    "resp_1",
                    "gpt-5",
                    json!([{"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read-file", "arguments": "{\"path\":\"src/lib.rs\"}"}]),
                    usage(1, 0, 1, 0),
                ),
            ]),
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[completed("resp_2", "gpt-5", json!([]), usage(1, 0, 1, 0))]),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    let ids: Vec<&str> = outcome
        .attempts
        .iter()
        .map(|attempt| attempt.attempt_id.0.as_str())
        .collect();
    assert_eq!(ids, vec!["att_1", "att_2"]);
    server.verify().await;
}

#[tokio::test]
async fn cancellation_before_submission_never_calls_the_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let adapter = adapter_for(&server).await;
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    assert!(outcome.attempts.is_empty());
    server.verify().await;
}
