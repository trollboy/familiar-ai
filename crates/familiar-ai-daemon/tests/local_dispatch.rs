//! PRD-094 production reachability proof. This begins at the same
//! `resolved_worker_plan`/`build_selected_agents` seam used by `run` and
//! drives the selected implementation agent through a loopback fake.

use std::collections::BTreeMap;

use familiar_ai_agent::{ExecutionBudget, ExecutionRequest, FilesystemPolicy};
use familiar_ai_core::config::{
    LocalEndpointConfig, LocalResourceProfileConfig, LocalRuntimeKind, LocalWorkerConfig,
    RegistryWorkerConfig, WorkerCapabilityConfig, WorkerRegistryConfig,
};
use familiar_ai_core::Config;
use familiar_ai_daemon::run::{build_selected_agents, resolved_worker_plan, RouteContext};
use familiar_ai_storage::Database;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(frame: serde_json::Value) -> String {
    format!("data: {frame}\n\ndata: [DONE]\n\n")
}

#[test]
fn selected_local_worker_executes_with_hardware_reservation_and_telemetry() {
    let mock_runtime = tokio::runtime::Runtime::new().unwrap();
    let server = mock_runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(serde_json::json!({
                    "choices": [{"delta": {"content": "implemented"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 4}
                })),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        server
    });

    let worker = RegistryWorkerConfig {
        adapter: None,
        provider: "local".into(),
        model: "llama3".into(),
        runtime: Some("ollama".into()),
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        local: Some(LocalWorkerConfig {
            runtime_kind: LocalRuntimeKind::Ollama,
            endpoint: LocalEndpointConfig {
                base_url: server.uri(),
                tls: false,
            },
            resources: LocalResourceProfileConfig {
                concurrent_inference_slots: Some(1),
                ..Default::default()
            },
        }),
        // If production construction accidentally falls back to a CLI
        // factory, launch must fail. Success therefore proves no vendor
        // subprocess was involved.
        executable: Some("/nonexistent/vendor-cli-must-never-run".into()),
        capabilities: vec![
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Remediation,
        ],
        fresh_process_isolation: true,
        context_tokens: 8_000,
        estimated_cost_microusd: Some(1),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    };
    let mut config = Config::default();
    config.agent_runtime.enabled = true;
    config.agent_runtime.offered_capabilities = vec!["report-progress".into()];
    config.worker_registry = Some(WorkerRegistryConfig {
        workers: BTreeMap::from([("local-ollama".into(), worker)]),
        capability_profiles: BTreeMap::new(),
        routing: Default::default(),
    });

    let (_, _, records) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    assert_eq!(records[0].selected_worker, "local-ollama");

    let state = tempfile::tempdir().unwrap();
    let database_path = state.path().join("familiar.db");
    let db = Database::open(&database_path).unwrap();
    db.run_migrations().unwrap();
    db.conn().execute(
        "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES('exec_local_dispatch','now','local-ollama','running','repo','wt','docs/prds/PRD-094.md','[]')",
        [],
    ).unwrap();
    drop(db);
    let worktree = tempfile::tempdir().unwrap();
    let agents = build_selected_agents(
        &config,
        &RouteContext::default(),
        "exec_local_dispatch",
        &database_path,
        "project-094",
        worktree.path(),
        "# PRD-094\n",
    )
    .unwrap()
    .unwrap();
    agents.0.preflight().unwrap();
    let result = agents.0.execute(
        ExecutionRequest {
            working_directory: worktree.path(),
            denied_read_path: None,
            prompt: "implement PRD-094",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: ExecutionBudget::default(),
        },
        &mut Vec::new(),
    );
    assert!(result.is_ok(), "owned local execution failed: {result:?}");

    let db = Database::open(&database_path).unwrap();
    let local_reservation: (String, String) = db
        .conn()
        .query_row(
            "SELECT owner_kind,state FROM resource_reservations WHERE execution_id='exec_local_dispatch' AND owner_kind='local-agent'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        local_reservation,
        ("local-agent".into(), "committed".into())
    );
    let telemetry: (String, u64) = db
        .conn()
        .query_row(
            "SELECT attempt_id,output_tokens FROM local_worker_telemetry WHERE execution_id='exec_local_dispatch'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!telemetry.0.is_empty());
    assert_ne!(telemetry.0, "no-attempt");
    assert_eq!(telemetry.1, 4);
}
