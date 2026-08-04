// Intentionally targets the OpenAI-compatible API surface used by Ollama,
// LM Studio, llama.cpp server, vLLM, OpenRouter-compatible gateways, etc.
// This is not tied to OpenAI itself — do not rename to GenericHttpBackend.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Client, StatusCode};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::backend::LlmBackend;
use crate::error::LlmError;
use crate::types::HealthStatus;

pub struct OpenAiHttpBackend {
    base_url: String,
    model_name: String,
    api_key: Option<String>,
    max_input_chars: usize,
    client: Client,
    limiter: Arc<Semaphore>,
}

pub struct OpenAiHttpConfig {
    pub base_url: String,
    pub model_name: String,
    pub api_key: Option<String>,
    pub max_concurrent_requests: usize,
    pub request_timeout_secs: u64,
    pub max_input_chars: usize,
}

impl OpenAiHttpBackend {
    pub fn new(config: OpenAiHttpConfig) -> Result<Self, LlmError> {
        if config.base_url.is_empty() {
            return Err(LlmError::Config("endpoint_url required".into()));
        }
        if config.model_name.is_empty() {
            return Err(LlmError::Config("model_name required".into()));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|e| LlmError::Config(format!("failed to build HTTP client: {e}")))?;

        let limit = config.max_concurrent_requests.max(1);
        Ok(Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            model_name: config.model_name,
            api_key: config.api_key,
            max_input_chars: config.max_input_chars,
            client,
            limiter: Arc::new(Semaphore::new(limit)),
        })
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(key) = &self.api_key {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(AUTHORIZATION, value);
            }
        }
        headers
    }

    fn clamp_input(&self, input: &str) -> String {
        if input.chars().count() <= self.max_input_chars {
            input.to_string()
        } else {
            let clamped: String = input.chars().take(self.max_input_chars).collect();
            format!("{clamped} ... [input truncated]")
        }
    }

    fn map_request_error(err: reqwest::Error) -> LlmError {
        if err.is_timeout() {
            LlmError::Timeout
        } else if err.is_connect() {
            LlmError::Transport("connection refused".into())
        } else {
            LlmError::Transport(err.to_string())
        }
    }
}

