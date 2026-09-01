use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupConfig {
    #[serde(default = "default_rollup_enabled")]
    pub enabled: bool,
    #[serde(default = "default_rollup_default_limit")]
    pub default_limit: usize,
    #[serde(default = "default_max_rollup_tokens")]
    pub max_rollup_tokens: usize,
    #[serde(default = "default_max_rollup_chars")]
    pub max_rollup_chars: usize,
}

fn default_rollup_enabled() -> bool {
    true
}

fn default_rollup_default_limit() -> usize {
    20
}

fn default_max_rollup_tokens() -> usize {
    4000
}

fn default_max_rollup_chars() -> usize {
    50_000
}

impl Default for RollupConfig {
    fn default() -> Self {
        Self {
            enabled: default_rollup_enabled(),
            default_limit: default_rollup_default_limit(),
            max_rollup_tokens: default_max_rollup_tokens(),
            max_rollup_chars: default_max_rollup_chars(),
        }
    }
}
