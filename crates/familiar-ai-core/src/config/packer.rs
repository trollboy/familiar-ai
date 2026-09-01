use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetProfile {
    Minimal,
    #[default]
    Balanced,
    Aggressive,
    MaxAccuracy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackerConfig {
    #[serde(default)]
    pub default_profile: BudgetProfile,
    #[serde(default = "default_packer_hard_ceiling")]
    pub hard_ceiling_tokens: usize,
}

fn default_packer_hard_ceiling() -> usize {
    15_000
}

impl Default for PackerConfig {
    fn default() -> Self {
        Self {
            default_profile: BudgetProfile::default(),
            hard_ceiling_tokens: default_packer_hard_ceiling(),
        }
    }
}
