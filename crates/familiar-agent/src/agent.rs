use std::fmt;
use std::io;
use std::path::Path;

pub trait CodingAgent {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError>;
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionRequest<'a> {
    pub working_directory: &'a Path,
    pub prompt: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionResult {
    pub agent_version: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug)]
pub enum AgentExecutionError {
    Launch {
        executable: String,
        source: Box<io::Error>,
        result: Box<ExecutionResult>,
    },
    Input {
        source: Box<io::Error>,
        result: Box<ExecutionResult>,
    },
    Wait {
        source: Box<io::Error>,
        result: Box<ExecutionResult>,
    },
    Output {
        source: Box<io::Error>,
        result: Box<ExecutionResult>,
    },
}

impl AgentExecutionError {
    pub fn result(&self) -> &ExecutionResult {
        match self {
            Self::Launch { result, .. }
            | Self::Input { result, .. }
            | Self::Wait { result, .. }
            | Self::Output { result, .. } => result.as_ref(),
        }
    }
}

impl fmt::Display for AgentExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch {
                executable, source, ..
            } => write!(f, "cannot launch Codex executable {executable:?}: {source}"),
            Self::Input { source, .. } => {
                write!(f, "cannot feed execution prompt to Codex: {source}")
            }
            Self::Wait { source, .. } => write!(f, "cannot wait for Codex: {source}"),
            Self::Output { source, .. } => {
                write!(f, "cannot read Codex structured output: {source}")
            }
        }
    }
}

impl std::error::Error for AgentExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(
            match self {
                Self::Launch { source, .. }
                | Self::Input { source, .. }
                | Self::Wait { source, .. }
                | Self::Output { source, .. } => source,
            }
            .as_ref(),
        )
    }
}
