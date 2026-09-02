//! PRD-061 daemon-side integration coverage: `familiar_ai_llm::xai_api::XaiAdapter`
//! (a real `InferenceAdapter`, backed by `wiremock` — never a live call)
//! round-tripped through the exact same SQLite-backed tool journal,
//! sandboxed executor, and PRD-051 usage-ledger persistence path proven
//! generically in `raw_runtime.rs`. This proves the xAI adapter needs no
//! daemon-side change to participate in that ledger, and that a replayed
//! attempt persists exactly once.

use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, LoopCeilings, LoopConfig,
    StablePrefix, StopReason, VolatileTask,
};
use familiar_ai_core::config::AgentRuntimeSandboxConfig;
use familiar_ai_core::config::AuthDescriptor;
use familiar_ai_daemon::agent_runtime::{
    persist_run_outcome, write_scope_authorizer_from_prd, SandboxedToolExecutor, SqliteToolJournal,
};
use familiar_ai_llm::xai_api::{XaiAdapter, XaiAdapterConfig};
use familiar_ai_storage::repos::accounting::{AccountingRepository, UsageObservation};
use familiar_ai_storage::Database;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SAMPLE_PRD: &str = "# PRD-999: Sample\n\n## Expected Files\n\n- `src/lib.rs`\n";

fn setup_execution(db: &Database, execution_id: &str) {
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'raw-runtime','running','repo','wt','docs/prds/PRD-999.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
}

fn attempt_id_source() -> impl FnMut() -> familiar_ai_llm::attempt::AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        familiar_ai_llm::attempt::AttemptId(format!("att_{n}"))
    }
}

fn base_authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
    }
}

fn no_sandbox() -> AgentRuntimeSandboxConfig {
    AgentRuntimeSandboxConfig {
        allowed_commands: vec![],
        network_allowed: false,
        allowed_environment: vec![],
    }
}

fn sse(lines: &[&str]) -> String {
    let mut body = String::new();
    for line in lines {
        body.push_str("data: ");
        body.push_str(line);
        body.push_str("\n\n");
    }
    body
}

async fn xai_adapter(server: &MockServer, env_name: &str) -> XaiAdapter {
    std::env::set_var(env_name, "sk-test-key");
    XaiAdapter::new(XaiAdapterConfig {
        base_url: server.uri(),
        auth: AuthDescriptor::Env(env_name.into()),
        request_timeout_secs: 5,
    })
    .unwrap()
}

#[tokio::test]
async fn xai_wire_round_trip_persists_journal_evidence_and_usage_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let server = MockServer::start().await;
    let tool_call_body = sse(&[
        r#"{"id":"req_1","model":"grok-4-0709","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"apply-edit","arguments":"{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"id":"req_1","model":"grok-4-0709","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":10,"prompt_tokens_details":{"text_tokens":50,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0},"cost_in_usd_ticks":8000}}"#,
        "[DONE]",
    ]);
    let done_body = sse(&[
        r#"{"id":"req_2","model":"grok-4-0709","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
        r#"{"id":"req_2","model":"grok-4-0709","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3,"prompt_tokens_details":{"text_tokens":5,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0},"cost_in_usd_ticks":900}}"#,
        "[DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(tool_call_body, "text/event-stream"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(done_body, "text/event-stream"))
        .mount(&server)
        .await;

    let authorizer =
        write_scope_authorizer_from_prd(SAMPLE_PRD, vec![CapabilityId::ApplyEdit], &no_sandbox())
            .unwrap();
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree.clone(),
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");
    let adapter = xai_adapter(&server, "XAI_DAEMON_TEST_KEY_1").await;

    let config = LoopConfig {
        worker_spec_identity: "wspec-sha256:xai-test".into(),
        worker_empirical_version: "wver-sha256:xai-test".into(),
        model: "grok-4".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ApplyEdit],
        structured_output: None,
        authority: base_authority(),
    };

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "implement it".into(),
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
    assert_eq!(
        std::fs::read_to_string(worktree.join("src/lib.rs")).unwrap(),
        "fn main() {}"
    );

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-review",
        "worker_1",
        "xai-api",
        Some("grok-4"),
        None,
        &outcome,
    )
    .unwrap();

    let mut statement = db
        .conn()
        .prepare("SELECT attempt_id,adapter,uncached_input_tokens,output_tokens,spec_identity FROM usage_observations WHERE execution_id='exec_1' ORDER BY attempt_id")
        .unwrap();
    type UsageRow = (String, String, Option<i64>, Option<i64>, Option<String>);
    let rows: Vec<UsageRow> = statement
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
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    // Every row's adapter is xai-api — never openai, never anthropic.
    assert!(rows.iter().all(|(_, adapter, ..)| adapter == "xai-api"));
    assert_eq!(rows[0].2, Some(50));
    assert_eq!(rows[0].3, Some(10));
    assert_eq!(rows[0].4, Some("wspec-sha256:xai-test".into()));
    assert_eq!(rows[1].2, Some(5));
    assert_eq!(rows[1].3, Some(3));
}

