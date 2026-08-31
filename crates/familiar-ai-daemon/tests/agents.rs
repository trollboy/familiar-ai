//! Deterministic agent construction and configuration resolution.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use familiar_ai_agent::{ExecutionRequest, FilesystemPolicy, IsolationCapability};
use familiar_ai_core::{
    config::{ReviewAgentConfig, WorkerRouteRuleConfig},
    AgentAdapterKind, AgentEntryConfig, AgentPermissionMode, AgentsConfig, Config,
    RepositoryConfig,
};
use familiar_ai_daemon::run::{
    build_agent, next_implementation_worker, resolved_agent_entries, resolved_worker_plan,
    RouteContext,
};

#[test]
fn verification_escalation_selects_only_the_next_stronger_tier() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        temp.path(),
        r#"
[worker_registry.workers.cheap]
adapter = "codex"
provider = "openai"
model = "small"
capabilities = ["implementation", "remediation"]
context_tokens = 2000
estimated_cost_microusd = 1

[worker_registry.workers.strong]
adapter = "claude-code"
provider = "anthropic"
model = "strong"
capabilities = ["implementation", "remediation"]
context_tokens = 2000
estimated_cost_microusd = 5

[worker_registry.workers.strongest]
adapter = "codex"
provider = "openai"
model = "largest"
capabilities = ["implementation", "remediation"]
context_tokens = 2000
estimated_cost_microusd = 10
"#,
    )
    .unwrap();
    let mut config = Config::load(Some(temp.path())).unwrap();
    let next = next_implementation_worker(&config, &RouteContext::default())
        .unwrap()
        .unwrap();
    assert_eq!(next.0, "strong");
    config
        .worker_registry
        .as_mut()
        .unwrap()
        .routing
        .implementation_pin = Some("cheap".into());
    assert!(
        next_implementation_worker(&config, &RouteContext::default())
            .unwrap()
            .is_none()
    );
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn registry_routes_every_stage_deterministically_and_honors_pin() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        temp.path(),
        r#"
[worker_registry.routing]
implementation_pin = "claude"
max_stage_cost_microusd = 100
required_context_tokens = 1000

[worker_registry.workers.codex]
adapter = "codex"
provider = "openai"
model = "gpt"
capabilities = ["planning", "implementation", "review", "remediation", "narrow-task"]
fresh_process_isolation = true
context_tokens = 2000
estimated_cost_microusd = 1

[worker_registry.workers.claude]
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
capabilities = ["planning", "implementation", "review", "remediation", "narrow-task"]
fresh_process_isolation = true
context_tokens = 2000
estimated_cost_microusd = 2
"#,
    )
    .unwrap();
    let config = Config::load(Some(temp.path())).unwrap();
    let (implementation, _, first) =
        resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    let (_, _, second) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(implementation.adapter, AgentAdapterKind::ClaudeCode);
    assert_eq!(first[0].rule, "user-pin");
    assert_eq!(first[0].selected_worker, "claude");
    assert!(first[0]
        .candidates
        .iter()
        .any(|candidate| candidate.worker_id == "codex" && !candidate.rejected.is_empty()));
}

#[test]
fn risk_rule_overrides_one_file_tiebreak_and_reviewer_stays_independent() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        temp.path(),
        r#"
[worker_registry.workers.cheap]
adapter = "codex"
provider = "openai"
model = "small"
capabilities = ["implementation", "review", "remediation"]
fresh_process_isolation = true
context_tokens = 2000
estimated_cost_microusd = 1

[worker_registry.workers.high-risk]
adapter = "claude-code"
provider = "anthropic"
model = "strong"
capabilities = ["implementation", "review", "remediation"]
fresh_process_isolation = true
context_tokens = 2000
estimated_cost_microusd = 2
"#,
    )
    .unwrap();
    let mut config = Config::load(Some(temp.path())).unwrap();
    config.repositories.insert(
        "/unused".into(),
        RepositoryConfig {
            risk_vocabulary: vec!["security".into()],
            ..RepositoryConfig::default()
        },
    );
    config
        .worker_registry
        .as_mut()
        .unwrap()
        .routing
        .rules
        .push(WorkerRouteRuleConfig {
            id: "high-risk".into(),
            worker: "high-risk".into(),
            risk_classes: vec!["security".into()],
            max_expected_files: None,
        });
    config.review.enabled = true;

    let (_, reviewer, records) = resolved_worker_plan(
        &config,
        &RouteContext {
            risk_classes: vec!["security".into()],
            expected_file_count: 1,
        },
    )
    .unwrap();
    assert_eq!(records[0].selected_worker, "high-risk");
    assert_eq!(records[0].rule, "high-risk");
    assert_eq!(reviewer.model.as_deref(), Some("small"));
    let review = records
        .iter()
        .find(|record| record.stage == familiar_ai_agent::WorkerStage::Review)
        .unwrap();
    assert_eq!(review.selected_worker, "cheap");

    let (_, _, ordinary_records) = resolved_worker_plan(
        &config,
        &RouteContext {
            risk_classes: vec![],
            expected_file_count: 1,
        },
    )
    .unwrap();
    assert_eq!(ordinary_records[0].selected_worker, "cheap");
    assert_ne!(ordinary_records[0], records[0]);
}

