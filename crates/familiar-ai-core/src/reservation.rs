use serde::{Deserialize, Serialize};

/// A finite capacity denomination. Custom values let later PRDs add resources
/// without changing the reservation lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceType {
    NanousdBudget,
    UncachedTokens,
    TotalTokens,
    AcceleratorMemory,
    SystemMemory,
    InferenceSlots,
    ModelLoadingSlots,
    ExclusiveRuntime,
    #[serde(untagged)]
    Custom(String),
}

impl ResourceType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::NanousdBudget => "nanousd-budget",
            Self::UncachedTokens => "uncached-tokens",
            Self::TotalTokens => "total-tokens",
            Self::AcceleratorMemory => "accelerator-memory",
            Self::SystemMemory => "system-memory",
            Self::InferenceSlots => "inference-slots",
            Self::ModelLoadingSlots => "model-loading-slots",
            Self::ExclusiveRuntime => "exclusive-runtime",
            Self::Custom(value) => value,
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "nanousd-budget" => Self::NanousdBudget,
            "uncached-tokens" => Self::UncachedTokens,
            "total-tokens" => Self::TotalTokens,
            "accelerator-memory" => Self::AcceleratorMemory,
            "system-memory" => Self::SystemMemory,
            "inference-slots" => Self::InferenceSlots,
            "model-loading-slots" => Self::ModelLoadingSlots,
            "exclusive-runtime" => Self::ExclusiveRuntime,
            other => Self::Custom(other.to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationOwnerIdentity {
    pub owner_instance_id: String,
    pub installation_id: Option<String>,
    pub nonce_or_generation: String,
    pub owner_kind: String,
    pub project_id: String,
    pub execution_id: String,
    pub component_id: String,
}

impl ReservationOwnerIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if self.owner_instance_id.is_empty()
            || self.nonce_or_generation.is_empty()
            || self.owner_kind.is_empty()
            || self.project_id.is_empty()
            || self.execution_id.is_empty()
            || self.component_id.is_empty()
        {
            return Err("reservation owner identity and attribution must be non-empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRequest {
    pub pool_id: String,
    pub resource_type: ResourceType,
    pub amount: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantMode {
    AllOrNothing,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownConsumptionPolicy {
    HoldReservation,
    SettleReservedAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnerLiveness {
    Live,
    ProvablyDead,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerLivenessEvidence {
    pub owner_instance_id: String,
    pub nonce_or_generation: String,
    pub resolution: OwnerLiveness,
    pub provenance: String,
    pub observed_at: String,
}
