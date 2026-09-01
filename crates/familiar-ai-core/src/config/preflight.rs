use serde::{Deserialize, Serialize};

/// Deterministic prerequisites checked before any PRD is claimed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightConfig {
    #[serde(default)]
    pub commands: Vec<PreflightCommandConfig>,
    /// Environment variable names whose values must exist and be non-empty.
    /// Values are never persisted or printed.
    #[serde(default)]
    pub required_environment: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightCommandConfig {
    pub check_id: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub working_directory: String,
}
