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

/// PRD-053 reconciliation policy. Both fields are operator policy, not
/// provider guarantees, and are recorded exactly on every reconciliation run
/// for audit even as the configured default changes over time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationConfig {
    /// Exact nanoUSD variance per source-window within which an
    /// authoritative/local mismatch classifies as `reconciled-with-variance`
    /// rather than `mismatch`. Default: one cent (10,000,000 nanoUSD).
    pub tolerance_nanousd: u64,
    /// Daily buckets past a window's end after which a local estimate with
    /// no matching provider cost stops being `pending` and becomes an
    /// explicit `mismatch`. Default: three days.
    pub settlement_horizon_days: u32,
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        Self {
            tolerance_nanousd: 10_000_000,
            settlement_horizon_days: 3,
        }
    }
}
