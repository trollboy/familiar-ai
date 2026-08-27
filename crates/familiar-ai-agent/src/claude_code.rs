use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::isolation::{isolated_command, stream_lines, StreamAction};
use crate::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};

/// Adapter-owned tool restrictions guaranteeing that a `ReadOnly` execution
/// cannot edit files or run repository-modifying commands, independent of the
/// configured permission mode. Pinned by tests.
pub const READ_ONLY_RESTRICTIONS: [&str; 2] =
    ["--disallowedTools", "Bash,Edit,Write,NotebookEdit,WebFetch"];

const DEFAULT_PERMISSION_MODE: &str = "default";
const BYPASS_PERMISSION_MODE: &str = "bypassPermissions";

/// Plain-field construction settings; validation belongs to configuration.
#[derive(Debug, Clone, Default)]
pub struct ClaudeCodeSettings {
    pub executable: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub max_budget_microusd: Option<u64>,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeAgent {
    settings: ClaudeCodeSettings,
}

impl ClaudeCodeAgent {
    pub fn new(settings: ClaudeCodeSettings) -> Self {
        Self { settings }
    }

    fn probe_version(&self) -> Option<String> {
        let output = Command::new(&self.settings.executable)
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
        if line.is_empty() || !line.to_ascii_lowercase().contains("claude") {
            None
        } else {
            Some(line.to_owned())
        }
    }

    fn permission_mode(&self) -> &str {
        self.settings
            .permission_mode
            .as_deref()
            .unwrap_or(DEFAULT_PERMISSION_MODE)
    }

