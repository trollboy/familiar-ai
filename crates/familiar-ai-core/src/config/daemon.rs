use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
