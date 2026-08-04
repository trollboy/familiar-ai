use async_trait::async_trait;

use crate::error::LlmError;
use crate::types::HealthStatus;

/// Dumb adapter trait. Backends should not own lifecycle state — the
/// `LlmManager` handles that. Each method is a single operation.
#[async_trait]
pub trait LlmBackend: Send + Sync {
    fn name(&self) -> &'static str;

    async fn health_check(&self) -> Result<HealthStatus, LlmError>;

    async fn summarize(&self, input: &str, max_tokens: usize) -> Result<String, LlmError>;

    async fn classify(&self, input: &str, labels: &[String]) -> Result<String, LlmError>;

    async fn embed(&self, input: &str) -> Result<Vec<f32>, LlmError>;
}
