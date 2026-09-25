//! PRD-100 dispatch-level regressions: Familiar's own raw-model agent loop
//! is a selectable, executable worker.
//!
//! `crates/familiar-ai-agent/tests/raw_runtime.rs` and
//! `crates/familiar-ai-daemon/tests/raw_runtime.rs` already cover the loop
//! core and its SQLite-backed journal/executor/authorizer/persistence in
//! isolation. `crates/familiar-ai-core/src/config/registry_workers.rs` and
//! `crates/familiar-ai-daemon/src/run.rs` carry unit tests for the
//! config/routing-level pieces (`AgentAdapterKind`, `as_agent_entry`,
//! `build_raw_worker_context`, `resolved_worker_plan`). This file covers
//! what only the whole stack together can prove: a worker declaring a raw
//! runtime, constructed through the same `AdapterFactories` registry the
//! CLI-driven path uses, actually completing a full attempt over a real
//! (loopback-only) HTTP round trip — with neither `claude` nor `codex`
//! anywhere in the picture, let alone on `PATH` — and an unregistered
//! runtime failing closed by name.

use std::sync::Arc;

use familiar_ai_agent::raw_runtime::{CapabilityId, LoopCeilings};
use familiar_ai_agent::{
    builtin_adapter_factories, ExecutionBudget, ExecutionRequest, FilesystemPolicy,
    RawWorkerContext, WorkerDescriptor,
};
use familiar_ai_core::config::LocalEndpointConfig;
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_daemon::agent_runtime::SqliteRawAgentHost;
use familiar_ai_daemon::run::{execute_with_config_tracked_from, AgentSet};
use familiar_ai_storage::Database;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(frames: &[serde_json::Value]) -> String {
    let mut body: String = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn setup_db(execution_id: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("db.sqlite");
    let db = Database::open(&database_path).unwrap();
    db.run_migrations().unwrap();
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'raw-runtime','running','repo','wt','docs/prds/PRD-100.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
    (dir, database_path)
}

fn minimal_descriptor(runtime_id: &str, executable: &str) -> WorkerDescriptor {
    WorkerDescriptor {
        id: "raw-worker".into(),
        spec_identity: "wspec-sha256:test".into(),
        empirical_version: "wver-sha256:test".into(),
        runtime_id: runtime_id.into(),
        provider: "local".into(),
        model: "llama3".into(),
        executable: executable.into(),
        capabilities: Default::default(),
        fresh_process_isolation: true,
        context_tokens: 0,
        estimated_cost_microusd: None,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    }
}

struct PlaceholderAgent;
impl familiar_ai_agent::CodingAgent for PlaceholderAgent {
    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<familiar_ai_agent::ExecutionResult, familiar_ai_agent::AgentExecutionError> {
        panic!("worker registry must replace the placeholder")
    }
}

fn app_paths(root: &std::path::Path) -> AppPaths {
    AppPaths {
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        state_dir: root.join("state"),
        runtime_dir: root.join("runtime"),
        log_dir: root.join("log"),
        socket_path: root.join("runtime/socket"),
        pid_path: root.join("state/pid"),
    }
}

/// A worker declaring a raw runtime executes a full attempt through
/// Familiar's own loop: real HTTP round trip against a loopback fake, real
/// SQLite journal/evidence/usage persistence, no vendor CLI anywhere.
/// `executable` deliberately names a path that does not exist — proof by
/// construction that this path never shells out to `codex` or `claude`,
/// unlike the `CodexFactory` this runtime id used to resolve to.
///
/// `RawAgent::execute` is a synchronous `CodingAgent` method that manages
/// its own tokio runtime internally (matching every other `CodingAgent`,
/// which is a blocking, subprocess-driven call). It cannot be called from
/// inside an async `#[tokio::test]` executor — tokio refuses to start a
/// runtime from within a runtime — so this test is itself synchronous and
/// only uses a manually-owned multi-thread runtime to host the mock server
/// in the background while `execute` runs its own, separate runtime.
#[test]
fn full_prd_cycle_executes_through_the_owned_loop_with_no_vendor_cli_present() {
    let mock_runtime = tokio::runtime::Runtime::new().unwrap();
    let server = mock_runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[serde_json::json!({
                    "choices": [{"delta": {"content": "implemented"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 4}
                })]),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        server
    });

    let (_dir, database_path) = setup_db("exec_full_cycle");
    let worktree = tempfile::tempdir().unwrap();

    let descriptor = minimal_descriptor("ollama", "/nonexistent/vendor-cli-must-never-run");
    let host = SqliteRawAgentHost::new(
        database_path.clone(),
        "exec_full_cycle".into(),
        "proj-100".into(),
        "raw-worker".into(),
        "implementation".into(),
        "ollama".into(),
        Some("llama3".into()),
        worktree.path().to_path_buf(),
        Default::default(),
        Default::default(),
        vec![],
        vec![CapabilityId::ReportProgress],
        2_000,
        4096,
    );
    let raw_context = RawWorkerContext {
        host: Arc::new(host),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ReportProgress],
        prompt_template_version: "agent-loop-prompt/1".into(),
        credential: None,
        local_endpoint: Some(LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        }),
    };

    let agent = builtin_adapter_factories()
        .build(&descriptor, Some(&raw_context))
        .expect("the ollama runtime must be registered against the owned loop");

    // Never spawns a vendor subprocess: preflight succeeds even though
    // `executable` names a path that cannot possibly be launched.
    agent
        .preflight()
        .expect("the owned loop's preflight never shells out to a vendor CLI");

    let mut output = Vec::new();
    let result = agent
        .execute(
            ExecutionRequest {
                working_directory: worktree.path(),
                denied_read_path: None,
                prompt: "implement PRD-100",
                prompt_cache_key: None,
                codex_session: None,
                filesystem: FilesystemPolicy::Normal,
                model: None,
                timeout_ms: None,
                budget: ExecutionBudget::default(),
            },
            &mut output,
        )
        .expect("the owned loop must complete a full attempt with no vendor CLI present");

    assert_eq!(result.output_tokens, Some(4));

    // PRD-051 usage and PRD-058 evidence actually landed: the full
    // authority stack (journal, executor, evidence, ledger) ran, not just
    // the CodingAgent bridge in isolation.
    let db = Database::open(&database_path).unwrap();
    let evidence_count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM agent_runtime_evidence WHERE execution_id='exec_full_cycle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence_count, 1);
    let usage_count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM usage_observations WHERE execution_id='exec_full_cycle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_count, 1);
}

