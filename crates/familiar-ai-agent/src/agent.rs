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

    fn budget_capability(&self) -> BudgetCapability {
        BudgetCapability::NONE
    }

    /// Deterministic availability probe used before a backlog item is claimed.
    /// Test and in-process agents need no external prerequisite by default.
    fn preflight(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetCapability {
    pub cost: bool,
    pub tokens: bool,
    pub duration: bool,
    pub cost_always_zero: bool,
}

impl BudgetCapability {
    pub const NONE: Self = Self {
        cost: false,
        tokens: false,
        duration: false,
        cost_always_zero: false,
    };
    pub const CLAUDE_CODE: Self = Self {
        cost: true,
        tokens: false,
        duration: false,
        cost_always_zero: false,
    };

    pub fn supports(self, denomination: BudgetDenomination) -> bool {
        match denomination {
            BudgetDenomination::Cost => self.cost,
            BudgetDenomination::Tokens => self.tokens,
            BudgetDenomination::Duration => self.duration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDenomination {
    Cost,
    Tokens,
    Duration,
}

impl fmt::Display for BudgetDenomination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cost => "cost",
            Self::Tokens => "tokens",
            Self::Duration => "duration",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub max_cost_microusd: Option<std::num::NonZeroU64>,
    pub max_tokens: Option<std::num::NonZeroU64>,
    pub max_duration_ms: Option<std::num::NonZeroU64>,
}

impl ExecutionBudget {
    pub fn denominations(self) -> impl Iterator<Item = BudgetDenomination> {
        [
            self.max_cost_microusd.map(|_| BudgetDenomination::Cost),
            self.max_tokens.map(|_| BudgetDenomination::Tokens),
            self.max_duration_ms.map(|_| BudgetDenomination::Duration),
        ]
        .into_iter()
        .flatten()
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
    pub budget: ExecutionBudget,
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
    BudgetStopped {
        result: Box<ExecutionResult>,
    },
    UnenforceableBudget {
        adapter: &'static str,
        denomination: BudgetDenomination,
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
            Self::BudgetStopped { result } => result.as_ref(),
            Self::UnenforceableBudget { result, .. } => result.as_ref(),
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
            Self::UnenforceableBudget { adapter, denomination, .. } => write!(f, "adapter {adapter} cannot enforce a per-execution {denomination} ceiling"),
            Self::BudgetStopped { .. } => write!(f, "agent execution reached its enforced budget ceiling"),
        }
    }
}

impl std::error::Error for AgentExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout { .. }
            | Self::BudgetExceeded { .. }
            | Self::BudgetStopped { .. }
            | Self::UnenforceableBudget { .. }
            | Self::MalformedOutput { .. } => None,
            Self::Launch { source, .. }
            | Self::Input { source, .. }
            | Self::Wait { source, .. }
            | Self::Output { source, .. } => Some(source.as_ref()),
        }
    }
}
