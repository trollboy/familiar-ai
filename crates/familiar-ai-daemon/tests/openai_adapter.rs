//! PRD-060 daemon-side integration coverage: the OpenAI Responses API
//! adapter driven through the real PRD-058 loop against a `wiremock` fake
//! server, persisted through the exact same shared `persist_run_outcome`/
//! `AccountingRepository` machinery every other raw-runtime worker uses
//! (`docs/contracts/agent-loop.md`), plus BYO-Auth credential resolution
//! and exactly-once ledger persistence. No test performs, or could
//! perform, a live or billable OpenAI call.

use familiar_ai_agent::openai::{OpenAiInferenceAdapter, OpenAiResponsesConfig};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, LoopCeilings, LoopConfig,
    StablePrefix, StopReason, VolatileTask,
};
use familiar_ai_core::config::{
    AgentRuntimeSandboxConfig, AuthDescriptor, PriceCurrency, PriceScheduleConfig,
    PriceScheduleRateConfig,
};
use familiar_ai_daemon::agent_runtime::{
    persist_run_outcome, write_scope_authorizer_from_prd, SandboxedToolExecutor, SqliteToolJournal,
};
use familiar_ai_daemon::config_cli::check_auth;
use familiar_ai_llm::attempt::{AttemptId, InferenceAdapter};
use familiar_ai_storage::repos::accounting::{AccountingRepository, UsageObservation};
use familiar_ai_storage::Database;
use serde_json::{json, Value};
use wiremock::matchers::method;
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

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn no_sandbox() -> AgentRuntimeSandboxConfig {
    AgentRuntimeSandboxConfig {
        allowed_commands: vec![],
        network_allowed: false,
        allowed_environment: vec![],
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

fn completed(id: &str, model: &str, usage: Value) -> Value {
    json!({"type": "response.completed", "response": {
        "id": id, "status": "completed", "model": model, "output": [], "usage": usage,
    }})
}

#[tokio::test]
async fn full_loop_persists_through_the_shared_agent_runtime_pipeline() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"type": "response.output_text.delta", "delta": "done"}),
            completed(
                "resp_1",
                "gpt-5-2026-09-01",
                json!({
                    "input_tokens": 500, "input_tokens_details": {"cached_tokens": 100},
                    "output_tokens": 40, "output_tokens_details": {"reasoning_tokens": 15},
                }),
            ),
        ],
    )
    .await;

    let temp = tempfile::tempdir().unwrap();
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let authorizer =
        write_scope_authorizer_from_prd(SAMPLE_PRD, vec![CapabilityId::ApplyEdit], &no_sandbox())
            .unwrap();
    let mut executor = SandboxedToolExecutor {
        worktree_root: temp.path().to_path_buf(),
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");
    let adapter = OpenAiInferenceAdapter::new(
        "sk-test",
        OpenAiResponsesConfig {
            base_url: server.uri(),
            request_timeout_secs: 5,
            service_tier: None,
        },
    )
    .unwrap();

    let config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "gpt-5".into(),
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
    assert_eq!(outcome.attempts.len(), 1);

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-review",
        "worker_1",
        "openai-api",
        Some("gpt-5"),
        None,
        &outcome,
    )
    .unwrap();

    let (uncached, cache_read, output, reasoning): (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = db
        .conn()
        .query_row(
            "SELECT uncached_input_tokens,cache_read_tokens,output_tokens,reasoning_output_tokens FROM usage_observations WHERE execution_id='exec_1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(uncached, Some(400));
    assert_eq!(cache_read, Some(100));
    assert_eq!(output, Some(40));
    assert_eq!(reasoning, Some(15));

    // The full raw SSE body, the request/response text, and the API key
    // must never appear anywhere in the persisted rows.
    let evidence_blob: String = db
        .conn()
        .query_row(
            "SELECT usage_json FROM accounting_evidence WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!evidence_blob.contains("sk-test"));
    assert!(!evidence_blob.contains("done"));
}

#[tokio::test]
async fn provider_response_identity_and_resolved_model_enrich_the_ledger() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[completed(
            "resp_alias_drift",
            "gpt-5-2026-09-01",
            json!({
                "input_tokens": 10, "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 3, "output_tokens_details": {"reasoning_tokens": 0},
            }),
        )],
    )
    .await;

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let authorizer = write_scope_authorizer_from_prd(SAMPLE_PRD, vec![], &no_sandbox()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut executor = SandboxedToolExecutor {
        worktree_root: temp.path().to_path_buf(),
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");
    let adapter = OpenAiInferenceAdapter::new(
        "sk-test",
        OpenAiResponsesConfig {
            base_url: server.uri(),
            request_timeout_secs: 5,
            service_tier: None,
        },
    )
    .unwrap();
    let config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        // The worker spec pins the moving alias "gpt-5" — never the
        // response-resolved snapshot.
        model: "gpt-5".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![],
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
            bytes: "ctx".into(),
            version: "v1".into(),
        },
        &VolatileTask { bytes: "go".into() },
        &config,
        attempt_id_source(),
    )
    .await;
    assert_eq!(outcome.attempts.len(), 1);
    let attempt = &outcome.attempts[0];

    // `persist_run_outcome`'s shared, adapter-neutral helper has no field
    // for a per-attempt resolved model or provider response id (see its
    // doc comment); the alias pinned in worker configuration is never
    // overwritten by it. The adapter's own metadata recovers both facts
    // and a host enriches its own ledger row through the ordinary
    // `AccountingRepository` API — no loop or persistence *semantics*
    // change to do so.
    let meta = adapter.response_meta(&attempt.attempt_id).unwrap();
    assert_eq!(meta.resolved_model.as_deref(), Some("gpt-5-2026-09-01"));
    assert_ne!(meta.resolved_model.as_deref(), Some(config.model.as_str()));
    assert_eq!(
        meta.provider_request_id.as_deref(),
        Some("resp_alias_drift")
    );

    let accounting = AccountingRepository::new(db.conn());
    let source_event_hash = format!("openai-enriched:{}", attempt.attempt_id.0);
    let first = accounting
        .append_observation(&UsageObservation {
            execution_id: "exec_1",
            attempt_id: &attempt.attempt_id.0,
            stage: "raw-runtime-review",
            session_id: None,
            worker_identity: "worker_1",
            adapter: "openai-api",
            cli_version: None,
            model_identity: meta.resolved_model.as_deref(),
            service_tier: meta.service_tier.as_deref(),
            provider_request_id: meta.provider_request_id.as_deref(),
            uncached_input_tokens: attempt.usage.uncached_input_tokens,
            cache_read_tokens: attempt.usage.cache_read_tokens,
            cache_write_tokens: attempt.usage.cache_write_tokens,
            output_tokens: attempt.usage.output_tokens,
            reasoning_output_tokens: attempt.usage.reasoning_output_tokens,
            unknown_reason: None,
            period_start: "2026-09-01T00:00:00Z",
            period_end: "2026-09-01T00:00:01Z",
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
        .unwrap()
        .unwrap();

    let (stored_model, stored_request_id): (Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT model_identity,provider_request_id FROM usage_observations WHERE observation_id=?1",
            [&first],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_model.as_deref(), Some("gpt-5-2026-09-01"));
    assert_eq!(stored_request_id.as_deref(), Some("resp_alias_drift"));

    // Replaying the same persisted provider event is idempotent: the same
    // source-event hash returns the same observation, not a duplicate row.
    let second = accounting
        .append_observation(&UsageObservation {
            execution_id: "exec_1",
            attempt_id: &attempt.attempt_id.0,
            stage: "raw-runtime-review",
            session_id: None,
            worker_identity: "worker_1",
            adapter: "openai-api",
            cli_version: None,
            model_identity: meta.resolved_model.as_deref(),
            service_tier: meta.service_tier.as_deref(),
            provider_request_id: meta.provider_request_id.as_deref(),
            uncached_input_tokens: attempt.usage.uncached_input_tokens,
            cache_read_tokens: attempt.usage.cache_read_tokens,
            cache_write_tokens: attempt.usage.cache_write_tokens,
            output_tokens: attempt.usage.output_tokens,
            reasoning_output_tokens: attempt.usage.reasoning_output_tokens,
            unknown_reason: None,
            period_start: "2026-09-01T00:00:00Z",
            period_end: "2026-09-01T00:00:01Z",
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
        .unwrap()
        .unwrap();
    assert_eq!(first, second);
    let row_count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM usage_observations WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(row_count, 1);
}

#[test]
fn resolved_model_binds_a_versioned_dated_price_schedule_to_a_local_estimate() {
    // The PRD-051 versioned-schedule config shape is provider-neutral —
    // `[execution_history.price_schedules."<id>"]` keys by model string —
    // and reconciliation (PRD-053/054) reads whatever is in
    // `cost_estimates`/`price_schedules` however it was produced. No
    // PRD-060 acceptance criterion requires inventing a new generic
    // config-to-estimate computation pipeline (none exists for any
    // provider yet); this proves the dimensions this adapter records
    // (resolved model, distinct token categories) are exactly what a
    // versioned schedule needs to key on and price a `local-estimate` row
    // that the existing append-only `cost_estimates` schema already
    // supports.
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let schedule = PriceScheduleConfig {
        effective_at: "2026-09-01T00:00:00Z".into(),
        currency: PriceCurrency::USD,
        calculation_version: "openai-2026-09-01".into(),
        models: std::collections::BTreeMap::from([(
            "gpt-5-2026-09-01".to_string(),
            PriceScheduleRateConfig {
                uncached_input_nanousd_per_million: Some(1_250_000),
                cache_read_nanousd_per_million: Some(125_000),
                cache_write_nanousd_per_million: None,
                output_nanousd_per_million: Some(10_000_000),
                reasoning_output_nanousd_per_million: Some(10_000_000),
            },
        )]),
    };
    let rate = &schedule.models["gpt-5-2026-09-01"];
    let schedule_id = "openai-2026-09-01";
    db.conn()
        .execute(
            "INSERT INTO price_schedules(schedule_id,effective_at,currency,calculation_version,rates_json,created_at) VALUES(?1,?2,'USD',?3,?4,?5)",
            rusqlite::params![
                schedule_id,
                schedule.effective_at,
                schedule.calculation_version,
                serde_json::to_string(&schedule.models).unwrap(),
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();

    let accounting = AccountingRepository::new(db.conn());
    let observation_id = accounting
        .append_observation(&UsageObservation {
            execution_id: "exec_1",
            attempt_id: "att_1",
            stage: "raw-runtime-review",
            session_id: None,
            worker_identity: "worker_1",
            adapter: "openai-api",
            cli_version: None,
            model_identity: Some("gpt-5-2026-09-01"),
            service_tier: None,
            provider_request_id: Some("resp_1"),
            uncached_input_tokens: Some(400),
            cache_read_tokens: Some(100),
            cache_write_tokens: None,
            output_tokens: Some(40),
            reasoning_output_tokens: Some(15),
            unknown_reason: None,
            period_start: "2026-09-01T00:00:00Z",
            period_end: "2026-09-01T00:00:01Z",
            terminal_status: "completed",
            source_event_hash: "price-schedule-test",
            provider_cost_lexical: None,
            project_resolution_evidence: None,
            output_register_id: "raw-runtime-none",
            output_register_version: "raw-runtime-none",
            input_compression_id: "raw-runtime-none",
            input_compression_version: "raw-runtime-none",
            compression_experiment: None,
            compression_lane: None,
        })
        .unwrap()
        .unwrap();

    // Every price-relevant dimension this schedule declares, applied to
    // the exact categories this adapter recorded distinctly.
    let amount_nanousd = (400 * rate.uncached_input_nanousd_per_million.unwrap()) / 1_000_000
        + (100 * rate.cache_read_nanousd_per_million.unwrap()) / 1_000_000
        + (40 * rate.output_nanousd_per_million.unwrap()) / 1_000_000
        + (15 * rate.reasoning_output_nanousd_per_million.unwrap()) / 1_000_000;

    db.conn()
        .execute(
            "INSERT INTO cost_estimates(estimate_id,observation_id,billing_mode,provenance,unit,amount,schedule_id,calculation_version,created_at) VALUES(?1,?2,'local-estimate','configured-rate','nanoUSD',?3,?4,?5,?6)",
            rusqlite::params![
                "est_1",
                observation_id,
                amount_nanousd as i64,
                schedule_id,
                schedule.calculation_version,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();

    let (stored_amount, stored_schedule, stored_version): (i64, String, String) = db
        .conn()
        .query_row(
            "SELECT amount,schedule_id,calculation_version FROM cost_estimates WHERE observation_id=?1",
            [&observation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored_amount, amount_nanousd as i64);
    assert_eq!(stored_schedule, schedule_id);
    assert_eq!(stored_version, "openai-2026-09-01");

    // A price change is a new schedule, never a rewrite of this estimate
    // (PRD-051's append-only rule) — attempting to update the row fails.
    let err = db
        .conn()
        .execute(
            "UPDATE cost_estimates SET amount=0 WHERE observation_id=?1",
            [&observation_id],
        )
        .unwrap_err();
    assert!(err.to_string().contains("append-only"));
}

#[test]
fn missing_env_credential_fails_closed_with_the_exact_byo_auth_remedy() {
    const KEY_ENV: &str = "FAMILIAR_AI_TEST_OPENAI_API_KEY_PRD060_MISSING";
    std::env::remove_var(KEY_ENV);
    let auth = AuthDescriptor::Env(KEY_ENV.to_string());
    let error = check_auth(&auth).unwrap_err();
    assert!(error.contains(&format!("export `{KEY_ENV}`")));
}

#[test]
fn resolved_credential_never_prints_and_feeds_the_adapter_boundary_only() {
    const KEY_ENV: &str = "FAMILIAR_AI_TEST_OPENAI_API_KEY_PRD060_PRESENT";
    std::env::set_var(KEY_ENV, "sk-super-secret-value");
    let auth = AuthDescriptor::Env(KEY_ENV.to_string());
    let credential = check_auth(&auth).unwrap().unwrap();
    assert_eq!(format!("{credential:?}"), "ResolvedCredential([REDACTED])");

    let adapter = OpenAiInferenceAdapter::new(
        credential.expose_for_request(),
        OpenAiResponsesConfig::default(),
    )
    .unwrap();
    assert_eq!(adapter.runtime_id().to_string(), "openai-api");
    std::env::remove_var(KEY_ENV);
}