#[test]
fn tracked_run_records_exactly_one_usage_observation_for_one_raw_attempt() {
    use familiar_ai_core::config::{
        OllamaRuntimeConfig, RegistryWorkerConfig, WorkerCapabilityConfig, WorkerRegistryConfig,
    };
    use std::collections::BTreeMap;
    use std::process::Command;

    let mock_runtime = tokio::runtime::Runtime::new().unwrap();
    let server = mock_runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[serde_json::json!({
                    "choices": [{"delta": {"content": "implemented"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 4}
                })]),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        server
    });
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    std::fs::create_dir_all(repository.join("docs/prds")).unwrap();
    std::fs::write(
        repository.join("docs/prds/PRD-001.md"),
        "# PRD-001: raw cycle\n\n**Status:** Ready for implementation\n\n## Acceptance Criteria\n\n1. The raw cycle completes.\n\n## Expected Files\n\n- `src/lib.rs`\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }
    let mut config = Config::default();
    config.database.path = Some(temp.path().join("tracked.sqlite"));
    config.agent_runtime.enabled = true;
    config.worker_registry = Some(WorkerRegistryConfig {
        workers: BTreeMap::from([(
            "raw".into(),
            RegistryWorkerConfig {
                adapter: None,
                provider: "legacy-ollama".into(),
                model: "llama3".into(),
                runtime: Some("ollama".into()),
                model_artifact: None,
                auth_profile: None,
                capability_profile: None,
                runtime_config: Some(OllamaRuntimeConfig {
                    host: Some(server.uri()),
                }),
                local: None,
                executable: None,
                capabilities: vec![
                    WorkerCapabilityConfig::Implementation,
                    WorkerCapabilityConfig::Remediation,
                ],
                fresh_process_isolation: true,
                context_tokens: 0,
                estimated_cost_microusd: Some(1),
                available: true,
                effort: None,
                permission_mode: None,
                extra_args: vec![],
            },
        )]),
        capability_profiles: BTreeMap::new(),
        routing: familiar_ai_core::config::WorkerRoutingConfig {
            implementation_pin: Some("raw".into()),
            remediation_pin: Some("raw".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    let placeholder = PlaceholderAgent;
    let prd = repository.join("docs/prds/PRD-001.md");
    let (_result, trace) = execute_with_config_tracked_from(
        &repository,
        &prd,
        &AgentSet {
            implementation: &placeholder,
            reviewer: &placeholder,
            remediation: &placeholder,
        },
        &config,
        &app_paths(temp.path()),
    );
    assert!(trace.execution_id.is_some());
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let attempts: i64 = db.conn().query_row(
        "SELECT count(*) FROM agent_runtime_attempts WHERE execution_id IN (SELECT execution_id FROM execution_history)",
        [], |row| row.get(0)
    ).unwrap();
    let observations: i64 = db.conn().query_row(
        "SELECT count(*) FROM usage_observations WHERE execution_id IN (SELECT execution_id FROM execution_history)",
        [], |row| row.get(0)
    ).unwrap();
    assert_eq!(attempts, 1);
    assert_eq!(
        observations, attempts,
        "one PRD-051 observation per raw attempt"
    );
}

/// PRD-100 acceptance: every owned-loop attempt records its edit outcome —
/// offered, attempted, refused, applied — as a measurable number, not an
/// assumption. This exercises a *refused* cycle (an out-of-scope
/// `apply-edit`) through the real `RawAgent` bridge, distinct from the
/// completed cycle above: the refusal must still be durably recorded in
/// `agent_runtime_evidence`, and the file must be left untouched.
#[test]
fn a_refused_edit_cycle_records_its_disposition_in_evidence() {
    let mock_runtime = tokio::runtime::Runtime::new().unwrap();
    let server = mock_runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[
                    serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "apply-edit", "arguments": ""}}]}}]}),
                    serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"secrets/keys.pem\",\"content\":\"malicious\"}"}}]}}]}),
                    serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 10, "completion_tokens": 5}}),
                ]),
                "text/event-stream",
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[serde_json::json!({
                    "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 1}
                })]),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        server
    });

    let (_dir, database_path) = setup_db("exec_refused_cycle");
    let worktree = tempfile::tempdir().unwrap();

    let descriptor = minimal_descriptor("ollama", "/nonexistent/vendor-cli-must-never-run");
    let host = SqliteRawAgentHost::new(
        database_path.clone(),
        "exec_refused_cycle".into(),
        "proj-100".into(),
        "raw-worker".into(),
        "implementation".into(),
        "ollama".into(),
        Some("llama3".into()),
        worktree.path().to_path_buf(),
        Default::default(),
        Default::default(),
        // No path is authorized: any apply-edit is out of scope.
        vec![],
        vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress],
        2_000,
        4096,
    );
    let raw_context = RawWorkerContext {
        host: Arc::new(host),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress],
        prompt_template_version: "agent-loop-prompt/1".into(),
        credential: None,
        local_endpoint: Some(LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        }),
    };
    let agent = builtin_adapter_factories()
        .build(&descriptor, Some(&raw_context))
        .unwrap();

    let mut output = Vec::new();
    let result = agent.execute(
        ExecutionRequest {
            working_directory: worktree.path(),
            denied_read_path: None,
            prompt: "implement PRD-100",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: ExecutionBudget::default(),
        },
        &mut output,
    );
    assert!(
        result.is_ok(),
        "a refused tool call is not itself an execution failure: {result:?}"
    );
    assert!(!worktree.path().join("secrets/keys.pem").exists());

    let db = Database::open(&database_path).unwrap();
    let calls_json: String = db
        .conn()
        .query_row(
            "SELECT calls_json FROM agent_runtime_evidence WHERE execution_id='exec_refused_cycle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        calls_json.contains("AuthorizationRefused"),
        "the refusal must be durably recorded: {calls_json}"
    );
}

