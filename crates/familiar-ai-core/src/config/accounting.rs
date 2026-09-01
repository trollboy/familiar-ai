use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHistoryConfig {
    #[serde(default)]
    pub pricing: BTreeMap<String, ExecutionPrice>,
    /// Append-only schedule declarations. A changed price uses a new key.
    #[serde(default)]
    pub price_schedules: BTreeMap<String, PriceScheduleConfig>,
    #[serde(default)]
    pub subscriptions: BTreeMap<String, SubscriptionDeclarationConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPrice {
    pub input_microusd_per_million: Option<u64>,
    pub cached_input_microusd_per_million: Option<u64>,
    pub output_microusd_per_million: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceScheduleConfig {
    pub effective_at: String,
    pub currency: PriceCurrency,
    pub calculation_version: String,
    pub models: BTreeMap<String, PriceScheduleRateConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PriceCurrency {
    USD,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceScheduleRateConfig {
    pub uncached_input_nanousd_per_million: Option<u64>,
    pub cache_read_nanousd_per_million: Option<u64>,
    pub cache_write_nanousd_per_million: Option<u64>,
    pub output_nanousd_per_million: Option<u64>,
    pub reasoning_output_nanousd_per_million: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionDeclarationConfig {
    pub available: bool,
    pub price_nanousd: Option<u64>,
    pub actor: String,
    pub declared_at: String,
}
