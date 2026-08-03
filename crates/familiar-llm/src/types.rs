use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded(String),
    Unavailable(String),
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmHealthState {
    pub loaded: bool,
    pub healthy: bool,
    pub backend_name: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub loaded_at: Option<DateTime<Utc>>,
}

impl LlmHealthState {
    /// Disabled state — not an error. Used when the manager has no backend.
    pub fn disabled() -> Self {
        Self {
            loaded: false,
            healthy: false,
            backend_name: None,
            last_check: Some(Utc::now()),
            last_error: None,
            loaded_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_is_healthy() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Degraded("x".into()).is_healthy());
        assert!(!HealthStatus::Unavailable("y".into()).is_healthy());
    }

    #[test]
    fn disabled_state_is_not_an_error() {
        let s = LlmHealthState::disabled();
        assert!(!s.loaded);
        assert!(!s.healthy);
        assert!(s.backend_name.is_none());
        assert!(s.last_error.is_none()); // crucial: disabled is not an error
        assert!(s.last_check.is_some());
    }

    #[test]
    fn default_state_is_empty() {
        let s = LlmHealthState::default();
        assert!(!s.loaded);
        assert!(!s.healthy);
        assert!(s.last_check.is_none());
    }
}
