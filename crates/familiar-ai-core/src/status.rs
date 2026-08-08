use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AppStatus {
    pub startup_time: DateTime<Utc>,
    pub active_projects: usize,
    pub local_llm_enabled: bool,
    pub mcp_enabled: bool,
    pub last_heartbeat: DateTime<Utc>,
}

impl AppStatus {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            startup_time: now,
            active_projects: 0,
            local_llm_enabled: false,
            mcp_enabled: false,
            last_heartbeat: now,
        }
    }

    pub fn record_heartbeat(&mut self) {
        self.last_heartbeat = Utc::now();
    }
}

impl Default for AppStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_sane_defaults() {
        let status = AppStatus::new();
        assert_eq!(status.active_projects, 0);
        assert!(!status.local_llm_enabled);
        assert!(!status.mcp_enabled);
        assert!(status.startup_time <= Utc::now());
    }

    #[test]
    fn record_heartbeat_updates_timestamp() {
        let mut status = AppStatus::new();
        let before = status.last_heartbeat;
        std::thread::sleep(std::time::Duration::from_millis(10));
        status.record_heartbeat();
        assert!(status.last_heartbeat > before);
    }
}
