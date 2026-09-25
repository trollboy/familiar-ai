use assert_cmd::Command;
use familiar_ai_core::config::HostWorkerFacts;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[test]
fn host_with_only_api_key_selects_non_cli_worker() {
    let (_, worker) = HostWorkerFacts {
        openai_api_key: true,
        ..Default::default()
    }
    .selected_worker()
    .unwrap();
    assert_eq!(worker.runtime.as_deref(), Some("openai-api"));
    assert!(worker.executable.is_none());
}

#[test]
fn host_with_only_local_endpoint_selects_owned_loop() {
    let (_, worker) = HostWorkerFacts {
        ollama_endpoint: Some("http://127.0.0.1:8888".into()),
        ..Default::default()
    }
    .selected_worker()
    .unwrap();
    assert_eq!(worker.runtime.as_deref(), Some("ollama"));
    assert_eq!(
        worker.runtime_config.unwrap().host.as_deref(),
        Some("http://127.0.0.1:8888")
    );
}

#[test]
fn host_with_only_cli_preserves_cli_path() {
    let (_, worker) = HostWorkerFacts {
        codex_cli: true,
        ..Default::default()
    }
    .selected_worker()
    .unwrap();
    assert_eq!(worker.runtime.as_deref(), Some("codex"));
}

#[test]
fn host_with_nothing_gets_every_working_path() {
    let error = HostWorkerFacts::default().selected_worker().unwrap_err();
    for remedy in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "Ollama",
        "claude/codex CLI",
    ] {
        assert!(error.contains(remedy), "missing {remedy}: {error}");
    }
}

#[test]
fn documentation_does_not_make_a_vendor_cli_mandatory() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in ["README.md", "docs/guides/provider-setup.md"] {
        let text = std::fs::read_to_string(root.join(relative)).unwrap();
        let lower = text.to_ascii_lowercase();
        assert!(
            !lower.contains("at least one coding agent cli"),
            "{relative}"
        );
        assert!(!lower.contains("cli is a real prerequisite"), "{relative}");
    }
}

