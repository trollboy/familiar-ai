use std::fmt;
use std::io;
use std::path::Path;

pub trait CodingAgent: Send + Sync {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError>;

    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::Unavailable
    }

    /// Deterministic availability probe used before a backlog item is claimed.
    /// Test and in-process agents need no external prerequisite by default.
    fn preflight(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationCapability {
    Unavailable,
    FreshProcessPerExecution,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionRequest<'a> {
    pub working_directory: &'a Path,
    /// Repository tree that the child process must not be able to read.
    /// Used only for isolated review execution.
    pub denied_read_path: Option<&'a Path>,
    pub prompt: &'a str,
    pub filesystem: FilesystemPolicy,
    pub model: Option<&'a str>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemPolicy {
    Normal,
    ReadOnly,
    WorkspaceWrite,
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
    /// Vendor session identity, recorded as provenance only. `None` for
    /// adapters that do not report one.
    pub session_id: Option<String>,
    /// Cost self-reported by the agent in micro-USD. Observability only:
    /// pricing-config estimation remains the sole source of
    /// `estimated_cost_microusd` in execution history.
    pub reported_cost_microusd: Option<u64>,
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
    MalformedOutput {
        detail: String,
        result: Box<ExecutionResult>,
    },
    Timeout {
        result: Box<ExecutionResult>,
    },
    /// The agent's self-reported cost exceeded the configured adapter budget
    /// ceiling. Detection is post-execution; the complete result is retained.
    BudgetExceeded {
        limit_microusd: u64,
        reported_microusd: u64,
        result: Box<ExecutionResult>,
    },
}

impl AgentExecutionError {
    pub fn result(&self) -> &ExecutionResult {
        match self {
            Self::Launch { result, .. }
            | Self::Input { result, .. }
            | Self::Wait { result, .. }
            | Self::Output { result, .. }
            | Self::MalformedOutput { result, .. }
            | Self::Timeout { result }
            | Self::BudgetExceeded { result, .. } => result.as_ref(),
        }
    }
}

impl fmt::Display for AgentExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch {
                executable, source, ..
            } => write!(f, "cannot launch agent executable {executable:?}: {source}"),
            Self::Input { source, .. } => {
                write!(f, "cannot feed execution prompt to the agent: {source}")
            }
            Self::Wait { source, .. } => write!(f, "cannot wait for the agent: {source}"),
            Self::Output { source, .. } => {
                write!(f, "cannot read agent structured output: {source}")
            }
            Self::MalformedOutput { detail, .. } => {
                write!(f, "agent did not produce a valid terminal result: {detail}")
            }
            Self::Timeout { .. } => {
                write!(f, "agent execution exceeded its configured timeout")
            }
            Self::BudgetExceeded {
                limit_microusd,
                reported_microusd,
                ..
            } => write!(
                f,
                "agent-reported cost {reported_microusd} micro-USD exceeds the configured adapter budget {limit_microusd} micro-USD"
            ),
        }
    }
}

impl std::error::Error for AgentExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout { .. } | Self::BudgetExceeded { .. } | Self::MalformedOutput { .. } => {
                None
            }
            Self::Launch { source, .. }
            | Self::Input { source, .. }
            | Self::Wait { source, .. }
            | Self::Output { source, .. } => Some(source.as_ref()),
        }
    }
}
