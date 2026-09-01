use serde::{Deserialize, Serialize};

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
