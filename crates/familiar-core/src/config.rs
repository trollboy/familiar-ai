use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use crate::FamiliarError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub tray: TrayConfig,
    #[serde(default)]
    pub summary: SummaryConfig,
    #[serde(default)]
    pub rollup: RollupConfig,
    #[serde(default)]
    pub packer: PackerConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub execution_history: ExecutionHistoryConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionHistoryConfig {
    #[serde(default)]
    pub pricing: BTreeMap<String, ExecutionPrice>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionPrice {
    pub input_microusd_per_million: Option<u64>,
    pub cached_input_microusd_per_million: Option<u64>,
    pub output_microusd_per_million: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub pid_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,
}

fn default_heartbeat_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub format: LogFormat,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<PathBuf>,
}

impl DatabaseConfig {
    pub fn resolve_path(&self, data_dir: &Path) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| data_dir.join("familiar.db"))
    }
}

// --- Inference config (replaces old LlmConfig) ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    #[default]
    Disabled,
    LocalOnly,
    RemoteOnly,
    Hybrid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Disabled,
    Builtin,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInferenceConfig {
    #[serde(default)]
    pub mode: InferenceMode,
    #[serde(default)]
    pub provider: ProviderKind,
    #[serde(default = "default_builtin_text_model")]
    pub builtin_model: String,
    #[serde(default = "default_builtin_url")]
    pub builtin_url: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    #[serde(default)]
    pub prefer_privacy: bool,
    #[serde(default = "default_true")]
    pub prefer_cost_savings: bool,
    #[serde(default = "default_inference_max_input_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_inference_max_concurrent")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_inference_timeout_secs")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingInferenceConfig {
    #[serde(default)]
    pub provider: ProviderKind,
    #[serde(default = "default_builtin_embedding_model")]
    pub builtin_model: String,
    #[serde(default = "default_builtin_url")]
    pub builtin_url: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    #[serde(default = "default_inference_max_concurrent")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_inference_timeout_secs")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceConfig {
    #[serde(default)]
    pub text: TextInferenceConfig,
    #[serde(default)]
    pub embedding: EmbeddingInferenceConfig,
}

fn default_builtin_text_model() -> String {
    "qwen2.5:3b".to_string()
}

fn default_builtin_embedding_model() -> String {
    "nomic-embed-text".to_string()
}

fn default_builtin_url() -> String {
    "http://127.0.0.1:11434/v1".to_string()
}

fn default_true() -> bool {
    true
}

fn default_inference_max_input_chars() -> usize {
    16_000
}

fn default_inference_max_concurrent() -> usize {
    4
}

fn default_inference_timeout_secs() -> u64 {
    60
}

impl Default for TextInferenceConfig {
    fn default() -> Self {
        Self {
            mode: InferenceMode::default(),
            provider: ProviderKind::default(),
            builtin_model: default_builtin_text_model(),
            builtin_url: default_builtin_url(),
            remote_url: None,
            remote_api_key: None,
            fallback_enabled: true,
            prefer_privacy: false,
            prefer_cost_savings: true,
            max_input_chars: default_inference_max_input_chars(),
            max_concurrent_requests: default_inference_max_concurrent(),
            request_timeout_secs: default_inference_timeout_secs(),
        }
    }
}

impl Default for EmbeddingInferenceConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::default(),
            builtin_model: default_builtin_embedding_model(),
            builtin_url: default_builtin_url(),
            remote_url: None,
            remote_api_key: None,
            fallback_enabled: true,
            max_concurrent_requests: default_inference_max_concurrent(),
            request_timeout_secs: default_inference_timeout_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_watcher_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_ignore_patterns")]
    pub ignore_patterns: Vec<String>,
    #[serde(default = "default_respect_gitignore")]
    pub respect_gitignore: bool,
}

fn default_watcher_enabled() -> bool {
    true
}

fn default_debounce_ms() -> u64 {
    1000
}

fn default_ignore_patterns() -> Vec<String> {
    vec!["target/**".into(), "node_modules/**".into()]
}

