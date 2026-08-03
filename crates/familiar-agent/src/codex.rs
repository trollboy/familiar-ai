use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

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
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let mut result = ExecutionResult {
            agent_version: self.probe_version(),
            ..ExecutionResult::default()
        };
        let mut child = Command::new(&self.executable)
            .args(["exec", "--json", "-"])
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
        let output_result = stream_json(
            child.stdout.take().expect("piped stdout"),
            output,
            &mut result,
        );
        let status = child.wait().map_err(|source| AgentExecutionError::Wait {
            source: Box::new(source),
            result: Box::new(result.clone()),
        })?;
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

fn parse_event(line: &str, result: &mut ExecutionResult) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "turn.started" => None,
        "turn.completed" => {
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
) -> io::Result<()> {
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if let Some(text) = parse_event(&line, result) {
            writeln!(output, "{text}")?;
            output.flush()?;
        } else if serde_json::from_str::<Value>(&line).is_err() {
            writeln!(output, "{line}")?;
            output.flush()?;
        }
    }
    Ok(())
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
        assert_eq!(
            parse_event(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}"#,
                &mut result
            ),
            Some("hello".into())
        );
        parse_event(
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":3,"output_tokens":4}}"#,
            &mut result,
        );
        assert_eq!(result.input_tokens, Some(10));
        assert_eq!(result.cached_tokens, Some(3));
        assert_eq!(result.output_tokens, Some(4));
        assert_eq!(result.model, None);
        assert_eq!(parse_event(r#"{"type":"future.event"}"#, &mut result), None);
    }

    #[test]
    fn malformed_lines_are_forwarded_without_fabricating_metrics() {
        let mut result = ExecutionResult::default();
        let mut output = Vec::new();
        stream_json(
            b"not json\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":-1}}\n".as_slice(),
            &mut output,
            &mut result,
        )
        .unwrap();
        assert_eq!(output, b"not json\n");
        assert_eq!(result.input_tokens, None);
    }
}
