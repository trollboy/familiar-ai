use serde::{Deserialize, Serialize};

/// Portable user-supervisor settings. The worker deliberately inherits only
/// PATH; credentials required by preflight must be supplied by the supervisor
/// environment and are checked before a PRD is claimed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    #[serde(default = "default_worker_label")]
    pub label: String,
    #[serde(default = "default_worker_restart_throttle_secs")]
    pub restart_throttle_secs: u64,
    #[serde(default = "default_worker_max_prds")]
    pub max_prds_per_run: u64,
}

fn default_worker_label() -> String {
    "ai.familiar.worker".into()
}

fn default_worker_restart_throttle_secs() -> u64 {
    10
}

fn default_worker_max_prds() -> u64 {
    1
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            label: default_worker_label(),
            restart_throttle_secs: default_worker_restart_throttle_secs(),
            max_prds_per_run: default_worker_max_prds(),
        }
    }
}

impl WorkerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.label.is_empty()
            || !self
                .label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        {
            return Err("worker.label must contain only letters, digits, '.', '-', or '_'".into());
        }
        if self.restart_throttle_secs == 0 {
            return Err("worker.restart_throttle_secs must be positive".into());
        }
        if self.max_prds_per_run == 0 {
            return Err("worker.max_prds_per_run must be positive and finite".into());
        }
        Ok(())
    }
}
