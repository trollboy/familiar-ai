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
