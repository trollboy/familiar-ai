use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default = "default_dashboard_enabled")]
    pub enabled: bool,
    #[serde(default = "default_dashboard_bind")]
    pub bind_address: String,
}

fn default_dashboard_bind() -> String {
    "127.0.0.1:9400".to_string()
}

fn default_dashboard_enabled() -> bool {
    cfg!(target_os = "macos")
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: default_dashboard_enabled(),
            bind_address: default_dashboard_bind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_default_matches_the_native_ui_available_on_this_platform() {
        assert_eq!(
            DashboardConfig::default().enabled,
            cfg!(target_os = "macos")
        );
    }

    #[test]
    fn an_explicit_dashboard_choice_still_wins() {
        let disabled: DashboardConfig = toml::from_str("enabled = false").unwrap();
        let enabled: DashboardConfig = toml::from_str("enabled = true").unwrap();
        assert!(!disabled.enabled);
        assert!(enabled.enabled);
    }
}
