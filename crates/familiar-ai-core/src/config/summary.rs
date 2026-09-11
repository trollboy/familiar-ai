use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryConfig {
    #[serde(default = "default_summary_enabled")]
    pub enabled: bool,
    #[serde(default = "default_staleness_threshold_secs")]
    pub staleness_threshold_secs: u64,
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: u64,
    #[serde(default = "default_max_pending_files")]
    pub max_pending_files: usize,
    #[serde(default = "default_per_file_quiet_ms")]
    pub per_file_quiet_ms: u64,
}

fn default_summary_enabled() -> bool {
    true
}

fn default_staleness_threshold_secs() -> u64 {
    86_400 // 24h
}

fn default_flush_interval_secs() -> u64 {
    3
}

fn default_max_file_size_bytes() -> u64 {
    1_048_576 // 1 MB
}

fn default_max_pending_files() -> usize {
    10_000
}

fn default_per_file_quiet_ms() -> u64 {
    1_500
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            enabled: default_summary_enabled(),
            staleness_threshold_secs: default_staleness_threshold_secs(),
            flush_interval_secs: default_flush_interval_secs(),
            max_file_size_bytes: default_max_file_size_bytes(),
            max_pending_files: default_max_pending_files(),
            per_file_quiet_ms: default_per_file_quiet_ms(),
        }
    }
}

// TODO: future PRDs may want per-project summary settings (e.g., custom
// max_file_size or custom ignore patterns) overriding the global SummaryConfig.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dashboard_bind")]
    pub bind_address: String,
}

fn default_dashboard_bind() -> String {
    "127.0.0.1:9400".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_dashboard_bind(),
        }
    }
}
