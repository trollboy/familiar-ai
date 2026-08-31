pub mod backlog;
pub mod billing;
pub mod bootstrap;
pub mod checkpoint;
pub mod config_decision;
pub mod control_plane;
pub mod decision;
pub mod delivery;
pub mod driver;
pub mod execution_history;
pub mod file_summary;
pub mod lifecycle;
pub mod orchestration;
pub mod planner;
pub mod probation;
pub mod project;
pub mod project_config;
pub mod reservation;
pub mod review;
pub mod session_rollup;
pub mod stats;
pub mod stewardship;
pub mod worker_selection;
pub mod worker_spec;

use chrono::{DateTime, Utc};
use familiar_ai_core::FamiliarError;

pub(crate) fn parse_dt(s: &str) -> familiar_ai_core::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| FamiliarError::Database(format!("failed to parse timestamp '{s}': {e}")))
}

pub(crate) fn json_to_vec(s: &str) -> familiar_ai_core::Result<Vec<String>> {
    serde_json::from_str(s)
        .map_err(|e| FamiliarError::Database(format!("failed to parse JSON array: {e}")))
}

pub(crate) fn vec_to_json(v: &[String]) -> familiar_ai_core::Result<String> {
    serde_json::to_string(v)
        .map_err(|e| FamiliarError::Database(format!("failed to serialize JSON array: {e}")))
}

pub(crate) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}
pub mod accounting;