fn default_respect_gitignore() -> bool {
    true
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: default_watcher_enabled(),
            paths: Vec::new(),
            debounce_ms: default_debounce_ms(),
            ignore_patterns: default_ignore_patterns(),
            respect_gitignore: default_respect_gitignore(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayConfig {
    #[serde(default = "default_tray_enabled")]
    pub enabled: bool,
    #[serde(default = "default_recent_projects_count")]
    pub recent_projects_count: usize,
}

fn default_tray_enabled() -> bool {
    true
}

fn default_recent_projects_count() -> usize {
    5
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            enabled: default_tray_enabled(),
            recent_projects_count: default_recent_projects_count(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryConfig {
    #[serde(default = "default_summary_enabled")]
    pub enabled: bool,
    #[serde(default = "default_staleness_threshold_secs")]
    pub staleness_threshold_secs: u64,
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: u64,
    #[serde(default = "default_max_pending_files")]
    pub max_pending_files: usize,
    #[serde(default = "default_per_file_quiet_ms")]
    pub per_file_quiet_ms: u64,
}

fn default_summary_enabled() -> bool {
    true
}

fn default_staleness_threshold_secs() -> u64 {
    86_400 // 24h
}

fn default_flush_interval_secs() -> u64 {
    3
}

fn default_max_file_size_bytes() -> u64 {
    1_048_576 // 1 MB
}

fn default_max_pending_files() -> usize {
    10_000
}

fn default_per_file_quiet_ms() -> u64 {
    1_500
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            enabled: default_summary_enabled(),
            staleness_threshold_secs: default_staleness_threshold_secs(),
            flush_interval_secs: default_flush_interval_secs(),
            max_file_size_bytes: default_max_file_size_bytes(),
            max_pending_files: default_max_pending_files(),
            per_file_quiet_ms: default_per_file_quiet_ms(),
        }
    }
}

// TODO: future PRDs may want per-project summary settings (e.g., custom
// max_file_size or custom ignore patterns) overriding the global SummaryConfig.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupConfig {
    #[serde(default = "default_rollup_enabled")]
    pub enabled: bool,
    #[serde(default = "default_rollup_default_limit")]
    pub default_limit: usize,
    #[serde(default = "default_max_rollup_tokens")]
    pub max_rollup_tokens: usize,
    #[serde(default = "default_max_rollup_chars")]
    pub max_rollup_chars: usize,
}

fn default_rollup_enabled() -> bool {
    true
}

fn default_rollup_default_limit() -> usize {
    20
}

fn default_max_rollup_tokens() -> usize {
    4000
}

fn default_max_rollup_chars() -> usize {
    50_000
}

impl Default for RollupConfig {
    fn default() -> Self {
        Self {
            enabled: default_rollup_enabled(),
            default_limit: default_rollup_default_limit(),
            max_rollup_tokens: default_max_rollup_tokens(),
            max_rollup_chars: default_max_rollup_chars(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetProfile {
    Minimal,
    #[default]
    Balanced,
    Aggressive,
    MaxAccuracy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackerConfig {
    #[serde(default)]
    pub default_profile: BudgetProfile,
    #[serde(default = "default_packer_hard_ceiling")]
    pub hard_ceiling_tokens: usize,
}

fn default_packer_hard_ceiling() -> usize {
    15_000
}

impl Default for PackerConfig {
    fn default() -> Self {
        Self {
            default_profile: BudgetProfile::default(),
            hard_ceiling_tokens: default_packer_hard_ceiling(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dashboard_bind")]
    pub bind_address: String,
}

fn default_dashboard_bind() -> String {
    "127.0.0.1:9400".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_dashboard_bind(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: None,
            socket_path: None,
            heartbeat_interval_secs: default_heartbeat_interval(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            format: LogFormat::default(),
        }
    }
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> crate::Result<Self> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }

        figment = figment.merge(Env::prefixed("FAMILIAR_").split("__"));

        figment
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))
    }

    pub fn load_with_overrides(
        config_path: Option<&Path>,
        overrides: figment::Figment,
    ) -> crate::Result<Self> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }

        figment = figment.merge(Env::prefixed("FAMILIAR_").split("__"));
        figment = figment.merge(overrides);

        figment
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let config = Config::default();
        assert_eq!(config.daemon.heartbeat_interval_secs, 60);
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.logging.format, LogFormat::Pretty);
        // inference defaults to disabled
        assert!(config.daemon.pid_file.is_none());
        assert!(config.daemon.socket_path.is_none());
        assert!(config.database.path.is_none());
    }

    #[test]
    fn load_without_file_succeeds() {
        // Note: actual values may differ from defaults if FAMILIAR_ env vars are set
        let config = Config::load(None);
        assert!(config.is_ok());
    }

    #[test]
    fn load_from_toml_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[logging]
level = "debug"
format = "json"
"#,
        )
        .unwrap();

        let config = Config::load(Some(tmp.path())).unwrap();
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.logging.format, LogFormat::Json);
    }

    #[test]
    fn loads_exact_model_execution_pricing() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[execution_history.pricing."exact-model"]
input_microusd_per_million = 100
cached_input_microusd_per_million = 20
output_microusd_per_million = 300
"#,
        )
        .unwrap();
        let config = Config::load(Some(tmp.path())).unwrap();
        let price = &config.execution_history.pricing["exact-model"];
        assert_eq!(price.input_microusd_per_million, Some(100));
        assert_eq!(price.cached_input_microusd_per_million, Some(20));
        assert_eq!(price.output_microusd_per_million, Some(300));
    }

    #[test]
    fn env_overrides_file() {
        // Use INFERENCE__TEXT__MODE to test env override with new config
        let env_key = "FAMILIAR_INFERENCE__TEXT__MODE";
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        std::env::set_var(env_key, "local_only");
        let config = Config::load(Some(tmp.path())).unwrap();
        std::env::remove_var(env_key);

        assert_eq!(config.inference.text.mode, InferenceMode::LocalOnly);
    }

    #[test]
    fn load_with_overrides_takes_priority() {
        let overrides = Figment::from(Serialized::defaults(Config {
            logging: LoggingConfig {
                level: "trace".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }));

        let config = Config::load_with_overrides(None, overrides).unwrap();
        assert_eq!(config.logging.level, "trace");
    }

    #[test]
    fn missing_config_file_does_not_error() {
        let config = Config::load(Some(Path::new("/nonexistent/config.toml")));
        assert!(config.is_ok());
    }
}
