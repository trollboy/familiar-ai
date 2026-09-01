use serde::{Deserialize, Serialize};

/// The closed PRD-058 canonical tool capability vocabulary, spelled exactly
/// as `familiar_ai_agent::raw_runtime::CapabilityId::as_str()` does. Kept as
/// a standalone string list rather than a shared type because
/// `familiar-ai-core` has no dependency on `familiar-ai-agent` (and must
/// not gain one: core stays foundational).
pub const AGENT_RUNTIME_CAPABILITIES: [&str; 7] = [
    "read-file",
    "search-list",
    "run-command",
    "apply-edit",
    "report-progress",
    "submit-evidence",
    "request-escalation",
];

/// The read-only-plus-reporting set phase 1 of PRD-058 calls the smallest
/// viable worker: an independent reviewer/narrow-task worker.
fn default_offered_capabilities() -> Vec<String> {
    vec![
        "read-file".to_string(),
        "search-list".to_string(),
        "report-progress".to_string(),
        "submit-evidence".to_string(),
        "request-escalation".to_string(),
    ]
}

fn default_max_iterations() -> u32 {
    20
}

fn default_true() -> bool {
    true
}

/// Configuration for the Familiar-owned raw-model agent loop (PRD-058).
/// Absent entirely means the raw runtime is disabled and no behavior
/// changes for any existing harness-driven execution path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Canonical capability ids offered to the model this turn. Must be a
    /// subset of [`AGENT_RUNTIME_CAPABILITIES`]; a projection can narrow the
    /// canonical set but this config can never widen it.
    #[serde(default = "default_offered_capabilities")]
    pub offered_capabilities: Vec<String>,
    #[serde(default)]
    pub ceilings: AgentRuntimeCeilingsConfig,
    #[serde(default)]
    pub sandbox: AgentRuntimeSandboxConfig,
    /// Enables the PRD-029 stable-prefix cache strategy and adapter cache
    /// controls. Unsupported adapters retain native behavior.
    #[serde(default = "default_true")]
    pub prompt_cache_enabled: bool,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            offered_capabilities: default_offered_capabilities(),
            ceilings: AgentRuntimeCeilingsConfig::default(),
            sandbox: AgentRuntimeSandboxConfig::default(),
            prompt_cache_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeCeilingsConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_clock_ms: Option<u64>,
}

impl Default for AgentRuntimeCeilingsConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            max_output_tokens: None,
            max_context_tokens: None,
            max_wall_clock_ms: None,
        }
    }
}

/// Command and network policy for the `run-command` capability. Every field
/// is a fail-closed allowlist: an empty `allowed_commands` means no command
/// may ever run, and `network_allowed` defaults to deny-by-default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeSandboxConfig {
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub network_allowed: bool,
    /// Environment variable names (never values) explicitly allowed to
    /// reach a tool subprocess beyond the adapter's own inference
    /// credentials. Billing/admin credential names must never appear here;
    /// validation rejects known billing/admin markers closed.
    #[serde(default)]
    pub allowed_environment: Vec<String>,
}

const FORBIDDEN_ENVIRONMENT_MARKERS: [&str; 5] = ["BILLING", "ADMIN", "STRIPE", "PAYMENT", "ROOT_"];

impl AgentRuntimeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.offered_capabilities.is_empty() {
            return Err("agent_runtime.offered_capabilities must not be empty when agent_runtime.enabled = true".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for capability in &self.offered_capabilities {
            if !AGENT_RUNTIME_CAPABILITIES.contains(&capability.as_str()) {
                return Err(format!(
                    "agent_runtime.offered_capabilities contains unknown capability {capability:?}; must be one of {AGENT_RUNTIME_CAPABILITIES:?}"
                ));
            }
            if !seen.insert(capability.as_str()) {
                return Err(format!(
                    "agent_runtime.offered_capabilities contains duplicate capability {capability:?}"
                ));
            }
        }
        if self.ceilings.max_iterations == 0 {
            return Err("agent_runtime.ceilings.max_iterations must be greater than zero".into());
        }
        if self.offered_capabilities.iter().any(|c| c == "run-command")
            && self.sandbox.allowed_commands.is_empty()
        {
            return Err(
                "agent_runtime.sandbox.allowed_commands must be non-empty when run-command is offered"
                    .into(),
            );
        }
        for name in &self.sandbox.allowed_environment {
            let upper = name.to_ascii_uppercase();
            if FORBIDDEN_ENVIRONMENT_MARKERS
                .iter()
                .any(|marker| upper.contains(marker))
            {
                return Err(format!(
                    "agent_runtime.sandbox.allowed_environment must never name a billing/admin credential; offending entry {name:?}"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_default_validates() {
        assert!(AgentRuntimeConfig::default().validate().is_ok());
    }

    #[test]
    fn enabled_with_unknown_capability_fails_closed() {
        let config = AgentRuntimeConfig {
            enabled: true,
            offered_capabilities: vec!["delete-everything".into()],
            ..AgentRuntimeConfig::default()
        };
        assert!(config
            .validate()
            .unwrap_err()
            .contains("unknown capability"));
    }

    #[test]
    fn run_command_without_allowlist_fails_closed() {
        let config = AgentRuntimeConfig {
            enabled: true,
            offered_capabilities: vec!["run-command".into()],
            ..AgentRuntimeConfig::default()
        };
        assert!(config
            .validate()
            .unwrap_err()
            .contains("allowed_commands must be non-empty"));
    }

    #[test]
    fn run_command_with_allowlist_validates() {
        let config = AgentRuntimeConfig {
            enabled: true,
            offered_capabilities: vec!["run-command".into()],
            sandbox: AgentRuntimeSandboxConfig {
                allowed_commands: vec!["cargo".into()],
                network_allowed: false,
                allowed_environment: vec![],
            },
            ..AgentRuntimeConfig::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn billing_credential_names_are_rejected_closed() {
        let config = AgentRuntimeConfig {
            enabled: true,
            sandbox: AgentRuntimeSandboxConfig {
                allowed_commands: vec![],
                network_allowed: false,
                allowed_environment: vec!["STRIPE_SECRET_KEY".into()],
            },
            ..AgentRuntimeConfig::default()
        };
        assert!(config
            .validate()
            .unwrap_err()
            .contains("billing/admin credential"));
    }

    #[test]
    fn zero_iteration_ceiling_fails_closed() {
        let config = AgentRuntimeConfig {
            enabled: true,
            ceilings: AgentRuntimeCeilingsConfig {
                max_iterations: 0,
                ..AgentRuntimeCeilingsConfig::default()
            },
            ..AgentRuntimeConfig::default()
        };
        assert!(config
            .validate()
            .unwrap_err()
            .contains("max_iterations must be greater than zero"));
    }

    #[test]
    fn duplicate_capability_fails_closed() {
        let config = AgentRuntimeConfig {
            enabled: true,
            offered_capabilities: vec!["read-file".into(), "read-file".into()],
            ..AgentRuntimeConfig::default()
        };
        assert!(config.validate().unwrap_err().contains("duplicate"));
    }
}