#[test]
fn onboarding_on_a_cli_less_host_writes_a_non_cli_worker() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("worker.toml");
    Command::cargo_bin("familiar-ai")
        .unwrap()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("OPENAI_API_KEY", "test-only-not-a-secret")
        .args([
            "onboard",
            "worker-config",
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let generated = std::fs::read_to_string(output).unwrap();
    assert!(generated.contains("runtime = \"openai-api\""));
    assert!(!generated.contains("runtime = \"codex\""));
    assert!(!generated.contains("runtime = \"claude-code\""));
}

fn sse(frames: &[serde_json::Value]) -> String {
    let mut body = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>();
    body.push_str("data: [DONE]\n\n");
    body
}

fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(value) => out.push(value.clone()),
        serde_json::Value::Array(values) => values.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(values) => values.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

struct MustBeReplaced;
impl familiar_ai_agent::CodingAgent for MustBeReplaced {
    fn execute(
        &self,
        _: familiar_ai_agent::ExecutionRequest<'_>,
        _: &mut dyn std::io::Write,
    ) -> Result<familiar_ai_agent::ExecutionResult, familiar_ai_agent::AgentExecutionError> {
        panic!("registry-selected raw worker must replace the placeholder")
    }
}

/// The whole attached workflow runs through the owned raw loop. Both worker
/// executables are executable sentinels which would leave a marker if a
/// vendor subprocess were launched; the marker remains absent.
#[test]
fn cli_less_raw_worker_completes_claim_verify_review_and_integration() {
    use familiar_ai_core::config::{
        OllamaRuntimeConfig, RegistryWorkerConfig, ReviewAgentConfig, ReviewVerificationConfig,
        WorkerCapabilityConfig, WorkerRegistryConfig, WorkerRoutingConfig,
    };
    use familiar_ai_core::{AppPaths, Config};
    use familiar_ai_daemon::run::{execute_with_config_tracked_from, AgentSet};

    struct PathGuard(Option<std::ffi::OsString>);
    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                unsafe { std::env::set_var("PATH", path) };
            }
        }
    }
    let _path = PathGuard(std::env::var_os("PATH"));
    unsafe { std::env::set_var("PATH", "/usr/bin:/bin") };
    for vendor in ["codex", "claude"] {
        assert!(!std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .any(|dir| dir.join(vendor).exists()));
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        let calls = calls.clone();
        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let ordinal = calls.fetch_add(1, Ordering::SeqCst);
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let mut text = Vec::new();
                strings(&body, &mut text);
                let prompt = text.join("\n");
                let frames = if prompt.contains("FAMILIAR_AI_REVIEW_CAPABILITY_PROBE") {
                    vec![serde_json::json!({
                        "choices": [{"delta": {"content": "{\"structured_output\":true,\"native_tool_calling\":true,\"protocol\":\"familiar-ai-review-v1\",\"runtime_version\":\"0.13.4\"}"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
                    })]
                } else if prompt.contains("independent code reviewer") {
                    if let Some(package) = text.iter().find_map(|candidate| {
                        serde_json::from_str::<serde_json::Value>(candidate)
                            .ok()
                            .filter(|value| value.get("review_id").is_some())
                    }) {
                        let clean = serde_json::json!({
                            "review_id": package["review_id"],
                            "reviewed_manifest_hash": package["manifest"]["manifest_hash"],
                            "findings": []
                        })
                        .to_string();
                        vec![serde_json::json!({
                            "choices": [{"delta": {"content": clean}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 8, "completion_tokens": 2}
                        })]
                    } else {
                        vec![
                            serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "read-review", "function": {"name": "read-file", "arguments": ""}}]}}]}),
                            serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"review-request.json\"}"}}]}}]}),
                            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 4, "completion_tokens": 2}}),
                        ]
                    }
                } else if ordinal == 0 {
                    vec![
                        serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "edit", "function": {"name": "apply-edit", "arguments": ""}}]}}]}),
                        serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"src/lib.rs\",\"content\":\"implemented\\n\"}"}}]}}]}),
                        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 10, "completion_tokens": 4}}),
                    ]
                } else {
                    vec![serde_json::json!({
                        "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 1}
                    })]
                };
                ResponseTemplate::new(200)
                    .set_body_raw(sse(&frames), "text/event-stream")
            })
            .mount(&server)
            .await;
        server
    });

    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    std::fs::create_dir_all(repository.join("docs/prds")).unwrap();
    std::fs::create_dir_all(repository.join("src")).unwrap();
    std::fs::write(repository.join("src/lib.rs"), "base\n").unwrap();
    std::fs::write(
        repository.join("docs/prds/PRD-001.md"),
        "# PRD-001: raw e2e\n\n**Status:** Ready for implementation\n\n## Objective\n\nChange the file.\n\n## Acceptance Criteria\n\n1. The file changes.\n\n## Expected Files\n\n- `src/lib.rs`\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }
    let marker = temp.path().join("vendor-launched");
    let sentinel = temp.path().join("vendor-cli");
    std::fs::write(
        &sentinel,
        format!("#!/bin/sh\ntouch '{}'\nexit 99\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let worker = |model: &str, capabilities| RegistryWorkerConfig {
        adapter: None,
        provider: model.into(),
        model: model.into(),
        runtime: Some("ollama".into()),
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: Some(OllamaRuntimeConfig {
            host: Some(server.uri()),
        }),
        local: None,
        executable: Some(sentinel.display().to_string()),
        capabilities,
        fresh_process_isolation: true,
        context_tokens: 0,
        estimated_cost_microusd: Some(1),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    };
    let mut config = Config::default();
    config.database.path = Some(temp.path().join("db.sqlite"));
    config.agent_runtime.enabled = true;
    config
        .agent_runtime
        .offered_capabilities
        .push("apply-edit".into());
    config.review.enabled = true;
    config.review.max_review_attempts = 2;
    config.review.max_remediation_attempts = 1;
    config.review.max_total_duration_ms = 10_000;
    config.review.allowed_paths = vec!["src/".into()];
    config.review.verification = vec![ReviewVerificationConfig {
        check_id: "verify".into(),
        argv: vec!["/usr/bin/true".into()],
        working_directory: ".".into(),
        timeout_ms: 2_000,
        required: true,
        path_prefixes: vec!["src/".into()],
        environment: BTreeMap::new(),
    }];
    config.review.implementation_agent = ReviewAgentConfig {
        adapter_id: "ollama".into(),
        agent_id: "implementer".into(),
        provider: Some("implementer".into()),
        model: Some("implementer".into()),
    };
    config.review.reviewer_agent = ReviewAgentConfig {
        adapter_id: "ollama".into(),
        agent_id: "reviewer".into(),
        provider: Some("reviewer".into()),
        model: Some("reviewer".into()),
    };
    config.worker_registry = Some(WorkerRegistryConfig {
        workers: BTreeMap::from([
            (
                "implementer".into(),
                worker(
                    "implementer",
                    vec![
                        WorkerCapabilityConfig::Implementation,
                        WorkerCapabilityConfig::Remediation,
                    ],
                ),
            ),
            (
                "reviewer".into(),
                worker("reviewer", vec![WorkerCapabilityConfig::Review]),
            ),
        ]),
        capability_profiles: BTreeMap::new(),
        routing: WorkerRoutingConfig {
            implementation_pin: Some("implementer".into()),
            review_pin: Some("reviewer".into()),
            remediation_pin: Some("implementer".into()),
            ..Default::default()
        },
    });
    let paths = AppPaths {
        config_dir: temp.path().join("config"),
        data_dir: temp.path().join("data"),
        state_dir: temp.path().join("state"),
        runtime_dir: temp.path().join("runtime"),
        log_dir: temp.path().join("log"),
        socket_path: temp.path().join("runtime/socket"),
        pid_path: temp.path().join("state/pid"),
    };
    let placeholder = MustBeReplaced;
    let (result, trace) = execute_with_config_tracked_from(
        &repository,
        &repository.join("docs/prds/PRD-001.md"),
        &AgentSet {
            implementation: &placeholder,
            reviewer: &placeholder,
            remediation: &placeholder,
        },
        &config,
        &paths,
    );
    assert!(
        result.is_ok(),
        "raw end-to-end workflow failed: {result:?}; {trace:?}"
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("src/lib.rs")).unwrap(),
        "implemented\n"
    );
    assert!(!marker.exists(), "a vendor subprocess was launched");
    let db = familiar_ai_storage::Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let phase: String = db
        .conn()
        .query_row(
            "SELECT phase FROM execution_checkpoints ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(phase, "completed");
}
