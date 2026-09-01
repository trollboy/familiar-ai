use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ProviderConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompressionConfig {
    /// Output register identity by stage (`implementation`, `review`, or
    /// `remediation`). Empty means no output register anywhere.
    #[serde(default)]
    pub output_registers: BTreeMap<String, String>,
    /// Input transform identity by provider. Empty means no provider input is
    /// transformed. Adapter wiring is deliberately reserved for PRD-058–061.
    #[serde(default)]
    pub input_providers: BTreeMap<String, String>,
    #[serde(default)]
    pub experiment_label: Option<String>,
    #[serde(default)]
    pub experiment_lane: Option<String>,
}

impl CompressionConfig {
    pub fn validate(&self, providers: &BTreeMap<String, ProviderConfig>) -> Result<(), String> {
        if self.experiment_label.is_some() != self.experiment_lane.is_some() {
            return Err(
                "compression experiment_label and experiment_lane must be set together".into(),
            );
        }
        if let Some(lane) = &self.experiment_lane {
            if !matches!(lane.as_str(), "off" | "on") {
                return Err("compression experiment_lane must be 'off' or 'on'".into());
            }
        }
        for (stage, register) in &self.output_registers {
            if !matches!(stage.as_str(), "implementation" | "review" | "remediation") {
                return Err(format!("compression output stage '{stage}' is unknown"));
            }
            if register != "compact" {
                return Err(format!(
                    "compression output register '{register}' is unknown"
                ));
            }
        }
        for (provider, transform) in &self.input_providers {
            if !providers.contains_key(provider) {
                return Err(format!(
                    "compression input provider '{provider}' is not configured"
                ));
            }
            if transform != "native-rle" {
                return Err(format!(
                    "compression input transform '{transform}' is unknown"
                ));
            }
        }
        Ok(())
    }
}
