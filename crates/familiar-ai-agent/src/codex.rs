use std::io::{self, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::isolation::{isolated_command, stream_lines, StreamAction};
use crate::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};

#[derive(Debug, Clone)]
pub struct CodexAgent {
    executable: String,
}

impl CodexAgent {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    fn probe_version(&self) -> Option<String> {
        let output = Command::new(&self.executable)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let line = text.lines().next()?.trim();
        if line.is_empty() || !line.to_ascii_lowercase().contains("codex") {
            None
        } else {
            Some(line.to_owned())
        }
    }
}

impl CodingAgent for CodexAgent {
    fn preflight(&self) -> Result<(), String> {
        self.probe_version().map(|_| ()).ok_or_else(|| {
            format!(
                "Codex executable {:?} is unavailable or invalid",
                self.executable
            )
        })
    }
    fn isolation_capability(&self) -> crate::IsolationCapability {
        crate::IsolationCapability::FreshProcessPerExecution
    }

    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        if let Some(denomination) = request.budget.denominations().next() {
            return Err(AgentExecutionError::UnenforceableBudget {
                adapter: "codex",
                denomination,
                result: Box::default(),
            });
        }
        let mut result = ExecutionResult {
            // Probing executes the adapter binary. Never do that outside the
            // isolated filesystem boundary used for review.
            agent_version: request
                .denied_read_path
                .is_none()
                .then(|| self.probe_version())
                .flatten(),
            ..ExecutionResult::default()
        };
        let mut command = isolated_command(&self.executable, request.denied_read_path)?;
        command.arg("exec");
        match request.filesystem {
            crate::FilesystemPolicy::ReadOnly => {
                command.args(["--sandbox", "read-only"]);
            }
            crate::FilesystemPolicy::WorkspaceWrite => {
                command.args(["--sandbox", "workspace-write"]);
            }
            crate::FilesystemPolicy::Normal => {}
        }
        if let Some(model) = request.model {
            if let Some(local_model) = model.strip_prefix("ollama/") {
                command.args([
                    "--oss",
                    "--local-provider",
                    "ollama",
                    "--model",
                    local_model,
                ]);
            } else {
                command.args(["--model", model]);
            }
        }
        #[cfg(unix)]
        if request.timeout_ms.is_some() {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .args(["--json", "-"])
            .current_dir(request.working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| AgentExecutionError::Launch {
                executable: self.executable.clone(),
                source: Box::new(source),
                result: Box::new(result.clone()),
            })?;
        let input = child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(request.prompt.as_bytes());
        #[cfg(unix)]
        let watchdog = crate::isolation::spawn_watchdog(child.id(), request.timeout_ms);
        let mut terminal_seen = false;
        let mut malformed_seen = false;
        let output_result = stream_json(
            child.stdout.take().expect("piped stdout"),
            output,
            &mut result,
            &mut terminal_seen,
            &mut malformed_seen,
        );
        let status = child.wait().map_err(|source| AgentExecutionError::Wait {
            source: Box::new(source),
            result: Box::new(result.clone()),
        })?;
        #[cfg(unix)]
        let timed_out = crate::isolation::finish_watchdog(watchdog);
        result.exit_code = status.code();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            result.signal = status.signal();
        }
        if let Err(source) = output_result {
            return Err(AgentExecutionError::Output {
                source: Box::new(source),
                result: Box::new(result),
            });
        }
        #[cfg(unix)]
        if timed_out {
            return Err(AgentExecutionError::Timeout {
                result: Box::new(result),
            });
        }
        // A crashing adapter has an OS-authenticated terminal result even if
        // its structured stream ended abruptly. Preserve exit/signal rather
        // than misclassifying the crash as malformed JSON.
        if !status.success() {
            return Ok(result);
        }
        if !terminal_seen {
            return Err(AgentExecutionError::MalformedOutput {
                detail: "EOF before turn.completed".into(),
                result: Box::new(result),
            });
        }
        if malformed_seen {
            return Err(AgentExecutionError::MalformedOutput {
                detail: "stream contained malformed JSON".into(),
                result: Box::new(result),
            });
        }
        match input {
            Ok(()) => Ok(result),
            Err(_) if !status.success() => Ok(result),
            Err(source) => Err(AgentExecutionError::Input {
                source: Box::new(source),
                result: Box::new(result),
            }),
        }
    }
}