/// N1 regression: `build_selected_agents` constructs one `SqliteRawAgentHost`
/// per stage and hands back a `Box<dyn CodingAgent>` that is reused for
/// every attempt within that stage — a remediation round, a review re-run,
/// or a retried implementation attempt call `execute` again on the exact
/// same agent/host. Before N1, the first `execute()` call's budget
/// reservation pool was sized to exactly that call's request and consumed by
/// `finish`'s settle; a second `execute()` call re-acquired against the same
/// now-exhausted pool under a reservation-owner id required to be globally
/// unique, and was refused before the adapter could ever be reached — a
/// remediation round failed closed on a budget diagnostic unrelated to the
/// configured budget.
#[test]
fn a_second_execute_call_on_the_same_agent_still_gets_a_reservation() {
    let mock_runtime = tokio::runtime::Runtime::new().unwrap();
    let server = mock_runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[serde_json::json!({
                    "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2}
                })]),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        server
    });

    let (_dir, database_path) = setup_db("exec_remediation_round");
    let worktree = tempfile::tempdir().unwrap();

    let descriptor = minimal_descriptor("ollama", "/nonexistent/vendor-cli-must-never-run");
    let host = SqliteRawAgentHost::new(
        database_path,
        "exec_remediation_round".into(),
        "proj-100".into(),
        "raw-worker".into(),
        "implementation".into(),
        "ollama".into(),
        Some("llama3".into()),
        worktree.path().to_path_buf(),
        Default::default(),
        Default::default(),
        vec![],
        vec![CapabilityId::ReportProgress],
        2_000,
        4096,
    );
    let raw_context = RawWorkerContext {
        host: Arc::new(host),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ReportProgress],
        prompt_template_version: "agent-loop-prompt/1".into(),
        credential: None,
        local_endpoint: Some(LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        }),
    };
    // Constructed once, exactly as `build_selected_agents` does — then
    // reused for two `execute()` calls, the remediation-round shape.
    let agent = builtin_adapter_factories()
        .build(&descriptor, Some(&raw_context))
        .unwrap();

    fn request(dir: &std::path::Path) -> ExecutionRequest<'_> {
        ExecutionRequest {
            working_directory: dir,
            denied_read_path: None,
            prompt: "implement it",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: ExecutionBudget {
                max_cost_microusd: std::num::NonZeroU64::new(1_000),
                ..ExecutionBudget::default()
            },
        }
    }

    let mut first_output = Vec::new();
    agent
        .execute(request(worktree.path()), &mut first_output)
        .expect("the first attempt must be granted a reservation and complete");

    let mut second_output = Vec::new();
    agent
        .execute(request(worktree.path()), &mut second_output)
        .expect(
            "a second execute() call on the same agent/host — a remediation round or retried \
             attempt — must still be granted its own reservation rather than refused against \
             an already-exhausted pool",
        );
}

