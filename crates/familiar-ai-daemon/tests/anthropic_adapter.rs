//! PRD-059 daemon-side integration coverage: the real Anthropic adapter
//! (`familiar_ai_agent::anthropic::AnthropicAdapter`), driven through the
//! PRD-058 raw-runtime loop against a `wiremock::MockServer`, persisted
//! through the same `persist_run_outcome` / `AccountingRepository` path
//! every other execution uses. No test in this file performs, or is able
//! to perform, a live or billable model call.

use familiar_ai_agent::anthropic::{AnthropicAdapter, AnthropicAdapterConfig};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, InMemoryToolJournal, LoopCeilings, LoopConfig,
    StablePrefix, StopReason, VolatileTask,
};
use familiar_ai_core::config::AuthDescriptor;
use familiar_ai_daemon::agent_runtime::persist_run_outcome;
use familiar_ai_llm::anthropic_api::{AnthropicHttpConfig, StaticCredentialResolver};
use familiar_ai_llm::attempt::AttemptId;
use familiar_ai_storage::Database;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn setup_execution(db: &Database, execution_id: &str) {
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'raw-runtime','running','repo','wt','docs/prds/PRD-059.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_authority(execution_id: &str) -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: execution_id.into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_anthropic".into(),
    }
}

fn base_config(execution_id: &str) -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:anthropic-daemon-test".into(),
        worker_empirical_version: "wver-sha256:anthropic-daemon-test".into(),
        model: "claude-test-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![],
        structured_output: None,
        authority: base_authority(execution_id),
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

#[tokio::test]
async fn usage_ledger_carries_provider_request_id_and_cache_categories() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let server = MockServer::start().await;
    let body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":80,"cache_read_input_tokens":30,"cache_creation_input_tokens":5}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream")
                .insert_header("request-id", "req_abc123"),
        )
        .mount(&server)
        .await;

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let config = base_config("exec_1");
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };

    let outcome = run_loop(
        &adapter,
        &mut StubExecutor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "do the task".into(),
        },
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

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-implementation",
        "worker_anthropic",
        "anthropic-api",
        Some("claude-test-model"),
        None,
        &outcome,
    )
    .unwrap();

    let mut statement = db
        .conn()
        .prepare(
            "SELECT provider_request_id, uncached_input_tokens, cache_read_tokens, cache_write_tokens, output_tokens FROM usage_observations WHERE execution_id='exec_1'",
        )
        .unwrap();
    type Row = (
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );
    let rows: Vec<Row> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    let (provider_request_id, uncached_input, cache_read, cache_write, output) = &rows[0];
    assert_eq!(provider_request_id.as_deref(), Some("req_abc123"));
    assert_eq!(*uncached_input, Some(80));
    assert_eq!(*cache_read, Some(30));
    assert_eq!(*cache_write, Some(5));
    assert_eq!(*output, Some(7));
}

