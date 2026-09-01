use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayConfig {
    #[serde(default = "default_tray_enabled")]
    pub enabled: bool,
    #[serde(default = "default_recent_projects_count")]
    pub recent_projects_count: usize,
}

fn default_tray_enabled() -> bool {
    true
}

fn default_recent_projects_count() -> usize {
    5
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            enabled: default_tray_enabled(),
            recent_projects_count: default_recent_projects_count(),
        }
    }
}
