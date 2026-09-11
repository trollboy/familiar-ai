use std::path::PathBuf;

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