#[tokio::test]
async fn replaying_the_same_attempt_is_idempotent_separate_attempts_are_not() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let server = MockServer::start().await;
    let body = sse_body(&[
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test-model","usage":{"input_tokens":10}}}"#,
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
    let config = base_config("exec_1");
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };

    let outcome = run_loop(
        &adapter,
        &mut StubExecutor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "do the task".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;
    assert_eq!(outcome.attempts.len(), 1);

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-implementation",
        "worker_anthropic",
        "anthropic-api",
        Some("claude-test-model"),
        None,
        &outcome,
    )
    .unwrap();

    // Replay of the same persisted provider event: the accounting ledger's
    // own idempotency key (source_event_hash, derived from
    // execution_id:attempt_id — the exact same construction
    // `persist_run_outcome` uses) must make a second `append_observation`
    // call for the identical attempt a no-op, never a second row. This is
    // the actual idempotency boundary the ledger documents
    // (`AccountingRepository::append_observation`'s doc comment); a bare
    // second `persist_run_outcome` call is not itself idempotent (it would
    // also re-insert an `agent_runtime_attempts` row, a distinct table with
    // its own primary key), so this exercises the accounting layer directly.
    use familiar_ai_storage::repos::accounting::{AccountingRepository, UsageObservation};
    let accounting = AccountingRepository::new(db.conn());
    let attempt_id = &outcome.attempts[0].attempt_id.0;
    let source_event_hash = {
        let material = format!("exec_1:{attempt_id}");
        ring::digest::digest(&ring::digest::SHA256, material.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let now = chrono::Utc::now().to_rfc3339();
    let replay_result = accounting
        .append_observation(&UsageObservation {
            execution_id: "exec_1",
            attempt_id,
            stage: "raw-runtime-implementation",
            session_id: None,
            worker_identity: "worker_anthropic",
            adapter: "anthropic-api",
            cli_version: None,
            model_identity: Some("claude-test-model"),
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: outcome.attempts[0].usage.uncached_input_tokens,
            cache_read_tokens: outcome.attempts[0].usage.cache_read_tokens,
            cache_write_tokens: outcome.attempts[0].usage.cache_write_tokens,
            output_tokens: outcome.attempts[0].usage.output_tokens,
            reasoning_output_tokens: outcome.attempts[0].usage.reasoning_output_tokens,
            unknown_reason: None,
            period_start: &now,
            period_end: &now,
            terminal_status: "completed",
            source_event_hash: &source_event_hash,
            provider_cost_lexical: None,
            project_resolution_evidence: None,
            output_register_id: "raw-runtime-none",
            output_register_version: "raw-runtime-none",
            input_compression_id: "raw-runtime-none",
            input_compression_version: "raw-runtime-none",
            compression_experiment: None,
            compression_lane: None,
        })
        .unwrap();
    assert!(
        replay_result.is_some(),
        "replay must return the existing observation id, not fail"
    );

    let count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM usage_observations WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "replaying the same attempt must not duplicate the observation"
    );

    // A second, genuinely separate execution (its own attempt ids — every
    // `AttemptId` is globally unique, never scoped per execution) must
    // still land as its own, separate observation — replay-idempotency
    // never merges distinct billable attempts.
    setup_execution(&db, "exec_2");
    let mut journal2 = InMemoryToolJournal::default();
    let config2 = base_config("exec_2");
    let outcome2 = run_loop(
        &adapter,
        &mut StubExecutor,
        &authorizer,
        &mut journal2,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "do a different task".into(),
        },
        &config2,
        {
            let mut n = 100u32;
            move || {
                n += 1;
                AttemptId(format!("att_{n}"))
            }
        },
    )
    .await;
    persist_run_outcome(
        db.conn(),
        "exec_2",
        "raw-runtime-implementation",
        "worker_anthropic",
        "anthropic-api",
        Some("claude-test-model"),
        None,
        &outcome2,
    )
    .unwrap();

    let total: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM usage_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        total, 2,
        "a genuinely separate attempt must be its own observation"
    );
}

#[tokio::test]
async fn missing_usage_records_an_explicit_unknown_reason_never_a_fabricated_zero() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

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

    let adapter = adapter_against(&server);
    let mut journal = InMemoryToolJournal::default();
    let config = base_config("exec_1");
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };

    let outcome = run_loop(
        &adapter,
        &mut StubExecutor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "do the task".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-implementation",
        "worker_anthropic",
        "anthropic-api",
        Some("claude-test-model"),
        None,
        &outcome,
    )
    .unwrap();

    let (output_tokens, unknown_reason): (Option<i64>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT output_tokens, unknown_reason FROM usage_observations WHERE execution_id='exec_1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(output_tokens, None);
    assert!(unknown_reason.is_some());
}

#[tokio::test]
async fn auth_failure_stops_closed_without_ever_sending_a_request() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let server = MockServer::start().await;
    // No mock is registered for /v1/messages — if the adapter ever sent a
    // request despite the missing credential, wiremock would 404 it; the
    // test instead asserts zero requests were made at all.
    let missing_env = "PRD059_TEST_ANTHROPIC_KEY_ABSENT_DAEMON";
    std::env::remove_var(missing_env);
    let adapter = AnthropicAdapter::new(AnthropicAdapterConfig {
        auth: AuthDescriptor::Env(missing_env.into()),
        http: AnthropicHttpConfig {
            base_url: server.uri(),
            request_timeout_secs: 5,
            ..AnthropicHttpConfig::default()
        },
        ..AnthropicAdapterConfig::default()
    })
    .unwrap();

    let mut journal = InMemoryToolJournal::default();
    let config = base_config("exec_1");
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };

    let outcome = run_loop(
        &adapter,
        &mut StubExecutor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "do the task".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: familiar_ai_agent::raw_runtime::ProviderFailureTaxonomy::NonRetryable
        }
    );
    assert!(outcome.attempts.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());

    // Evidence is still recorded honestly even though no attempt ran.
    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-implementation",
        "worker_anthropic",
        "anthropic-api",
        None,
        None,
        &outcome,
    )
    .unwrap();
    let stop_reason: String = db
        .conn()
        .query_row(
            "SELECT stop_reason FROM agent_runtime_evidence WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stop_reason, "provider-failure");
}

/// Acknowledges every capability without touching the filesystem or a
/// subprocess — this test file has no tool-call fixtures, only inference.
struct StubExecutor;
impl familiar_ai_agent::raw_runtime::ToolExecutor for StubExecutor {
    fn execute(
        &mut self,
        call: &familiar_ai_agent::raw_runtime::ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<
        familiar_ai_agent::raw_runtime::ExecutionOutcome,
        familiar_ai_agent::raw_runtime::ExecutionError,
    > {
        Ok(familiar_ai_agent::raw_runtime::ExecutionOutcome {
            result_text: "stub".into(),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}
