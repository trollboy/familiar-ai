use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ReviewVerificationConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AssignmentsConfig {
    #[serde(default)]
    pub verification: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckConfig {
    pub argv: Vec<String>,
    #[serde(default = "default_working_directory")]
    pub working_directory: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub path_prefixes: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckOverrideConfig {
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub environment: Option<BTreeMap<String, String>>,
}

impl CheckConfig {
    pub fn from_legacy(value: &ReviewVerificationConfig) -> Self {
        Self {
            argv: value.argv.clone(),
            working_directory: value.working_directory.clone(),
            timeout_ms: value.timeout_ms,
            required: value.required,
            path_prefixes: value.path_prefixes.clone(),
            environment: value.environment.clone(),
        }
    }

    pub fn resolve(
        &self,
        name: &str,
        override_: Option<&CheckOverrideConfig>,
    ) -> ReviewVerificationConfig {
        ReviewVerificationConfig {
            check_id: name.to_owned(),
            argv: self.argv.clone(),
            working_directory: override_
                .and_then(|value| value.working_directory.clone())
                .unwrap_or_else(|| self.working_directory.clone()),
            timeout_ms: override_
                .and_then(|value| value.timeout_ms)
                .unwrap_or(self.timeout_ms),
            required: override_
                .and_then(|value| value.required)
                .unwrap_or(self.required),
            path_prefixes: self.path_prefixes.clone(),
            environment: override_
                .and_then(|value| value.environment.clone())
                .unwrap_or_else(|| self.environment.clone()),
        }
    }
}

const fn default_timeout_ms() -> u64 {
    300_000
}

fn default_working_directory() -> String {
    ".".into()
}

const fn default_true() -> bool {
    true
}
