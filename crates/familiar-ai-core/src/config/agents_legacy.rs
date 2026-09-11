use serde::{Deserialize, Serialize};

use super::agent_entry::AgentEntryConfig;
use super::review::ReviewConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsConfig {
    #[serde(default)]
    pub implementation: AgentEntryConfig,
    #[serde(default)]
    pub reviewer: AgentEntryConfig,
}

impl AgentsConfig {
    /// Fail-closed validation, including consistency with the authoritative
    /// review audit identities when review is enabled. Runs only when the
    /// `[agents]` section is present.
    pub fn validate(&self, review: &ReviewConfig) -> Result<(), String> {
        self.implementation.validate("implementation", false)?;
        self.reviewer.validate("reviewer", true)?;
        if review.enabled {
            for (role, entry, identity) in [
                (
                    "implementation",
                    &self.implementation,
                    &review.implementation_agent,
                ),
                ("reviewer", &self.reviewer, &review.reviewer_agent),
            ] {
                if identity.adapter_id != entry.adapter.as_str() {
                    return Err(format!(
                        "review.{role}_agent.adapter_id '{}' contradicts agents.{role}.adapter '{}'",
                        identity.adapter_id,
                        entry.adapter.as_str()
                    ));
                }
                if let (Some(review_model), Some(agent_model)) = (&identity.model, &entry.model) {
                    if review_model != agent_model {
                        return Err(format!(
                            "review.{role}_agent.model '{review_model}' contradicts agents.{role}.model '{agent_model}'"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
