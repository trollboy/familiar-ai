//! Stub backend — no network, no model, always healthy.
//!
//! Used in tests, in headless Docker builds, and as the default when no
//! real LLM is configured. All responses are deterministic placeholders
//! clearly marked as stub output so nothing downstream mistakes them for
//! real inference.

use async_trait::async_trait;

use crate::backend::LlmBackend;
use crate::error::LlmError;
use crate::types::HealthStatus;

/// Fixed stub embedding length. This is a stub-only convention — real
/// providers return 384/768/1024/1536/3072 etc. Nothing else in the
/// workspace should hardcode an expected embedding dimension.
const STUB_EMBEDDING_LEN: usize = 384;

const STUB_SUMMARY_PREVIEW_CHARS: usize = 200;

pub struct StubBackend;

#[async_trait]
impl LlmBackend for StubBackend {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn health_check(&self) -> Result<HealthStatus, LlmError> {
        Ok(HealthStatus::Healthy)
    }

    async fn summarize(&self, input: &str, _max_tokens: usize) -> Result<String, LlmError> {
        let preview: String = input.chars().take(STUB_SUMMARY_PREVIEW_CHARS).collect();
        Ok(format!("[stub summary] {preview}"))
    }

    async fn classify(&self, _input: &str, labels: &[String]) -> Result<String, LlmError> {
        labels
            .first()
            .cloned()
            .ok_or_else(|| LlmError::Config("classify requires at least one label".into()))
    }

    async fn embed(&self, _input: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; STUB_EMBEDDING_LEN])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn name_is_stub() {
        assert_eq!(StubBackend.name(), "stub");
    }

    #[tokio::test]
    async fn health_always_ok() {
        let health = StubBackend.health_check().await.unwrap();
        assert!(health.is_healthy());
    }

    #[tokio::test]
    async fn summarize_short_input() {
        let out = StubBackend.summarize("hello world", 100).await.unwrap();
        assert!(out.starts_with("[stub summary]"));
        assert!(out.contains("hello world"));
    }

    #[tokio::test]
    async fn summarize_long_input_is_clipped() {
        let long = "x".repeat(500);
        let out = StubBackend.summarize(&long, 100).await.unwrap();
        // "[stub summary] " prefix + 200 chars
        assert!(out.len() < long.len());
        assert!(out.starts_with("[stub summary]"));
    }

    #[tokio::test]
    async fn classify_returns_first_label() {
        let labels = vec!["alpha".to_string(), "beta".to_string()];
        let out = StubBackend.classify("any text", &labels).await.unwrap();
        assert_eq!(out, "alpha");
    }

    #[tokio::test]
    async fn classify_empty_labels_errors() {
        let result = StubBackend.classify("text", &[]).await;
        assert!(matches!(result, Err(LlmError::Config(_))));
    }

    #[tokio::test]
    async fn embed_returns_stub_dimension() {
        let v = StubBackend.embed("anything").await.unwrap();
        assert_eq!(v.len(), STUB_EMBEDDING_LEN);
        assert!(v.iter().all(|&x| x == 0.0));
    }
}