#[test]
fn ollama_registry_entry_uses_existing_codex_oss_adapter() {
    let entry = familiar_ai_core::config::RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Ollama),
        provider: "ollama".into(),
        model: "qwen3:8b".into(),
        runtime: None,
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        executable: None,
        capabilities: vec![familiar_ai_core::config::WorkerCapabilityConfig::Implementation],
        fresh_process_isolation: true,
        context_tokens: 1,
        estimated_cost_microusd: 0,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    }
    .as_agent_entry();
    assert_eq!(entry.resolved_executable(), "codex");
    assert_eq!(entry.model.as_deref(), Some("ollama/qwen3:8b"));
}

#[test]
fn absent_agents_section_resolves_to_historical_codex_defaults() {
    let config = Config::default();
    assert!(config.agents.is_none());
    let (implementation, reviewer) = resolved_agent_entries(&config).unwrap();
    for entry in [&implementation, &reviewer] {
        assert_eq!(entry.adapter, AgentAdapterKind::Codex);
        assert_eq!(entry.resolved_executable(), "codex");
        assert_eq!(entry.model, None);
        assert_eq!(entry.effort, None);
        assert_eq!(entry.permission_mode, None);
        assert_eq!(entry.max_execution_cost_microusd, 0);
        assert!(entry.extra_args.is_empty());
    }
}

#[test]
fn contradictory_review_identity_fails_resolution_when_section_present() {
    let mut config = Config::default();
    config.review.enabled = true;
    config.review.implementation_agent = ReviewAgentConfig {
        adapter_id: "codex".into(),
        agent_id: "implementation".into(),
        provider: None,
        model: None,
    };
    config.review.reviewer_agent = ReviewAgentConfig {
        adapter_id: "codex".into(),
        agent_id: "reviewer".into(),
        provider: None,
        model: None,
    };
    config.agents = Some(AgentsConfig {
        implementation: AgentEntryConfig {
            adapter: AgentAdapterKind::ClaudeCode,
            ..AgentEntryConfig::default()
        },
        reviewer: AgentEntryConfig::default(),
    });
    let error = resolved_agent_entries(&config).unwrap_err();
    assert!(error.contains("contradicts"), "got: {error}");
    // Absent review model or matching identity passes.
    config.agents.as_mut().unwrap().implementation.adapter = AgentAdapterKind::Codex;
    assert!(resolved_agent_entries(&config).is_ok());
}

#[test]
fn build_agent_maps_each_adapter_to_its_invocation_shape() {
    let temp = tempfile::tempdir().unwrap();
    let codex_argv = temp.path().join("codex-argv.txt");
    let claude_argv = temp.path().join("claude-argv.txt");
    let codex_exe = temp.path().join("codex");
    let claude_exe = temp.path().join("claude");
    write_executable(
        &codex_exe,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-test; exit 0; fi\nprintf '%s\\n' \"$@\" > {}\ncat >/dev/null\nprintf '%s\\n' '{{\"type\":\"turn.completed\"}}'\nexit 0\n",
            codex_argv.display()
        ),
    );
    write_executable(
        &claude_exe,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo claude-test; exit 0; fi\nprintf '%s\\n' \"$@\" > {}\ncat >/dev/null\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\"}}'\nexit 0\n",
            claude_argv.display()
        ),
    );

    let codex_agent = build_agent(&AgentEntryConfig {
        adapter: AgentAdapterKind::Codex,
        executable: Some(codex_exe.to_string_lossy().into_owned()),
        ..AgentEntryConfig::default()
    });
    let claude_agent = build_agent(&AgentEntryConfig {
        adapter: AgentAdapterKind::ClaudeCode,
        executable: Some(claude_exe.to_string_lossy().into_owned()),
        permission_mode: Some(AgentPermissionMode::AcceptEdits),
        ..AgentEntryConfig::default()
    });
    for agent in [&codex_agent, &claude_agent] {
        assert_eq!(
            agent.isolation_capability(),
            IsolationCapability::FreshProcessPerExecution
        );
    }
    fn request(working: &Path) -> ExecutionRequest<'_> {
        ExecutionRequest {
            working_directory: working,
            denied_read_path: None,
            prompt: "prompt",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: familiar_ai_agent::ExecutionBudget::default(),
        }
    }
    codex_agent
        .execute(request(temp.path()), &mut Vec::new())
        .unwrap();
    claude_agent
        .execute(request(temp.path()), &mut Vec::new())
        .unwrap();
    assert_eq!(
        fs::read_to_string(&codex_argv).unwrap(),
        "exec\n--json\n-\n"
    );
    assert_eq!(
        fs::read_to_string(&claude_argv).unwrap(),
        "--print\n--output-format\nstream-json\n--verbose\n--permission-mode\nacceptEdits\n"
    );
}
