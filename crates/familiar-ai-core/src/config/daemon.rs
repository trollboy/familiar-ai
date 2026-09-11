use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub pid_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_control_plane_ceiling")]
    pub global_concurrency_ceiling: usize,
    #[serde(default = "default_control_plane_ceiling")]
    pub default_project_concurrency_ceiling: usize,
    #[serde(default = "default_health_timeout_ms")]
    pub health_timeout_ms: u64,
}

fn default_heartbeat_interval() -> u64 {
    60
}

fn default_control_plane_ceiling() -> usize {
    1
}
fn default_health_timeout_ms() -> u64 {
    5_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub format: LogFormat,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<PathBuf>,
}

impl DatabaseConfig {
    pub fn resolve_path(&self, data_dir: &Path) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| data_dir.join("familiar.db"))
    }
}

// --- Inference config (replaces old LlmConfig) ---

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: None,
            socket_path: None,
            heartbeat_interval_secs: default_heartbeat_interval(),
            global_concurrency_ceiling: default_control_plane_ceiling(),
            default_project_concurrency_ceiling: default_control_plane_ceiling(),
            health_timeout_ms: default_health_timeout_ms(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            format: LogFormat::default(),
        }
    }
}

/// The configuration environment prefix.
pub const ENV_PREFIX: &str = "FAMILIAR_AI_";
/// The removed pre-rename prefix, named only so stale configuration fails
/// closed instead of being silently ignored.
/// identity-gate exception: the legacy prefix is intentional here.
pub const LEGACY_ENV_PREFIX: &str = "FAMILIAR_"; // identity-gate: allow

/// Legacy-prefixed variable names in `keys`, sorted. Non-empty means
/// configuration loading must fail closed.
pub fn stale_legacy_env(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut stale: Vec<String> = keys
        .filter(|key| key.starts_with(LEGACY_ENV_PREFIX) && !key.starts_with(ENV_PREFIX))
        .collect();
    stale.sort();
    stale
}
