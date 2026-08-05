use std::fmt;
use std::io;
use std::path::Path;

pub trait CodingAgent {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError>;

    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::Unavailable
    }

    /// What this adapter can natively bound, declared the way
    /// `isolation_capability()` declares isolation. Adapters that enforce
    /// nothing use the default: every denomination `Unenforced`.
    fn budget_capability(&self) -> BudgetCapability {
        BudgetCapability::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationCapability {
    Unavailable,
    FreshProcessPerExecution,
}

/// Whether an adapter can bound a given budget denomination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DenominationCapability {
    /// The adapter enforces a configured ceiling in this denomination
    /// pre-emptively, before it can be exceeded.
    Enforced,
    /// The adapter cannot enforce or meaningfully bound this denomination; a
    /// per-execution ceiling here is refused before launch.
    #[default]
    Unenforced,
    /// The adapter has no marginal consumption in this denomination and
    /// always reports it as a known zero; a ceiling bound only by
    /// always-zero denominations provides no real bound.
    AlwaysZero,
}

/// An adapter's declared native budget-enforcement capability, one entry per
/// denomination. The default (all `Unenforced`) is exactly correct for an
/// adapter that enforces nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetCapability {
    pub cost: DenominationCapability,
    pub tokens: DenominationCapability,
    pub duration: DenominationCapability,
}

/// Why a declared [`ExecutionBudget`] was refused before launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetRefusal {
    /// A declared ceiling was zero (or otherwise not finite and positive).
    InvalidCeiling { denomination: &'static str },
    /// The adapter cannot enforce a ceiling in this denomination at all.
    Unenforceable {
        adapter: String,
        denomination: &'static str,
    },
    /// Every declared denomination is one the adapter always reports as
    /// zero, so the declared budget provides no actual bound.
    NotAWarrant {
        adapter: String,
        denominations: Vec<&'static str>,
    },
}

impl fmt::Display for BudgetRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCeiling { denomination } => write!(
                f,
                "budget ceiling for {denomination} must be finite and positive when declared"
            ),
            Self::Unenforceable {
                adapter,
                denomination,
            } => write!(
                f,
                "adapter {adapter:?} cannot enforce a {denomination} budget ceiling"
            ),
            Self::NotAWarrant {
                adapter,
                denominations,
            } => write!(
                f,
                "adapter {adapter:?} always reports {} as zero; a budget bounded only by {} is not a warrant",
                denominations.join(", "),
                denominations.join(", "),
            ),
        }
    }
}

/// A ceiling over cost, tokens, and duration. Any subset may be declared;
/// every declared ceiling is finite and positive by construction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub max_cost_microusd: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_duration_ms: Option<u64>,
}

impl ExecutionBudget {
    /// No ceiling declared in any denomination.
    pub const NONE: Self = Self {
        max_cost_microusd: None,
        max_tokens: None,
        max_duration_ms: None,
    };