/// A refused reservation stops the whole execution before the adapter is
/// ever reached: no attempt runs without one (PRD-100 authority parity).
#[tokio::test]
async fn an_attempt_without_a_reservation_cannot_run() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[serde_json::json!({"choices": [{"delta": {"content": "unreachable"}, "finish_reason": "stop"}]})]),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let (_dir, database_path) = setup_db("exec_no_reservation");
    let worktree = tempfile::tempdir().unwrap();

    let host = SqliteRawAgentHost::new(
        database_path,
        "exec_no_reservation".into(),
        "proj-100".into(),
        "raw-worker".into(),
        "implementation".into(),
        "ollama".into(),
        Some("llama3".into()),
        worktree.path().to_path_buf(),
        Default::default(),
        Default::default(),
        vec![],
        vec![CapabilityId::ReportProgress],
        2_000,
        4096,
    );
    let descriptor = minimal_descriptor("ollama", "/nonexistent/vendor-cli-must-never-run");
    let raw_context = RawWorkerContext {
        host: Arc::new(host),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ReportProgress],
        prompt_template_version: "agent-loop-prompt/1".into(),
        credential: None,
        local_endpoint: Some(LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        }),
    };
    let agent = builtin_adapter_factories()
        .build(&descriptor, Some(&raw_context))
        .unwrap();

    let mut output = Vec::new();
    let result = agent.execute(
        ExecutionRequest {
            working_directory: worktree.path(),
            denied_read_path: None,
            prompt: "implement PRD-100",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            // A cost ceiling so large it overflows the nanoUSD range is
            // refused at the reservation gate before any HTTP call.
            timeout_ms: None,
            budget: ExecutionBudget {
                max_cost_microusd: std::num::NonZeroU64::new(u64::MAX),
                ..ExecutionBudget::default()
            },
        },
        &mut output,
    );
    assert!(
        result.is_err(),
        "an execution refused a reservation must never run an attempt"
    );
}

/// An unregistered runtime is refused by name, naming the registered set,
/// before any `CodingAgent` is built — never discovered later at dispatch.
#[test]
fn unregistered_runtime_is_refused_naming_the_registered_set() {
    let descriptor = minimal_descriptor("totally-unregistered-runtime", "nothing");
    let error = builtin_adapter_factories()
        .build(&descriptor, None)
        .err()
        .expect("an unregistered runtime must be refused");
    assert!(error.contains("totally-unregistered-runtime"), "{error}");
    assert!(error.contains("codex"), "{error}");
    assert!(error.contains("anthropic-api"), "{error}");
    assert!(error.contains("openai-api"), "{error}");
}

/// Existing `codex`/`claude-code` dispatch is unaffected: constructing a
/// CLI-driven agent through the same registry, with no `RawWorkerContext`
/// at all, behaves exactly as before PRD-100.
#[test]
fn cli_dispatch_is_unaffected_by_the_owned_loop_registration() {
    let descriptor = minimal_descriptor("codex", "codex");
    let agent = builtin_adapter_factories()
        .build(&descriptor, None)
        .unwrap();
    // A `CodexAgent` reports fresh-process isolation; a `RawAgent` reports
    // `Unavailable` (the default `CodingAgent::isolation_capability`).
    assert_eq!(
        agent.isolation_capability(),
        familiar_ai_agent::IsolationCapability::FreshProcessPerExecution
    );
}
