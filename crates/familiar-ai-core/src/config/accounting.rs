use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::review::ReviewConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContextConfig {
    pub hard_ceiling_tokens: Option<u64>,
    #[serde(default)]
    pub repository_map_enabled: bool,
    /// Enables adapter-specific prompt cache controls. Unsupported adapters
    /// retain their native behavior and receive no fabricated flags.
    #[serde(default = "default_prompt_cache_enabled")]
    pub prompt_cache_enabled: bool,
}

const fn default_prompt_cache_enabled() -> bool {
    true
}

impl Default for ExecutionContextConfig {
    fn default() -> Self {
        Self {
            hard_ceiling_tokens: None,
            repository_map_enabled: false,
            prompt_cache_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationSource {
    Global,
    Repository,
}

impl ConfigurationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Repository => "repository",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveExecutionConfig {
    pub review: ReviewConfig,
    pub review_source: ConfigurationSource,
    pub execution_context: ExecutionContextConfig,
    pub execution_context_source: ConfigurationSource,
}

/// The canonical Git common-directory identity of a worktree — the same key
/// `FilesystemBacklogDiscovery::resolve` computes — or None when the path is
/// not inside a Git repository (or git is unavailable).
pub(crate) fn git_common_directory(path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let canonical = Path::new(value.trim()).canonicalize().ok()?;
    Some(canonical.to_str()?.replace('\\', "/"))
}

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
