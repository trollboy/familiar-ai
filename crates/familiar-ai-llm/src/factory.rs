use std::sync::Arc;

use crate::backend::LlmBackend;
use crate::backends::openai_http::{OpenAiHttpBackend, OpenAiHttpConfig};
use crate::backends::stub::StubBackend;
use crate::error::LlmError;

/// Parameters needed to construct any LLM backend.
/// Extracted from InferenceConfig by the router; decouples the backend layer
/// from the config layer.
#[derive(Debug, Clone)]
pub struct BackendParams {
    pub url: String,
    pub model_name: String,
    pub api_key: Option<String>,
    pub max_concurrent_requests: usize,
    pub request_timeout_secs: u64,
    pub max_input_chars: usize,
}

/// Build a stub backend (no network, always healthy).
pub(crate) fn build_stub() -> Arc<dyn LlmBackend> {
    Arc::new(StubBackend)
}

/// Build an OpenAI-compatible HTTP backend from the given params.
pub(crate) fn build_http(params: &BackendParams) -> Result<Arc<dyn LlmBackend>, LlmError> {
    if params.url.is_empty() {
        return Err(LlmError::Config("url required".into()));
    }
    if params.model_name.is_empty() {
        return Err(LlmError::Config("model_name required".into()));
    }
    let http_config = OpenAiHttpConfig {
        base_url: params.url.clone(),
        model_name: params.model_name.clone(),
        api_key: params.api_key.clone(),
        max_concurrent_requests: params.max_concurrent_requests,
        request_timeout_secs: params.request_timeout_secs,
        max_input_chars: params.max_input_chars,
    };
    Ok(Arc::new(OpenAiHttpBackend::new(http_config)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_params() -> BackendParams {
        BackendParams {
            url: "http://127.0.0.1:11434/v1".into(),
            model_name: "test-model".into(),
            api_key: None,
            max_concurrent_requests: 4,
            request_timeout_secs: 60,
            max_input_chars: 16_000,
        }
    }

    #[test]
    fn build_stub_succeeds() {
        let backend = build_stub();
        assert_eq!(backend.name(), "stub");
    }

    #[test]
    fn build_http_without_url_errors() {
        let mut params = default_params();
        params.url = "".into();
        let result = build_http(&params);
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[test]
    fn build_http_without_model_errors() {
        let mut params = default_params();
        params.model_name = "".into();
        let result = build_http(&params);
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[test]
    fn build_http_success() {
        let params = default_params();
        let backend = build_http(&params).unwrap();
        assert_eq!(backend.name(), "openai-http");
    }
}