    /// Construct a budget, refusing any declared-but-zero ceiling.
    pub fn try_new(
        max_cost_microusd: Option<u64>,
        max_tokens: Option<u64>,
        max_duration_ms: Option<u64>,
    ) -> Result<Self, BudgetRefusal> {
        for (value, denomination) in [
            (max_cost_microusd, "cost"),
            (max_tokens, "tokens"),
            (max_duration_ms, "duration"),
        ] {
            if value == Some(0) {
                return Err(BudgetRefusal::InvalidCeiling { denomination });
            }
        }
        Ok(Self {
            max_cost_microusd,
            max_tokens,
            max_duration_ms,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.max_cost_microusd.is_none()
            && self.max_tokens.is_none()
            && self.max_duration_ms.is_none()
    }

    /// Refuse before launch: a denomination the adapter cannot enforce
    /// natively, or a budget whose only declared denominations are ones the
    /// adapter always reports as zero (no real bound).
    pub fn validate_for_adapter(
        &self,
        capability: &BudgetCapability,
        adapter: &str,
    ) -> Result<(), BudgetRefusal> {
        let entries = [
            (self.max_cost_microusd, capability.cost, "cost"),
            (self.max_tokens, capability.tokens, "tokens"),
            (self.max_duration_ms, capability.duration, "duration"),
        ];
        let mut has_real_bound = false;
        let mut always_zero_only = Vec::new();
        for (value, cap, denomination) in entries {
            if value.is_none() {
                continue;
            }
            match cap {
                DenominationCapability::Unenforced => {
                    return Err(BudgetRefusal::Unenforceable {
                        adapter: adapter.to_owned(),
                        denomination,
                    });
                }
                DenominationCapability::Enforced => has_real_bound = true,
                DenominationCapability::AlwaysZero => always_zero_only.push(denomination),
            }
        }
        if !has_real_bound && !always_zero_only.is_empty() {
            return Err(BudgetRefusal::NotAWarrant {
                adapter: adapter.to_owned(),
                denominations: always_zero_only,
            });
        }
        Ok(())
    }
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
    /// Per-execution ceiling, empty when no budget was declared for this
    /// call. Adapters that cannot enforce a declared denomination are never
    /// handed a request bearing it — refusal happens before launch.
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
    /// The vendor itself stopped the execution because a declared
    /// pre-emptive budget ceiling was reached. A distinct, closed outcome —
    /// not a failure to retry — with the complete partial result retained.
    BudgetStopped {
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
            | Self::Timeout { result }
            | Self::BudgetExceeded { result, .. }
            | Self::BudgetStopped { result } => result.as_ref(),
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
            Self::BudgetStopped { .. } => write!(
                f,
                "agent execution stopped by its own pre-emptive budget ceiling before completion"
            ),
        }
    }
}

impl std::error::Error for AgentExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout { .. } | Self::BudgetExceeded { .. } | Self::BudgetStopped { .. } => None,
            Self::Launch { source, .. }
            | Self::Input { source, .. }
            | Self::Wait { source, .. }
            | Self::Output { source, .. } => Some(source.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_zero_ceiling_is_refused_in_any_denomination() {
        assert_eq!(
            ExecutionBudget::try_new(Some(0), None, None),
            Err(BudgetRefusal::InvalidCeiling {
                denomination: "cost"
            })
        );
        assert_eq!(
            ExecutionBudget::try_new(None, Some(0), None),
            Err(BudgetRefusal::InvalidCeiling {
                denomination: "tokens"
            })
        );
        assert_eq!(
            ExecutionBudget::try_new(None, None, Some(0)),
            Err(BudgetRefusal::InvalidCeiling {
                denomination: "duration"
            })
        );
        assert!(ExecutionBudget::try_new(Some(1), None, None).is_ok());
        assert!(ExecutionBudget::try_new(None, None, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn empty_budget_never_refused() {
        let capability = BudgetCapability::default();
        assert!(ExecutionBudget::NONE
            .validate_for_adapter(&capability, "codex")
            .is_ok());
    }

    #[test]
    fn unenforceable_denomination_is_refused_naming_adapter_and_denomination() {
        let budget = ExecutionBudget::try_new(Some(1), None, None).unwrap();
        let capability = BudgetCapability::default(); // all Unenforced
        assert_eq!(
            budget.validate_for_adapter(&capability, "codex"),
            Err(BudgetRefusal::Unenforceable {
                adapter: "codex".into(),
                denomination: "cost"
            })
        );
    }

    #[test]
    fn enforced_denomination_passes() {
        let budget = ExecutionBudget::try_new(Some(1), None, None).unwrap();
        let capability = BudgetCapability {
            cost: DenominationCapability::Enforced,
            ..BudgetCapability::default()
        };
        assert!(budget
            .validate_for_adapter(&capability, "claude-code")
            .is_ok());
    }

    #[test]
    fn warrant_bounded_only_by_always_zero_denomination_is_refused() {
        let budget = ExecutionBudget::try_new(Some(1), None, None).unwrap();
        let capability = BudgetCapability {
            cost: DenominationCapability::AlwaysZero,
            ..BudgetCapability::default()
        };
        assert_eq!(
            budget.validate_for_adapter(&capability, "costless"),
            Err(BudgetRefusal::NotAWarrant {
                adapter: "costless".into(),
                denominations: vec!["cost"]
            })
        );
    }

    #[test]
    fn always_zero_denomination_alongside_a_real_bound_is_accepted() {
        let budget = ExecutionBudget::try_new(Some(1), Some(1), None).unwrap();
        let capability = BudgetCapability {
            cost: DenominationCapability::AlwaysZero,
            tokens: DenominationCapability::Enforced,
            duration: DenominationCapability::Unenforced,
        };
        assert!(budget.validate_for_adapter(&capability, "hybrid").is_ok());
    }

    #[test]
    fn default_budget_capability_is_unenforced_everywhere() {
        assert_eq!(
            BudgetCapability::default(),
            BudgetCapability {
                cost: DenominationCapability::Unenforced,
                tokens: DenominationCapability::Unenforced,
                duration: DenominationCapability::Unenforced,
            }
        );
    }
}