fn parse_event(
    line: &str,
    result: &mut ExecutionResult,
    terminal_seen: &mut bool,
) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "turn.started" => None,
        "turn.completed" => {
            *terminal_seen = true;
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                result.input_tokens = uint(usage.get("input_tokens"));
                result.output_tokens = uint(usage.get("output_tokens"));
                result.cached_tokens = uint(usage.get("cached_input_tokens"));
            }
            None
        }
        "item.completed" => {
            let item = value.get("item")?;
            match item.get("type").and_then(Value::as_str)? {
                "agent_message" | "reasoning" => {
                    item.get("text").and_then(Value::as_str).map(str::to_owned)
                }
                _ => None,
            }
        }
        "error" => value
            .get("message")
            .and_then(Value::as_str)
            .map(|message| format!("error: {message}")),
        _ => None,
    }
}

fn uint(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value <= i64::MAX as u64)
}

fn stream_json(
    reader: impl io::Read,
    output: &mut dyn Write,
    result: &mut ExecutionResult,
    terminal_seen: &mut bool,
    malformed_seen: &mut bool,
) -> io::Result<()> {
    stream_lines(reader, output, |line| {
        match parse_event(line, result, terminal_seen) {
            Some(text) => StreamAction::Text(text),
            None if serde_json::from_str::<Value>(line).is_err() => {
                *malformed_seen = true;
                StreamAction::Forward
            }
            None => StreamAction::Silent,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_is_object_safe() {
        fn accepts(_: &dyn CodingAgent) {}
        accepts(&CodexAgent::new("codex"));
    }

    #[test]
    fn parses_only_supported_typed_events() {
        let mut result = ExecutionResult::default();
        let mut terminal_seen = false;
        assert_eq!(
            parse_event(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}"#,
                &mut result,
                &mut terminal_seen
            ),
            Some("hello".into())
        );
        parse_event(
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":3,"output_tokens":4}}"#,
            &mut result,
            &mut terminal_seen,
        );
        assert_eq!(result.input_tokens, Some(10));
        assert_eq!(result.cached_tokens, Some(3));
        assert_eq!(result.output_tokens, Some(4));
        assert_eq!(result.model, None);
        assert_eq!(
            parse_event(
                r#"{"type":"future.event"}"#,
                &mut result,
                &mut terminal_seen
            ),
            None
        );
    }

    #[test]
    fn malformed_lines_are_forwarded_without_fabricating_metrics() {
        let mut result = ExecutionResult::default();
        let mut terminal_seen = false;
        let mut malformed_seen = false;
        let mut output = Vec::new();
        stream_json(
            b"not json\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":-1}}\n".as_slice(),
            &mut output,
            &mut result,
            &mut terminal_seen,
            &mut malformed_seen,
        )
        .unwrap();
        assert_eq!(output, b"not json\n");
        assert_eq!(result.input_tokens, None);
        assert!(malformed_seen);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_execution_kills_a_timed_out_process() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("codex");
        fs::write(&executable,"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-test; exit 0; fi\ncat >/dev/null\nsleep 10\n").unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let started = Instant::now();
        let result = CodexAgent::new(executable.to_string_lossy()).execute(
            ExecutionRequest {
                working_directory: temp.path(),
                denied_read_path: None,
                prompt: "review",
                filesystem: crate::FilesystemPolicy::ReadOnly,
                model: None,
                timeout_ms: Some(50),
                budget: crate::ExecutionBudget::default(),
            },
            &mut Vec::new(),
        );
        assert!(matches!(result, Err(AgentExecutionError::Timeout { .. })));
        assert!(started.elapsed().as_secs() < 2);
    }

    #[cfg(unix)]
    #[test]
    fn crash_signal_is_preserved_as_terminal_process_evidence() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("codex");
        fs::write(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-test; exit 0; fi\ncat >/dev/null\nkill -TERM $$\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let result = CodexAgent::new(executable.to_string_lossy())
            .execute(
                ExecutionRequest {
                    working_directory: temp.path(),
                    denied_read_path: None,
                    prompt: "work",
                    filesystem: crate::FilesystemPolicy::Normal,
                    model: None,
                    timeout_ms: Some(5_000),
                    budget: crate::ExecutionBudget::default(),
                },
                &mut Vec::new(),
            )
            .unwrap();
        assert_eq!(result.signal, Some(libc::SIGTERM));
        assert_eq!(result.exit_code, None);
    }

    #[cfg(unix)]
    #[test]
    fn partial_and_malformed_streams_have_distinct_durable_errors() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("codex");
        let write = |script: &str| {
            fs::write(&executable, script).unwrap();
            let mut permissions = fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&executable, permissions).unwrap();
        };
        let request = || ExecutionRequest {
            working_directory: temp.path(),
            denied_read_path: None,
            prompt: "work",
            filesystem: crate::FilesystemPolicy::Normal,
            model: None,
            timeout_ms: Some(5_000),
            budget: crate::ExecutionBudget::default(),
        };

        write("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial\"}}'\n");
        let mut output = Vec::new();
        let partial = CodexAgent::new(executable.to_string_lossy())
            .execute(request(), &mut output)
            .unwrap_err();
        assert!(matches!(
            partial,
            AgentExecutionError::MalformedOutput { .. }
        ));
        assert_eq!(output, b"partial\n");

        write("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'not-json' '{\"type\":\"turn.completed\"}'\n");
        let malformed = CodexAgent::new(executable.to_string_lossy())
            .execute(request(), &mut Vec::new())
            .unwrap_err();
        assert!(matches!(
            malformed,
            AgentExecutionError::MalformedOutput { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_isolation_denies_repository_reads_or_fails_closed() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::isolation::ISOLATION_TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let workspace = temp.path().join("review-workspace");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let secret = repository.join("unrelated.txt");
        fs::write(&secret, "private repository content").unwrap();
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "readable").unwrap();
        let executable = temp.path().join("codex-test");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nif cat '{}' >/dev/null 2>&1; then inner=READ; else inner=DENIED; fi\nif cat '{}' >/dev/null 2>&1; then outer=OK; else outer=BLOCKED; fi\nif ls '{}' >/dev/null 2>&1; then anc=LISTED; else anc=HIDDEN; fi\nprintf '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"%s/%s/%s\"}}}}\\n' \"$inner\" \"$outer\" \"$anc\"\nprintf '%s\\n' '{{\"type\":\"turn.completed\"}}'\n",
                secret.display(),
                outside.display(),
                repository.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();

        let mut output = Vec::new();
        let result = CodexAgent::new(executable.to_string_lossy()).execute(
            ExecutionRequest {
                working_directory: &workspace,
                denied_read_path: Some(&repository),
                prompt: "review",
                filesystem: crate::FilesystemPolicy::ReadOnly,
                model: None,
                timeout_ms: Some(5_000),
            },
            &mut output,
        );
        if crate::isolation::linux_sandbox_available() {
            let result = result.unwrap();
            // Denied tree unreadable and unlistable; unrelated paths readable.
            assert_eq!(output, b"DENIED/OK/HIDDEN\n", "exit={:?}", result.exit_code);
        } else {
            assert!(
                matches!(result, Err(AgentExecutionError::Launch { .. })),
                "environment cannot sandbox: launch must fail closed"
            );
            assert!(output.is_empty());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn isolated_execution_cannot_read_denied_repository() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let workspace = temp.path().join("review-workspace");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let secret = repository.join("unrelated.txt");
        fs::write(&secret, "private repository content").unwrap();
        let executable = temp.path().join("codex-test");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nif cat '{}' >/dev/null 2>&1; then text=READ; else text=DENIED; fi\nprintf '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"%s\"}}}}\\n' \"$text\"\nprintf '%s\\n' '{{\"type\":\"turn.completed\"}}'\n",
                secret.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();

        let mut output = Vec::new();
        let result = CodexAgent::new(executable.to_string_lossy())
            .execute(
                ExecutionRequest {
                    working_directory: &workspace,
                    denied_read_path: Some(&repository),
                    prompt: "review",
                    filesystem: crate::FilesystemPolicy::ReadOnly,
                    model: None,
                    timeout_ms: Some(5_000),
                    budget: crate::ExecutionBudget::default(),
                },
                &mut output,
            )
            .unwrap();
        assert_ne!(output, b"READ\n");
        assert!(output == b"DENIED\n" || result.exit_code != Some(0));
    }
}