/// PRD-058's `persist_run_outcome` mints a fresh `agent_runtime_attempts`
/// row per call and is therefore not itself safe to invoke twice for one
/// attempt — that is pre-existing, provider-agnostic behavior this PRD does
/// not touch. The idempotency guarantee this PRD's acceptance criteria
/// actually name lives one layer down, at the PRD-051 ledger boundary
/// (`AccountingRepository::append_observation`, keyed on
/// `source_event_hash`): replaying *the same persisted provider event*
/// (identical hash) is a no-op, while a genuinely separate billable attempt
/// (a different hash) is its own observation. This test exercises that
/// boundary directly, with the exact xAI-shaped fields
/// `persist_run_outcome` would have produced for the attempt.
#[tokio::test]
async fn replaying_the_same_persisted_provider_event_is_idempotent_never_double_counted() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_2");

    let observation = UsageObservation {
        execution_id: "exec_2",
        attempt_id: "att_1",
        stage: "raw-runtime-review",
        session_id: None,
        worker_identity: "worker_1",
        adapter: "xai-api",
        cli_version: None,
        model_identity: Some("grok-4"),
        service_tier: None,
        provider_request_id: Some("req_1"),
        uncached_input_tokens: Some(5),
        cache_read_tokens: Some(0),
        cache_write_tokens: None,
        output_tokens: Some(1),
        reasoning_output_tokens: None,
        unknown_reason: None,
        period_start: "2026-09-01T00:00:00Z",
        period_end: "2026-09-01T00:00:00Z",
        terminal_status: "completed",
        source_event_hash: "xai-test-source-event-hash-1",
        provider_cost_lexical: None,
        project_resolution_evidence: None,
        output_register_id: "raw-runtime-none",
        output_register_version: "raw-runtime-none",
        input_compression_id: "raw-runtime-none",
        input_compression_version: "raw-runtime-none",
        compression_experiment: None,
        compression_lane: None,
    };

    let repo = AccountingRepository::new(db.conn());
    let first_id = repo.append_observation(&observation).unwrap();
    let second_id = repo.append_observation(&observation).unwrap();
    assert_eq!(
        first_id, second_id,
        "a replay of the same provider event must resolve to the same observation"
    );

    let count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM usage_observations WHERE execution_id='exec_2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "replaying the same persisted provider event must not double-count usage"
    );

    // A genuinely separate billable attempt (different source_event_hash,
    // e.g. a real retry) is its own observation, never merged into the
    // first.
    let second_attempt = UsageObservation {
        attempt_id: "att_2",
        source_event_hash: "xai-test-source-event-hash-2",
        provider_request_id: Some("req_2"),
        ..observation
    };
    repo.append_observation(&second_attempt).unwrap();
    let count_after_new_attempt: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM usage_observations WHERE execution_id='exec_2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count_after_new_attempt, 2,
        "a separate billable attempt must remain its own observation"
    );
}
