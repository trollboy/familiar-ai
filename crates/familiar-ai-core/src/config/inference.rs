use serde::{Deserialize, Serialize};

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

pub(super) fn default_true() -> bool {
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
