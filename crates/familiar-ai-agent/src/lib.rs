mod agent;
mod claude_code;
mod codex;
mod isolation;
mod registry;

pub use agent::{
    AgentExecutionError, BudgetCapability, BudgetDenomination, CodingAgent, ExecutionBudget,
    ExecutionRequest, ExecutionResult, FilesystemPolicy, IsolationCapability,
};
pub use claude_code::{ClaudeCodeAgent, ClaudeCodeSettings, READ_ONLY_RESTRICTIONS};
pub use codex::CodexAgent;
pub use registry::{
    builtin_adapter_factories, AdapterFactories, AdapterFactory, CandidateEvaluation,
    RejectionReason, RouteError, RouteRequest, SelectionRecord, WorkerCapability, WorkerDescriptor,
    WorkerRegistry, WorkerStage,
};
