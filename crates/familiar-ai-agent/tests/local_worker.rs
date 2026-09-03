//! PRD-063 integration coverage: the local worker adapter (`ollama`,
//! `unsloth`) driven through the real PRD-058 `run_loop`, against a
//! `wiremock` loopback fake HTTP server. No test in this file performs, or
//! could perform, real model execution or any network beyond the loopback
//! fake.

use familiar_ai_agent::local_worker::{
    LocalAuthToken, LocalChatConfig, LocalInferenceAdapter, LocalRuntimeKind,
};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, ProviderFailureTaxonomy, ScopeAuthorizer,
    StablePrefix, StopReason, ToolExecutor, ValidatedCall, VolatileTask,
};
use familiar_ai_llm::attempt::{AttemptId, StructuredOutputRequest};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
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
        worker_spec_identity: "wspec-sha256:local-test".into(),
        worker_empirical_version: "wver-sha256:local-test".into(),
        model: "llama3".into(),
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

fn adapter_for(server: &MockServer, runtime: LocalRuntimeKind) -> LocalInferenceAdapter {
    LocalInferenceAdapter::new(
        runtime,
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

async fn mount_sse(server: &MockServer, frames: &[Value]) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse(frames), "text/event-stream"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn streaming_text_completes_the_loop() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"choices": [{"delta": {"content": "done"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 3}}),
        ],
    )
    .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
async fn unsloth_runtime_reports_its_own_runtime_id() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[json!({"choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]})],
    )
    .await;
    let adapter = adapter_for(&server, LocalRuntimeKind::Unsloth);
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
        outcome.evidence.worker_spec_identity,
        "wspec-sha256:local-test"
    );
    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
}

#[tokio::test]
async fn tool_call_round_trips_through_the_full_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "read-file", "arguments": ""}}]}}]}),
                json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"src/lib.rs\"}"}}]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 20, "completion_tokens": 5}}),
            ]),
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[json!({"choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 30, "completion_tokens": 2}})]),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
        &[json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "delete-everything", "arguments": "{}"}}]}, "finish_reason": "tool_calls"}]})],
    )
    .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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

/// Endpoint disappearance (crash, never started, or network cut before any
/// response): the adapter classifies this as a retryable, honestly
/// zero-attempt failure — never a fabricated billable attempt.
#[tokio::test]
async fn endpoint_disappearance_stops_closed_with_no_fabricated_attempt() {
    let adapter = LocalInferenceAdapter::new(
        LocalRuntimeKind::Ollama,
        LocalAuthToken::new(None),
        LocalChatConfig {
            base_url: "http://127.0.0.1:1".into(),
            request_timeout_secs: 2,
        },
    )
    .unwrap();
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
    assert!(outcome.attempts.is_empty());
}

#[tokio::test]
async fn partial_interrupted_stream_preserves_ambiguous_usage_and_stops() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\": [{\"delta\": {\"content\": \"still thinking\"}}]}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
async fn missing_usage_records_unknown_never_zero() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[json!({"choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]})],
    )
    .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
async fn every_submission_is_its_own_attempt_hitting_the_endpoint_once_each() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "read-file", "arguments": "{\"path\":\"src/lib.rs\"}"}}]}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}})]),
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}})]),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
async fn cancellation_before_submission_never_calls_the_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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

#[tokio::test]
async fn artifact_verification_degrades_honestly_when_unverifiable() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, LocalRuntimeKind::Unsloth);
    let outcome = adapter.verify_artifact("llama3", Some("sha256:abc")).await;
    assert!(matches!(
        outcome,
        familiar_ai_agent::local_worker::ArtifactVerificationOutcome::Unverifiable { .. }
    ));
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
    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
    let outcome = adapter.verify_artifact("llama3", Some("sha256:abc")).await;
    assert_eq!(
        outcome,
        familiar_ai_agent::local_worker::ArtifactVerificationOutcome::Verified {
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
    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
    let outcome = adapter.verify_artifact("llama3", Some("sha256:abc")).await;
    assert_eq!(
        outcome,
        familiar_ai_agent::local_worker::ArtifactVerificationOutcome::Mismatch {
            claimed: "sha256:different".into(),
            expected: "sha256:abc".into(),
        }
    );
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
    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
    let outcome = adapter.verify_artifact("llama3", None).await;
    assert!(matches!(
        outcome,
        familiar_ai_agent::local_worker::ArtifactVerificationOutcome::Unverifiable { .. }
    ));
}

#[tokio::test]
async fn probe_health_lists_the_served_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "llama3"}, {"id": "qwen3"}]
        })))
        .mount(&server)
        .await;
    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
    let models = adapter.probe_health().await.unwrap();
    assert_eq!(models, vec!["llama3".to_string(), "qwen3".to_string()]);
}

#[tokio::test]
async fn auth_failure_stops_non_retryable_with_no_fabricated_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid credentials"}
        })))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
    assert!(outcome.attempts.is_empty());
}

#[tokio::test]
async fn server_overload_is_retryable_with_no_fabricated_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"message": "loading model"}
        })))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
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
    assert!(outcome.attempts.is_empty());
}

#[tokio::test]
async fn structured_output_request_reaches_the_wire_as_json_schema_response_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "plan", "schema": {"type": "object"}}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[json!({"choices": [{"delta": {"content": "{}"}, "finish_reason": "stop"}]})]),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let adapter = adapter_for(&server, LocalRuntimeKind::Ollama);
    let mut executor = SpyExecutor::default();
    let authorizer = permissive_authorizer();
    let mut journal = InMemoryToolJournal::default();
    let mut config = base_config();
    config.structured_output = Some(StructuredOutputRequest {
        schema_name: "plan".into(),
        json_schema: r#"{"type":"object"}"#.into(),
    });

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

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: true
        }
    );
}