#[async_trait]
impl LlmBackend for OpenAiHttpBackend {
    fn name(&self) -> &'static str {
        "openai-http"
    }

    async fn health_check(&self) -> Result<HealthStatus, LlmError> {
        let url = format!("{}/models", self.base_url);
        let _permit = self.limiter.acquire().await.ok();
        let request = self.client.get(&url).headers(self.auth_headers());

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    return Ok(HealthStatus::Unavailable("timeout".into()));
                } else if e.is_connect() {
                    return Ok(HealthStatus::Unavailable("connection refused".into()));
                } else {
                    return Ok(HealthStatus::Unavailable(e.to_string()));
                }
            }
        };

        let status = response.status();
        let result = match status {
            StatusCode::OK => HealthStatus::Healthy,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                HealthStatus::Unavailable("authentication failed".into())
            }
            StatusCode::NOT_FOUND => HealthStatus::Unavailable("models endpoint not found".into()),
            StatusCode::TOO_MANY_REQUESTS => HealthStatus::Degraded("rate limited".into()),
            s if s.is_server_error() => HealthStatus::Unavailable(format!("server error: {s}")),
            s => HealthStatus::Unavailable(format!("unexpected status: {s}")),
        };
        Ok(result)
    }

    async fn summarize(&self, input: &str, max_tokens: usize) -> Result<String, LlmError> {
        let clamped = self.clamp_input(input);
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model_name,
            "messages": [
                {
                    "role": "user",
                    "content": format!(
                        "Summarize the following in at most {max_tokens} tokens:\n\n{clamped}"
                    )
                }
            ],
            "max_tokens": max_tokens,
        });

        let _permit = self.limiter.acquire().await.ok();
        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(Self::map_request_error)?;

        if !response.status().is_success() {
            return Err(LlmError::Backend(format!(
                "summarize request failed: {}",
                response.status()
            )));
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| LlmError::Backend(format!("failed to parse response: {e}")))?;

        value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::Backend("response missing choices[0].message.content".into()))
    }

    async fn classify(&self, input: &str, labels: &[String]) -> Result<String, LlmError> {
        if labels.is_empty() {
            return Err(LlmError::Config(
                "classify requires at least one label".into(),
            ));
        }
        let clamped = self.clamp_input(input);
        let url = format!("{}/chat/completions", self.base_url);
        let labels_str = labels.join(", ");
        let body = json!({
            "model": self.model_name,
            "messages": [
                {
                    "role": "user",
                    "content": format!(
                        "Classify the following text into exactly one of these labels: [{labels_str}]. Respond with only the label.\n\n{clamped}"
                    )
                }
            ],
            "max_tokens": 16,
        });

        let _permit = self.limiter.acquire().await.ok();
        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(Self::map_request_error)?;

        if !response.status().is_success() {
            return Err(LlmError::Backend(format!(
                "classify request failed: {}",
                response.status()
            )));
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| LlmError::Backend(format!("failed to parse response: {e}")))?;

        let raw = value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| LlmError::Backend("response missing content".into()))?
            .trim()
            .to_string();

        // Exact match
        if let Some(m) = labels.iter().find(|l| l.eq_ignore_ascii_case(&raw)) {
            return Ok(m.clone());
        }
        // Substring match
        if let Some(m) = labels
            .iter()
            .find(|l| raw.to_lowercase().contains(&l.to_lowercase()))
        {
            return Ok(m.clone());
        }
        // Fallback to first label
        Ok(labels[0].clone())
    }

    async fn embed(&self, input: &str) -> Result<Vec<f32>, LlmError> {
        let clamped = self.clamp_input(input);
        let url = format!("{}/embeddings", self.base_url);
        let body = json!({
            "model": self.model_name,
            "input": clamped,
        });

        let _permit = self.limiter.acquire().await.ok();
        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(Self::map_request_error)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(LlmError::Backend("embeddings not supported".into()));
        }
        if !response.status().is_success() {
            return Err(LlmError::Backend(format!(
                "embed request failed: {}",
                response.status()
            )));
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| LlmError::Backend(format!("failed to parse response: {e}")))?;

        let embedding = value
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("embedding"))
            .and_then(|e| e.as_array())
            .ok_or_else(|| LlmError::Backend("response missing data[0].embedding".into()))?;

        let vec: Result<Vec<f32>, _> = embedding
            .iter()
            .map(|v| {
                v.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| LlmError::Backend("non-numeric embedding element".into()))
            })
            .collect();
        vec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn default_config(base_url: String) -> OpenAiHttpConfig {
        OpenAiHttpConfig {
            base_url,
            model_name: "test-model".into(),
            api_key: None,
            max_concurrent_requests: 4,
            request_timeout_secs: 5,
            max_input_chars: 16_000,
        }
    }

    #[tokio::test]
    async fn construction_requires_base_url() {
        let mut cfg = default_config("".into());
        cfg.base_url = "".into();
        let result = OpenAiHttpBackend::new(cfg);
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[tokio::test]
    async fn construction_requires_model_name() {
        let mut cfg = default_config("http://localhost".into());
        cfg.model_name = "".into();
        let result = OpenAiHttpBackend::new(cfg);
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[tokio::test]
    async fn health_check_200_is_healthy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn health_check_401_is_auth_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(matches!(health, HealthStatus::Unavailable(msg) if msg.contains("authentication")));
    }

    #[tokio::test]
    async fn health_check_403_is_auth_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(matches!(health, HealthStatus::Unavailable(msg) if msg.contains("authentication")));
    }

    #[tokio::test]
    async fn health_check_404_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(
            matches!(health, HealthStatus::Unavailable(msg) if msg.contains("models endpoint"))
        );
    }

    #[tokio::test]
    async fn health_check_429_is_degraded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(matches!(health, HealthStatus::Degraded(msg) if msg.contains("rate limited")));
    }

    #[tokio::test]
    async fn health_check_500_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(matches!(health, HealthStatus::Unavailable(msg) if msg.contains("server error")));
    }

    #[tokio::test]
    async fn health_check_connection_refused() {
        // Unused port
        let backend = OpenAiHttpBackend::new(default_config("http://127.0.0.1:1".into())).unwrap();
        let health = backend.health_check().await.unwrap();
        assert!(matches!(health, HealthStatus::Unavailable(_)));
    }

    #[tokio::test]
    async fn summarize_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "short summary"}
                }]
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let out = backend.summarize("long input", 100).await.unwrap();
        assert_eq!(out, "short summary");
    }

    #[tokio::test]
    async fn summarize_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let result = backend.summarize("input", 100).await;
        assert!(matches!(result, Err(LlmError::Backend(_))));
    }

    #[tokio::test]
    async fn summarize_clamps_long_input() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "ok"}}]
            })))
            .mount(&server)
            .await;

        let mut cfg = default_config(server.uri());
        cfg.max_input_chars = 50;
        let backend = OpenAiHttpBackend::new(cfg).unwrap();
        let huge = "x".repeat(10_000);
        let result = backend.summarize(&huge, 100).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn classify_returns_exact_label_match() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "beta"}}]
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let labels = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let out = backend.classify("text", &labels).await.unwrap();
        assert_eq!(out, "beta");
    }

    #[tokio::test]
    async fn classify_falls_back_on_unrecognized_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "wat"}}]
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let labels = vec!["alpha".to_string(), "beta".to_string()];
        let out = backend.classify("text", &labels).await.unwrap();
        assert_eq!(out, "alpha"); // fallback to first
    }

    #[tokio::test]
    async fn classify_substring_match() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "I'd say beta category"}}]
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let labels = vec!["alpha".to_string(), "beta".to_string()];
        let out = backend.classify("text", &labels).await.unwrap();
        assert_eq!(out, "beta");
    }

    #[tokio::test]
    async fn classify_empty_labels_errors() {
        let backend = OpenAiHttpBackend::new(default_config("http://localhost".into())).unwrap();
        let result = backend.classify("text", &[]).await;
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[tokio::test]
    async fn embed_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"embedding": [0.1, 0.2, 0.3, 0.4]}]
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let v = backend.embed("text").await.unwrap();
        assert_eq!(v.len(), 4);
        assert!((v[0] - 0.1).abs() < 1e-6);
    }

    #[tokio::test]
    async fn embed_empty_data_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": []
            })))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let result = backend.embed("text").await;
        assert!(matches!(result, Err(LlmError::Backend(_))));
    }

    #[tokio::test]
    async fn embed_not_found_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let backend = OpenAiHttpBackend::new(default_config(server.uri())).unwrap();
        let result = backend.embed("text").await;
        assert!(matches!(result, Err(LlmError::Backend(msg)) if msg.contains("not supported")));
    }

    #[tokio::test]
    async fn api_key_header_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;

        let mut cfg = default_config(server.uri());
        cfg.api_key = Some("sk-test".into());
        let backend = OpenAiHttpBackend::new(cfg).unwrap();
        let health = backend.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn name_is_openai_http() {
        let backend = OpenAiHttpBackend::new(default_config("http://localhost".into())).unwrap();
        assert_eq!(backend.name(), "openai-http");
    }
}
