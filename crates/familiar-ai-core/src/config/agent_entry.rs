use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentAdapterKind {
    #[default]
    Codex,
    ClaudeCode,
    /// Ollama is invoked through the existing Codex OSS adapter.
    Ollama,
}

impl AgentAdapterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Ollama => "ollama",
        }
    }
    pub fn default_executable(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::Ollama => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEffort {
    Low,
    Medium,
    High,
}

impl AgentEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentPermissionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

impl AgentPermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

/// Flags the adapter owns or forbids; configured `extra_args` may not name
/// them, exactly or in `<flag>=value` form.
pub const FORBIDDEN_AGENT_EXTRA_ARGS: [&str; 11] = [
    "--print",
    "--output-format",
    "--input-format",
    "--verbose",
    "--model",
    "--permission-mode",
    "--resume",
    "--continue",
    "--session-id",
    "--fork-session",
    "--dangerously-skip-permissions",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntryConfig {
    #[serde(default)]
    pub adapter: AgentAdapterKind,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<AgentEffort>,
    #[serde(default)]
    pub permission_mode: Option<AgentPermissionMode>,
    /// 0 or absent means no per-execution cost ceiling.
    #[serde(default)]
    #[serde(alias = "max_budget_microusd")]
    pub max_execution_cost_microusd: u64,
    #[serde(default)]
    pub max_execution_tokens: u64,
    #[serde(default)]
    pub max_execution_duration_ms: u64,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl AgentEntryConfig {
    pub fn resolved_executable(&self) -> String {
        self.executable
            .clone()
            .unwrap_or_else(|| self.adapter.default_executable().to_owned())
    }

    pub(crate) fn validate(&self, role: &str, is_reviewer: bool) -> Result<(), String> {
        if let Some(executable) = &self.executable {
            if executable.trim().is_empty() {
                return Err(format!("[agents.{role}] executable must be non-empty"));
            }
        }
        if matches!(
            self.adapter,
            AgentAdapterKind::Codex | AgentAdapterKind::Ollama
        ) && (self.effort.is_some() || self.permission_mode.is_some())
        {
            return Err(format!(
                "[agents.{role}] effort and permission_mode are valid only for adapter \"claude-code\""
            ));
        }
        if is_reviewer && self.permission_mode == Some(AgentPermissionMode::BypassPermissions) {
            return Err(format!(
                "[agents.{role}] bypassPermissions is never permitted for the reviewer"
            ));
        }
        for arg in &self.extra_args {
            for flag in FORBIDDEN_AGENT_EXTRA_ARGS {
                if arg == flag || arg.starts_with(&format!("{flag}=")) {
                    return Err(format!(
                        "[agents.{role}] extra_args may not include adapter-owned or forbidden flag '{flag}'"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlannerConfig {
    #[serde(flatten)]
    pub agent: AgentEntryConfig,
    #[serde(default = "default_planner_max_prds")]
    pub max_prds_per_batch: usize,
    #[serde(default = "default_planner_max_bytes")]
    pub max_bytes_per_prd: usize,
}

const fn default_planner_max_prds() -> usize {
    8
}
const fn default_planner_max_bytes() -> usize {
    64 * 1024
}

impl PlannerConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.agent.validate("planner", false)?;
        if self.max_prds_per_batch == 0 || self.max_bytes_per_prd == 0 {
            return Err(
                "[planner] max_prds_per_batch and max_bytes_per_prd must be positive and finite"
                    .into(),
            );
        }
        Ok(())
    }
}
