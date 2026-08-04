pub mod backlog;
pub mod bootstrap;
pub mod decision;
pub mod execution_history;
pub mod file_summary;
pub mod lifecycle;
pub mod project;
pub mod review;
pub mod session_rollup;
pub mod stats;

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