    fn argv(&self, request: &ExecutionRequest<'_>) -> Vec<String> {
        let mut argv: Vec<String> = ["--print", "--output-format", "stream-json", "--verbose"]
            .map(str::to_owned)
            .to_vec();
        if let Some(model) = request.model.or(self.settings.model.as_deref()) {
            argv.push("--model".into());
            argv.push(model.to_owned());
        }
        if let Some(limit) = request.budget.max_cost_microusd {
            argv.push("--max-budget-usd".into());
            argv.push(format!(
                "{}.{:06}",
                limit.get() / 1_000_000,
                limit.get() % 1_000_000
            ));
        }
        match request.filesystem {
            crate::FilesystemPolicy::ReadOnly => {
                let mode = if self.permission_mode() == BYPASS_PERMISSION_MODE {
                    DEFAULT_PERMISSION_MODE
                } else {
                    self.permission_mode()
                };
                argv.push("--permission-mode".into());
                argv.push(mode.to_owned());
                argv.extend(READ_ONLY_RESTRICTIONS.map(str::to_owned));
            }
            crate::FilesystemPolicy::Normal | crate::FilesystemPolicy::WorkspaceWrite => {
                argv.push("--permission-mode".into());
                argv.push(self.permission_mode().to_owned());
            }
        }
        if let Some(effort) = &self.settings.effort {
            argv.push("--effort".into());
            argv.push(effort.clone());
        }
        argv.extend(self.settings.extra_args.iter().cloned());
        argv
    }
}

impl CodingAgent for ClaudeCodeAgent {
    fn budget_capability(&self) -> crate::BudgetCapability {
        crate::BudgetCapability::CLAUDE_CODE
    }
    fn preflight(&self) -> Result<(), String> {
        self.probe_version().map(|_| ()).ok_or_else(|| {
            format!(
                "Claude executable {:?} is unavailable or invalid",
                self.settings.executable
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
        if let Some(denomination) = request
            .budget
            .denominations()
            .find(|value| !self.budget_capability().supports(*value))
        {
            return Err(AgentExecutionError::UnenforceableBudget {
                adapter: "claude-code",
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
        let mut command = isolated_command(&self.settings.executable, request.denied_read_path)?;
        command.args(self.argv(&request));
        #[cfg(unix)]
        if request.timeout_ms.is_some() {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .current_dir(request.working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| AgentExecutionError::Launch {
                executable: self.settings.executable.clone(),
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
        let mut stream = ClaudeStream::default();
        let output_result =
            stream_lines(child.stdout.take().expect("piped stdout"), output, |line| {
                parse_event(line, &mut stream)
            });
        let status = child.wait().map_err(|source| AgentExecutionError::Wait {
            source: Box::new(source),
            result: Box::new(result.clone()),
        })?;
        #[cfg(unix)]
        let timed_out = crate::isolation::finish_watchdog(watchdog);
        stream.apply(&mut result);
        result.exit_code = status.code();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            result.signal = status.signal();
        }
        if stream.budget_stopped {
            return Err(AgentExecutionError::BudgetStopped {
                result: Box::new(result),
            });
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
        if !status.success() {
            return Ok(result);
        }
        if !stream.terminal_seen {
            return Err(AgentExecutionError::MalformedOutput {
                detail: "EOF before result event".into(),
                result: Box::new(result),
            });
        }
        if stream.malformed_seen {
            return Err(AgentExecutionError::MalformedOutput {
                detail: "stream contained malformed JSON or duplicate terminal events".into(),
                result: Box::new(result),
            });
        }
        match input {
            Ok(()) => {}
            Err(_) if !status.success() => {}
            Err(source) => {
                return Err(AgentExecutionError::Input {
                    source: Box::new(source),
                    result: Box::new(result),
                })
            }
        }
        if let (Some(limit), Some(reported)) = (
            self.settings.max_budget_microusd.filter(|limit| *limit > 0),
            result.reported_cost_microusd,
        ) {
            if reported > limit {
                return Err(AgentExecutionError::BudgetExceeded {
                    limit_microusd: limit,
                    reported_microusd: reported,
                    result: Box::new(result),
                });
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Default)]
struct ClaudeStream {
    init_model: Option<String>,
    result_model: Option<String>,
    init_session: Option<String>,
    result_session: Option<String>,
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_cost_usd: Option<f64>,
    terminal_seen: bool,
    malformed_seen: bool,
    budget_stopped: bool,
}

impl ClaudeStream {
    fn apply(&self, result: &mut ExecutionResult) {
        result.model = self
            .init_model
            .clone()
            .or_else(|| self.result_model.clone());
        result.session_id = self
            .init_session
            .clone()
            .or_else(|| self.result_session.clone());
        result.input_tokens = match (
            self.input_tokens,
            self.cache_creation_input_tokens,
            self.cache_read_input_tokens,
        ) {
            (Some(direct), Some(created), Some(read)) => direct
                .checked_add(created)
                .and_then(|sum| sum.checked_add(read)),
            _ => None,
        };
        result.cached_tokens = self.cache_read_input_tokens;
        result.output_tokens = self.output_tokens;
        result.reported_cost_microusd = self.total_cost_usd.and_then(cost_to_microusd);
    }
}

/// Convert dollars to micro-USD with half-up rounding and checked range.
fn cost_to_microusd(cost_usd: f64) -> Option<u64> {
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return None;
    }
    let micro = (cost_usd * 1_000_000.0).round();
    if !micro.is_finite() || micro < 0.0 || micro > i64::MAX as f64 {
        return None;
    }
    Some(micro as u64)
}

fn uint(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value <= i64::MAX as u64)
}

fn string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn parse_event(line: &str, stream: &mut ClaudeStream) -> StreamAction {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        stream.malformed_seen = true;
        return StreamAction::Forward;
    };
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return StreamAction::Silent;
    };
    match event_type {
        "system" => {
            if value.get("subtype").and_then(Value::as_str) == Some("init") {
                if stream.init_model.is_none() {
                    stream.init_model = string(value.get("model"));
                }
                if stream.init_session.is_none() {
                    stream.init_session = string(value.get("session_id"));
                }
            }
            StreamAction::Silent
        }
        "assistant" => {
            let texts: Vec<&str> = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|block| block.get("text").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            if texts.is_empty() {
                StreamAction::Silent
            } else {
                StreamAction::Text(texts.join("\n"))
            }
        }
        "user" => StreamAction::Silent,
        "result" => {
            if stream.terminal_seen {
                // Malformed stream: keep the first terminal capture and
                // forward subsequent anomalies as unclassified lines.
                stream.malformed_seen = true;
                return StreamAction::Forward;
            }
            stream.terminal_seen = true;
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                stream.input_tokens = uint(usage.get("input_tokens"));
                stream.cache_creation_input_tokens = uint(usage.get("cache_creation_input_tokens"));
                stream.cache_read_input_tokens = uint(usage.get("cache_read_input_tokens"));
                stream.output_tokens = uint(usage.get("output_tokens"));
            }
            let top_level_credible = [
                stream.input_tokens,
                stream.cache_creation_input_tokens,
                stream.cache_read_input_tokens,
                stream.output_tokens,
            ]
            .into_iter()
            .all(|value| value.is_some())
                && [
                    stream.input_tokens,
                    stream.cache_creation_input_tokens,
                    stream.cache_read_input_tokens,
                    stream.output_tokens,
                ]
                .into_iter()
                .flatten()
                .any(|value| value > 0);
            if !top_level_credible {
                if let Some(aggregate) = aggregate_model_usage(&value) {
                    stream.input_tokens = Some(aggregate.0);
                    stream.cache_creation_input_tokens = Some(aggregate.1);
                    stream.cache_read_input_tokens = Some(aggregate.2);
                    stream.output_tokens = Some(aggregate.3);
                } else {
                    stream.input_tokens = None;
                    stream.cache_creation_input_tokens = None;
                    stream.cache_read_input_tokens = None;
                    stream.output_tokens = None;
                }
            }
            stream.total_cost_usd = value
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .filter(|cost| cost.is_finite() && *cost >= 0.0);
            stream.result_session = string(value.get("session_id"));
            if stream.result_model.is_none() {
                stream.result_model = single_result_model(&value);
            }
            let subtype = value.get("subtype").and_then(Value::as_str);
            stream.budget_stopped = subtype == Some("error_max_budget_usd");
            let is_error = value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_error || subtype.is_some_and(|subtype| subtype != "success") {
                let detail = subtype
                    .filter(|subtype| *subtype != "success")
                    .map(str::to_owned)
                    .or_else(|| string(value.get("result")))
                    .unwrap_or_else(|| "unknown error".into());
                StreamAction::Text(format!("error: {detail}"))
            } else {
                StreamAction::Silent
            }
        }
        _ => StreamAction::Silent,
    }
}

fn aggregate_model_usage(value: &Value) -> Option<(u64, u64, u64, u64)> {
    let models = value.get("modelUsage")?.as_object()?;
    if models.is_empty() {
        return None;
    }
    let mut totals = (0_u64, 0_u64, 0_u64, 0_u64);
    for usage in models.values().map(Value::as_object) {
        let usage = usage?;
        let values = (
            uint(
                usage
                    .get("inputTokens")
                    .or_else(|| usage.get("input_tokens")),
            )?,
            uint(
                usage
                    .get("cacheCreationInputTokens")
                    .or_else(|| usage.get("cache_creation_input_tokens")),
            )?,
            uint(
                usage
                    .get("cacheReadInputTokens")
                    .or_else(|| usage.get("cache_read_input_tokens")),
            )?,
            uint(
                usage
                    .get("outputTokens")
                    .or_else(|| usage.get("output_tokens")),
            )?,
        );
        totals.0 = totals.0.checked_add(values.0)?;
        totals.1 = totals.1.checked_add(values.1)?;
        totals.2 = totals.2.checked_add(values.2)?;
        totals.3 = totals.3.checked_add(values.3)?;
    }
    (totals != (0, 0, 0, 0)).then_some(totals)
}

/// A result event names a model only when `modelUsage` contains exactly one.
fn single_result_model(value: &Value) -> Option<String> {
    let usage = value.get("modelUsage").and_then(Value::as_object)?;
    if usage.len() == 1 {
        usage.keys().next().cloned()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn agent(settings: ClaudeCodeSettings) -> ClaudeCodeAgent {
        ClaudeCodeAgent::new(settings)
    }
    fn settings(executable: &Path) -> ClaudeCodeSettings {
        ClaudeCodeSettings {
            executable: executable.to_string_lossy().into_owned(),
            ..ClaudeCodeSettings::default()
        }
    }
    #[cfg(unix)]
    fn write_executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
    fn request<'a>(
        working_directory: &'a Path,
        model: Option<&'a str>,
        filesystem: crate::FilesystemPolicy,
    ) -> ExecutionRequest<'a> {
        ExecutionRequest {
            working_directory,
            denied_read_path: None,
            prompt: "prompt bytes",
            prompt_cache_key: None,
            filesystem,
            model,
            timeout_ms: None,
            budget: crate::ExecutionBudget::default(),
        }
    }

    #[test]
    fn trait_is_object_safe_and_reports_fresh_process() {
        fn accepts(agent: &dyn CodingAgent) -> crate::IsolationCapability {
            agent.isolation_capability()
        }
        assert_eq!(
            accepts(&agent(ClaudeCodeSettings {
                executable: "claude".into(),
                ..ClaudeCodeSettings::default()
            })),
            crate::IsolationCapability::FreshProcessPerExecution
        );
    }

    #[test]
    fn parses_init_assistant_and_result_events() {
        let mut stream = ClaudeStream::default();
        assert!(matches!(
            parse_event(
                r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5","session_id":"s-1"}"#,
                &mut stream
            ),
            StreamAction::Silent
        ));
        match parse_event(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"},{"type":"tool_use","name":"Bash"},{"type":"text","text":"still"}]}}"#,
            &mut stream,
        ) {
            StreamAction::Text(text) => assert_eq!(text, "working\nstill"),
            _ => panic!("assistant text should stream"),
        }
        assert!(matches!(
            parse_event(r#"{"type":"user","message":{}}"#, &mut stream),
            StreamAction::Silent
        ));
        assert!(matches!(
            parse_event(
                r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s-1","total_cost_usd":0.25,"usage":{"input_tokens":10,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":4}}"#,
                &mut stream
            ),
            StreamAction::Silent
        ));
        let mut result = ExecutionResult::default();
        stream.apply(&mut result);
        assert_eq!(result.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(result.session_id.as_deref(), Some("s-1"));
        assert_eq!(result.input_tokens, Some(15));
        assert_eq!(result.cached_tokens, Some(3));
        assert_eq!(result.output_tokens, Some(4));
        assert_eq!(result.reported_cost_microusd, Some(250_000));
    }

    #[test]
    fn error_results_emit_error_line_and_unknown_types_stay_silent() {
        let mut stream = ClaudeStream::default();
        match parse_event(
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"ran out"}"#,
            &mut stream,
        ) {
            StreamAction::Text(text) => assert_eq!(text, "error: error_max_turns"),
            _ => panic!("error result must emit"),
        }
        assert!(matches!(
            parse_event(r#"{"type":"future_event"}"#, &mut stream),
            StreamAction::Silent
        ));
        assert!(matches!(
            parse_event("not json at all", &mut stream),
            StreamAction::Forward
        ));
    }

    #[test]
    fn duplicate_results_keep_first_capture_and_forward_later_ones() {
        let mut stream = ClaudeStream::default();
        parse_event(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1},"total_cost_usd":0.1}"#,
            &mut stream,
        );
        assert!(matches!(
            parse_event(
                r#"{"type":"result","subtype":"success","usage":{"input_tokens":99,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":99}}"#,
                &mut stream
            ),
            StreamAction::Forward
        ));
        let mut result = ExecutionResult::default();
        stream.apply(&mut result);
        assert_eq!(result.input_tokens, Some(1));
        assert_eq!(result.reported_cost_microusd, Some(100_000));
    }

    #[test]
    fn malformed_usage_and_cost_stay_unknown_never_zero() {
        let mut stream = ClaudeStream::default();
        parse_event(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":-1,"cache_creation_input_tokens":null,"output_tokens":"four"},"total_cost_usd":-0.5,"session_id":7}"#,
            &mut stream,
        );
        let mut result = ExecutionResult::default();
        stream.apply(&mut result);
        assert_eq!(result.input_tokens, None);
        assert_eq!(result.cached_tokens, None);
        assert_eq!(result.output_tokens, None);
        assert_eq!(result.reported_cost_microusd, None);
        assert_eq!(result.session_id, None);
        // Partial usage: missing cache fields leave the total unknown.
        let mut stream = ClaudeStream::default();
        parse_event(
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":10,"output_tokens":4}}"#,
            &mut stream,
        );
        let mut result = ExecutionResult::default();
        stream.apply(&mut result);
        assert_eq!(result.input_tokens, None);
        assert_eq!(result.output_tokens, None);
    }

    #[test]
    fn zeroed_top_level_usage_falls_back_to_aggregated_model_usage() {
        let mut stream = ClaudeStream::default();
        parse_event(
            r#"{"type":"result","subtype":"error_max_budget_usd","usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0},"modelUsage":{"a":{"inputTokens":10,"cacheCreationInputTokens":2,"cacheReadInputTokens":3,"outputTokens":4},"b":{"inputTokens":20,"cacheCreationInputTokens":1,"cacheReadInputTokens":5,"outputTokens":6}}}"#,
            &mut stream,
        );
        let mut result = ExecutionResult::default();
        stream.apply(&mut result);
        assert!(stream.budget_stopped);
        assert_eq!(result.input_tokens, Some(41));
        assert_eq!(result.cached_tokens, Some(8));
        assert_eq!(result.output_tokens, Some(10));
    }

    #[test]
    fn cost_budget_is_pinned_in_argv_as_exact_fixed_point_dollars() {
        let mut request = request(Path::new("."), None, crate::FilesystemPolicy::Normal);
        request.budget.max_cost_microusd = std::num::NonZeroU64::new(8_000_001);
        let argv = agent(settings(Path::new("claude"))).argv(&request);
        let position = argv
            .iter()
            .position(|arg| arg == "--max-budget-usd")
            .unwrap();
        assert_eq!(
            &argv[position..=position + 1],
            ["--max-budget-usd", "8.000001"]
        );
        assert_eq!(
            argv.iter().filter(|arg| *arg == "--max-budget-usd").count(),
            1
        );
    }

    #[test]
    fn cost_conversion_rounds_half_up_with_checked_range() {
        assert_eq!(cost_to_microusd(0.0000005), Some(1));
        assert_eq!(cost_to_microusd(0.0000004), Some(0));
        assert_eq!(cost_to_microusd(1.25), Some(1_250_000));
        assert_eq!(cost_to_microusd(f64::NAN), None);
        assert_eq!(cost_to_microusd(f64::INFINITY), None);
        assert_eq!(cost_to_microusd(-0.01), None);
        assert_eq!(cost_to_microusd(1e300), None);
    }

    #[test]
    fn argv_is_deterministic_and_never_contains_forbidden_flags() {
        let temp = tempfile::tempdir().unwrap();
        let agent = agent(ClaudeCodeSettings {
            executable: "claude".into(),
            model: Some("configured-model".into()),
            effort: Some("high".into()),
            permission_mode: Some("acceptEdits".into()),
            max_budget_microusd: None,
            extra_args: vec!["--add-dir".into(), "/tmp/extra".into()],
        });
        let argv = agent.argv(&request(
            temp.path(),
            Some("request-model"),
            crate::FilesystemPolicy::WorkspaceWrite,
        ));
        assert_eq!(
            argv,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "request-model",
                "--permission-mode",
                "acceptEdits",
                "--effort",
                "high",
                "--add-dir",
                "/tmp/extra",
            ]
        );
        for forbidden in [
            "--resume",
            "--continue",
            "--session-id",
            "--fork-session",
            "--dangerously-skip-permissions",
        ] {
            assert!(!argv.iter().any(|arg| arg == forbidden));
        }
        // Configured model applies when the request has none.
        let argv = agent.argv(&request(temp.path(), None, crate::FilesystemPolicy::Normal));
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--model", "configured-model"]));
    }

    #[test]
    fn read_only_policy_pins_restrictions_regardless_of_configured_mode() {
        let temp = tempfile::tempdir().unwrap();
        for mode in ["default", "plan", "acceptEdits", "bypassPermissions"] {
            let agent = agent(ClaudeCodeSettings {
                executable: "claude".into(),
                permission_mode: Some(mode.into()),
                ..ClaudeCodeSettings::default()
            });
            let argv = agent.argv(&request(
                temp.path(),
                None,
                crate::FilesystemPolicy::ReadOnly,
            ));
            assert!(
                argv.windows(2).any(|pair| pair == READ_ONLY_RESTRICTIONS),
                "restrictions missing for mode {mode}: {argv:?}"
            );
            let emitted_mode = argv
                .windows(2)
                .find(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1].clone())
                .unwrap();
            assert_ne!(emitted_mode, "bypassPermissions");
        }
    }

    #[cfg(unix)]
    #[test]
    fn executes_fake_claude_streaming_output_and_mapping_results() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("claude");
        let capture = temp.path().join("stdin.txt");
        let argv_capture = temp.path().join("argv.txt");
        write_executable(
            &executable,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Claude Code 2.1'; exit 0; fi\nprintf '%s\\n' \"$@\" > {argv}\ncat > {stdin}\nprintf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-sonnet-4-5\",\"session_id\":\"sess-9\"}}'\nprintf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"progress line\"}}]}}}}'\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"sess-9\",\"total_cost_usd\":0.031415,\"usage\":{{\"input_tokens\":100,\"cache_creation_input_tokens\":20,\"cache_read_input_tokens\":30,\"output_tokens\":40}}}}'\nexit 0\n",
                argv = argv_capture.display(),
                stdin = capture.display(),
            ),
        );
        let mut output = Vec::new();
        let result = agent(settings(&executable))
            .execute(
                request(temp.path(), None, crate::FilesystemPolicy::WorkspaceWrite),
                &mut output,
            )
            .unwrap();
        assert_eq!(output, b"progress line\n");
        assert_eq!(fs::read_to_string(&capture).unwrap(), "prompt bytes");
        let argv = fs::read_to_string(&argv_capture).unwrap();
        assert_eq!(
            argv,
            "--print\n--output-format\nstream-json\n--verbose\n--permission-mode\ndefault\n"
        );
        assert!(!argv.contains("prompt bytes"));
        assert_eq!(result.agent_version.as_deref(), Some("Claude Code 2.1"));
        assert_eq!(result.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(result.session_id.as_deref(), Some("sess-9"));
        assert_eq!(result.input_tokens, Some(150));
        assert_eq!(result.cached_tokens, Some(30));
        assert_eq!(result.output_tokens, Some(40));
        assert_eq!(result.reported_cost_microusd, Some(31_415));
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn missing_result_event_is_malformed_terminal_output() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("claude");
        write_executable(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'claude 2'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"only text\"}]}}'\nexit 0\n",
        );
        let mut output = Vec::new();
        let result = agent(settings(&executable))
            .execute(
                request(temp.path(), None, crate::FilesystemPolicy::Normal),
                &mut output,
            )
            .unwrap_err();
        assert_eq!(output, b"only text\n");
        assert!(matches!(
            result,
            AgentExecutionError::MalformedOutput { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_requires_claude_identifier() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("claude");
        write_executable(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'other-tool 3.0'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\"}'\nexit 0\n",
        );
        let result = agent(settings(&executable))
            .execute(
                request(temp.path(), None, crate::FilesystemPolicy::Normal),
                &mut Vec::new(),
            )
            .unwrap();
        assert_eq!(result.agent_version, None);
    }

    #[cfg(unix)]
    #[test]
    fn budget_exceedance_is_typed_and_unknown_cost_never_violates() {
        let temp = tempfile::tempdir().unwrap();
        let script_with_cost = |cost: &str| {
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'claude 2'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\"{cost}}}'\nexit 0\n",
            )
        };
        for (cost_fragment, limit, expect_err) in [
            (",\"total_cost_usd\":2.0", 1_000_000, true),
            (",\"total_cost_usd\":1.0", 1_000_000, false),
            (",\"total_cost_usd\":0.5", 1_000_000, false),
            ("", 1_000_000, false),
        ] {
            let executable = temp.path().join("claude");
            write_executable(&executable, &script_with_cost(cost_fragment));
            let mut settings = settings(&executable);
            settings.max_budget_microusd = Some(limit);
            let outcome = agent(settings).execute(
                request(temp.path(), None, crate::FilesystemPolicy::Normal),
                &mut Vec::new(),
            );
            if expect_err {
                match outcome {
                    Err(AgentExecutionError::BudgetExceeded {
                        limit_microusd,
                        reported_microusd,
                        result,
                    }) => {
                        assert_eq!(limit_microusd, 1_000_000);
                        assert_eq!(reported_microusd, 2_000_000);
                        assert_eq!(result.reported_cost_microusd, Some(2_000_000));
                    }
                    other => panic!("expected budget exceedance, got {other:?}"),
                }
            } else {
                assert!(outcome.is_ok(), "cost {cost_fragment:?} should pass");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_execution_kills_a_timed_out_process() {
        use std::time::Instant;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("claude");
        write_executable(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo claude-test; exit 0; fi\ncat >/dev/null\nsleep 10\n",
        );
        let started = Instant::now();
        let mut request = request(temp.path(), None, crate::FilesystemPolicy::ReadOnly);
        request.timeout_ms = Some(50);
        let result = agent(settings(&executable)).execute(request, &mut Vec::new());
        assert!(matches!(result, Err(AgentExecutionError::Timeout { .. })));
        assert!(started.elapsed().as_secs() < 2);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn denied_read_path_fails_closed_without_platform_isolation() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = request(temp.path(), None, crate::FilesystemPolicy::ReadOnly);
        request.denied_read_path = Some(temp.path());
        let result = agent(ClaudeCodeSettings {
            executable: "claude".into(),
            ..ClaudeCodeSettings::default()
        })
        .execute(request, &mut Vec::new());
        assert!(matches!(result, Err(AgentExecutionError::Launch { .. })));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_isolation_denies_repository_reads_or_fails_closed() {
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
        let executable = temp.path().join("claude-test");
        write_executable(
            &executable,
            &format!(
                "#!/bin/sh\nif cat '{}' >/dev/null 2>&1; then inner=READ; else inner=DENIED; fi\nif cat '{}' >/dev/null 2>&1; then outer=OK; else outer=BLOCKED; fi\nif ls '{}' >/dev/null 2>&1; then anc=LISTED; else anc=HIDDEN; fi\nprintf '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"%s/%s/%s\"}}]}}}}\\n' \"$inner\" \"$outer\" \"$anc\"\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\"}}'\n",
                secret.display(),
                outside.display(),
                repository.display()
            ),
        );
        let mut output = Vec::new();
        let mut request = request(&workspace, None, crate::FilesystemPolicy::ReadOnly);
        request.denied_read_path = Some(&repository);
        request.timeout_ms = Some(5_000);
        let result = agent(settings(&executable)).execute(request, &mut output);
        if crate::isolation::linux_sandbox_available() {
            let result = result.unwrap();
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
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let workspace = temp.path().join("review-workspace");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let secret = repository.join("unrelated.txt");
        fs::write(&secret, "private repository content").unwrap();
        let executable = temp.path().join("claude-test");
        write_executable(
            &executable,
            &format!(
                "#!/bin/sh\nif cat '{}' >/dev/null 2>&1; then text=READ; else text=DENIED; fi\nprintf '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"%s\"}}]}}}}\\n' \"$text\"\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\"}}'\n",
                secret.display()
            ),
        );
        let mut output = Vec::new();
        let mut request = request(&workspace, None, crate::FilesystemPolicy::ReadOnly);
        request.denied_read_path = Some(&repository);
        request.timeout_ms = Some(5_000);
        let result = agent(settings(&executable))
            .execute(request, &mut output)
            .unwrap();
        assert_ne!(output, b"READ\n");
        assert!(output == b"DENIED\n" || result.exit_code != Some(0));
    }
}
