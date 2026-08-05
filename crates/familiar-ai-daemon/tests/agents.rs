//! Deterministic agent construction and configuration resolution.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use familiar_ai_agent::{
    BudgetRefusal, ExecutionBudget, ExecutionRequest, FilesystemPolicy, IsolationCapability,
};
use familiar_ai_core::{
    config::ReviewAgentConfig, AgentAdapterKind, AgentEntryConfig, AgentPermissionMode,
    AgentsConfig, Config,
};
use familiar_ai_daemon::run::{build_agent, resolved_agent_entries};

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
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
        assert_eq!(entry.max_budget_microusd, 0);
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
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-test; exit 0; fi\nprintf '%s\\n' \"$@\" > {}\ncat >/dev/null\nexit 0\n",
            codex_argv.display()
        ),
    );
    write_executable(
        &claude_exe,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo claude-test; exit 0; fi\nprintf '%s\\n' \"$@\" > {}\ncat >/dev/null\nexit 0\n",
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
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: ExecutionBudget::NONE,
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

    // A declared per-execution cost ceiling reaches the Claude Code adapter
    // as --max-budget-usd, exactly once, correctly formatted.
    let mut budgeted = request(temp.path());
    budgeted.budget = ExecutionBudget::try_new(Some(8_000_000), None, None).unwrap();
    claude_agent.execute(budgeted, &mut Vec::new()).unwrap();
    let argv = fs::read_to_string(&claude_argv).unwrap();
    assert_eq!(
        argv,
        "--print\n--output-format\nstream-json\n--verbose\n--max-budget-usd\n8.000000\n--permission-mode\nacceptEdits\n"
    );
    assert_eq!(argv.matches("--max-budget-usd").count(), 1);
}

#[test]
fn execution_budget_config_fields_round_trip_through_agent_entry() {
    let entry = AgentEntryConfig {
        adapter: AgentAdapterKind::ClaudeCode,
        max_execution_cost_microusd: Some(8_000_000),
        ..AgentEntryConfig::default()
    };
    let budget = ExecutionBudget::try_new(
        entry.max_execution_cost_microusd,
        entry.max_execution_tokens,
        entry.max_execution_duration_ms,
    )
    .unwrap();
    assert_eq!(budget.max_cost_microusd, Some(8_000_000));
    assert!(budget.max_tokens.is_none());
}

#[test]
fn a_denomination_the_constructed_adapter_cannot_enforce_is_refused_before_launch() {
    let codex_agent = build_agent(&AgentEntryConfig {
        adapter: AgentAdapterKind::Codex,
        executable: Some("codex".into()),
        ..AgentEntryConfig::default()
    });
    let budget = ExecutionBudget::try_new(Some(1), None, None).unwrap();
    let error = budget
        .validate_for_adapter(&codex_agent.budget_capability(), "codex")
        .unwrap_err();
    assert_eq!(
        error,
        BudgetRefusal::Unenforceable {
            adapter: "codex".into(),
            denomination: "cost",
        }
    );

    // The Claude Code adapter enforces cost but not tokens or duration.
    let claude_agent = build_agent(&AgentEntryConfig {
        adapter: AgentAdapterKind::ClaudeCode,
        executable: Some("claude".into()),
        ..AgentEntryConfig::default()
    });
    let token_budget = ExecutionBudget::try_new(None, Some(1), None).unwrap();
    assert!(matches!(
        token_budget.validate_for_adapter(&claude_agent.budget_capability(), "claude-code"),
        Err(BudgetRefusal::Unenforceable { .. })
    ));
    let cost_budget = ExecutionBudget::try_new(Some(1), None, None).unwrap();
    assert!(cost_budget
        .validate_for_adapter(&claude_agent.budget_capability(), "claude-code")
        .is_ok());
}
