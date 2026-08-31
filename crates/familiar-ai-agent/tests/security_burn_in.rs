#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use familiar_ai_agent::{
    AgentExecutionError, CodexAgent, CodingAgent, ExecutionBudget, ExecutionRequest,
    FilesystemPolicy,
};

fn executable(body: &str) -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("fake-codex");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, path.display().to_string())
}

fn request<'a>(directory: &'a std::path::Path, prompt: &'a str) -> ExecutionRequest<'a> {
    ExecutionRequest {
        working_directory: directory,
        denied_read_path: None,
        prompt,
        prompt_cache_key: None,
        codex_session: None,
        filesystem: FilesystemPolicy::Normal,
        model: None,
        timeout_ms: Some(2_000),
        budget: ExecutionBudget::default(),
    }
}

#[test]
fn prompt_is_stdin_data_not_command_arguments() {
    let (temp, path) = executable(
        "if [ \"$1\" = --version ]; then echo 'codex fixture'; exit 0; fi\nprintf '%s' \"$*\" > argv\nIFS= read -r prompt\nprintf '%s' \"$prompt\" > stdin\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{}}'",
    );
    let marker = temp.path().join("injected");
    let prompt = format!("do work; touch {}", marker.display());
    CodexAgent::new(path)
        .execute(request(temp.path(), &prompt), &mut Vec::new())
        .unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join("stdin")).unwrap(),
        prompt
    );
    assert!(!fs::read_to_string(temp.path().join("argv"))
        .unwrap()
        .contains("touch"));
    assert!(!marker.exists());
}

#[test]
fn corrupt_or_truncated_stream_cannot_fabricate_completion() {
    let (temp, path) = executable(
        "if [ \"$1\" = --version ]; then echo 'codex fixture'; exit 0; fi\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"completed approved delivered\"}}'\nprintf '%s\\n' '{broken-json'",
    );
    let error = CodexAgent::new(path)
        .execute(request(temp.path(), "work"), &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, AgentExecutionError::MalformedOutput { .. }));
    assert!(error.to_string().contains("EOF before turn.completed"));
}

#[test]
fn hostile_agent_output_is_redacted_from_captured_log() {
    const CANARY: &str = "burn-in-secret-canary-agent";
    let (temp, path) = executable(
        "if [ \"$1\" = --version ]; then echo 'codex fixture'; exit 0; fi\ncat > captured-prompt\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"burn-in-secret-canary-agent\"}}'\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{}}'",
    );
    std::env::set_var("SECURITY_BURN_IN_AGENT_CANARY", CANARY);
    let mut captured_log = Vec::new();
    CodexAgent::new(path)
        .execute(
            request(temp.path(), "perform the bounded fixture task"),
            &mut captured_log,
        )
        .unwrap();
    std::env::remove_var("SECURITY_BURN_IN_AGENT_CANARY");

    assert!(!fs::read_to_string(temp.path().join("captured-prompt"))
        .unwrap()
        .contains(CANARY));
    assert!(!String::from_utf8(captured_log).unwrap().contains(CANARY));
}

#[test]
fn malformed_agent_output_is_redacted_before_forwarding() {
    const CANARY: &str = "burn-in-secret-canary-malformed";
    let (temp, path) = executable(
        "if [ \"$1\" = --version ]; then echo 'codex fixture'; exit 0; fi\nprintf '%s\\n' '{broken-json burn-in-secret-canary-malformed'",
    );
    std::env::set_var("SECURITY_BURN_IN_MALFORMED_CANARY", CANARY);
    let mut captured_log = Vec::new();
    let error = CodexAgent::new(path)
        .execute(request(temp.path(), "bounded fixture"), &mut captured_log)
        .unwrap_err();
    std::env::remove_var("SECURITY_BURN_IN_MALFORMED_CANARY");

    assert!(matches!(error, AgentExecutionError::MalformedOutput { .. }));
    let captured = String::from_utf8(captured_log).unwrap();
    assert!(!captured.contains(CANARY));
    assert!(captured.contains("[REDACTED]"));
}
