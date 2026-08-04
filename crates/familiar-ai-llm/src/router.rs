//! Inference router — sits between consumers and backends.
//!
//! Handles mode selection (disabled/local_only/remote_only/hybrid),
//! fallback logic, and routing decision logging.

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use familiar_ai_core::config::{
    EmbeddingInferenceConfig, InferenceConfig, InferenceMode, ProviderKind, TextInferenceConfig,
};

use crate::error::LlmError;
use crate::factory::BackendParams;
use crate::heuristics::{
    heuristic_importance, heuristic_packer_profile, needs_model, ImportanceScore,
};
use crate::manager::LlmManager;
use crate::types::LlmHealthState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionTestResult {
    pub connected: bool,
    pub status_text: String,
    pub backend_name: Option<String>,
    pub last_error: Option<String>,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterHealthState {
    pub text_mode: String,
    pub text_primary: LlmHealthState,
    pub text_fallback: Option<LlmHealthState>,
    pub embedding_primary: LlmHealthState,
    pub embedding_fallback: Option<LlmHealthState>,
}

pub struct InferenceRouter {
    text_config: Arc<RwLock<TextInferenceConfig>>,
    embedding_config: Arc<RwLock<EmbeddingInferenceConfig>>,
    text_primary: Option<Arc<LlmManager>>,
    text_fallback: Option<Arc<LlmManager>>,
    embed_primary: Option<Arc<LlmManager>>,
    embed_fallback: Option<Arc<LlmManager>>,
}

fn builtin_params(
    url: &str,
    model: &str,
    cfg_concurrent: usize,
    cfg_timeout: u64,
    cfg_max_input: usize,
) -> BackendParams {
    BackendParams {
        url: url.to_string(),
        model_name: model.to_string(),
        api_key: None,
        max_concurrent_requests: cfg_concurrent,
        request_timeout_secs: cfg_timeout,
        max_input_chars: cfg_max_input,
    }
}

fn remote_params(
    url: &str,
    model: &str,
    api_key: Option<&str>,
    concurrent: usize,
    timeout: u64,
    max_input: usize,
) -> BackendParams {
    BackendParams {
        url: url.to_string(),
        model_name: model.to_string(),
        api_key: api_key.map(|s| s.to_string()),
        max_concurrent_requests: concurrent,
        request_timeout_secs: timeout,
        max_input_chars: max_input,
    }
}

impl InferenceRouter {
    pub fn new(config: &InferenceConfig) -> Self {
        let text = &config.text;
        let embed = &config.embedding;

        let (text_primary, text_fallback) = build_text_managers(text);
        let (embed_primary, embed_fallback) = build_embed_managers(embed);

        Self {
            text_config: Arc::new(RwLock::new(text.clone())),
            embedding_config: Arc::new(RwLock::new(embed.clone())),
            text_primary,
            text_fallback,
            embed_primary,
            embed_fallback,
        }
    }

    pub async fn enable(&self) -> Result<(), LlmError> {
        let mut errors = Vec::new();
        for (label, mgr) in self.all_managers() {
            if let Err(e) = mgr.enable().await {
                tracing::warn!(manager = label, error = %e, "failed to enable");
                errors.push(e);
            }
        }
        if errors.is_empty() || self.any_loaded().await {
            Ok(())
        } else {
            Err(errors.remove(0))
        }
    }

    pub async fn disable(&self) {
        for (_, mgr) in self.all_managers() {
            mgr.disable().await;
        }
    }

    pub async fn text_health(&self) -> (LlmHealthState, Option<LlmHealthState>) {
        let primary = match &self.text_primary {
            Some(m) => m.health().await,
            None => LlmHealthState::disabled(),
        };
        let fallback = match &self.text_fallback {
            Some(m) => Some(m.health().await),
            None => None,
        };
        (primary, fallback)
    }

    pub async fn embedding_health(&self) -> (LlmHealthState, Option<LlmHealthState>) {
        let primary = match &self.embed_primary {
            Some(m) => m.health().await,
            None => LlmHealthState::disabled(),
        };
        let fallback = match &self.embed_fallback {
            Some(m) => Some(m.health().await),
            None => None,
        };
        (primary, fallback)
    }

    pub async fn health(&self) -> RouterHealthState {
        let mode = self.text_config.read().await.mode.clone();
        let (tp, tf) = self.text_health().await;
        let (ep, ef) = self.embedding_health().await;
        RouterHealthState {
            text_mode: format!("{mode:?}").to_lowercase(),
            text_primary: tp,
            text_fallback: tf,
            embedding_primary: ep,
            embedding_fallback: ef,
        }
    }

    pub async fn summarize(&self, input: &str, max_tokens: usize) -> Result<String, LlmError> {
        let mode = self.text_config.read().await.mode.clone();
        if mode == InferenceMode::Disabled {
            return Err(LlmError::NotLoaded);
        }
        if !needs_model(input) {
            let preview: String = input.chars().take(200).collect();
            tracing::debug!(
                task = "summarize",
                route = "heuristic",
                reason = "trivial_input",
                "router_decision"
            );
            return Ok(format!("[heuristic] {preview}"));
        }
        let fallback_enabled = self.text_config.read().await.fallback_enabled;

        if let Some(backend) = self.get_text_primary_backend().await {
            tracing::debug!(task = "summarize", route = "primary", "router_decision");
            match backend.summarize(input, max_tokens).await {
                Ok(r) => return Ok(r),
                Err(e) => tracing::warn!(task = "summarize", error = %e, "primary failed"),
            }
        }
        if fallback_enabled {
            if let Some(backend) = self.get_text_fallback_backend().await {
                tracing::debug!(task = "summarize", route = "fallback", "router_decision");
                return backend.summarize(input, max_tokens).await;
            }
        }
        Err(LlmError::NotLoaded)
    }

    pub async fn classify(&self, input: &str, labels: &[String]) -> Result<String, LlmError> {
        let mode = self.text_config.read().await.mode.clone();
        if mode == InferenceMode::Disabled {
            return Err(LlmError::NotLoaded);
        }
        if !needs_model(input) {
            tracing::debug!(
                task = "classify",
                route = "heuristic",
                reason = "trivial_input",
                "router_decision"
            );
            return labels
                .first()
                .cloned()
                .ok_or_else(|| LlmError::Config("no labels".into()));
        }
        let fallback_enabled = self.text_config.read().await.fallback_enabled;

        if let Some(backend) = self.get_text_primary_backend().await {
            tracing::debug!(task = "classify", route = "primary", "router_decision");
            match backend.classify(input, labels).await {
                Ok(r) => return Ok(r),
                Err(e) => tracing::warn!(task = "classify", error = %e, "primary failed"),
            }
        }
        if fallback_enabled {
            if let Some(backend) = self.get_text_fallback_backend().await {
                tracing::debug!(task = "classify", route = "fallback", "router_decision");
                return backend.classify(input, labels).await;
            }
        }
        Err(LlmError::NotLoaded)
    }

    pub async fn embed(&self, input: &str) -> Result<Vec<f32>, LlmError> {
        let embed_provider = self.embedding_config.read().await.provider.clone();
        if embed_provider == ProviderKind::Disabled {
            return Err(LlmError::NotLoaded);
        }
        if !needs_model(input) {
            tracing::debug!(
                task = "embed",
                route = "heuristic",
                reason = "trivial_input",
                "router_decision"
            );
            return Ok(vec![]);
        }
        let fallback_enabled = self.embedding_config.read().await.fallback_enabled;

        if let Some(backend) = self.get_embed_primary_backend().await {
            tracing::debug!(task = "embed", route = "primary", "router_decision");
            match backend.embed(input).await {
                Ok(r) => return Ok(r),
                Err(e) => tracing::warn!(task = "embed", error = %e, "primary failed"),
            }
        }
        if fallback_enabled {
            if let Some(backend) = self.get_embed_fallback_backend().await {
                tracing::debug!(task = "embed", route = "fallback", "router_decision");
                return backend.embed(input).await;
            }
        }
        Err(LlmError::NotLoaded)
    }

    /// Heuristic-based profile selection. Stub for now — returns based on task length.
    pub fn choose_packer_profile(&self, task: &str) -> familiar_ai_core::config::BudgetProfile {
        heuristic_packer_profile(task)
    }

    /// Heuristic-based importance scoring. Stub for now — keyword-based.
    pub fn score_importance(&self, input: &str) -> ImportanceScore {
        heuristic_importance(input)
    }

    pub async fn test_connection(&self, target: &str) -> ConnectionTestResult {
        let mgr = match target {
            "text_primary" => self.text_primary.as_ref(),
            "text_fallback" => self.text_fallback.as_ref(),
            "embed_primary" => self.embed_primary.as_ref(),
            "embed_fallback" => self.embed_fallback.as_ref(),
            _ => None,
        };
        let Some(manager) = mgr else {
            return ConnectionTestResult {
                connected: false,
                status_text: "not configured".into(),
                backend_name: None,
                last_error: None,
                latency_ms: None,
            };
        };
        let start = Instant::now();
        let health = manager.check_health().await;
        let latency = start.elapsed().as_millis() as u64;

        ConnectionTestResult {
            connected: health.healthy,
            status_text: if health.healthy {
                "connected".into()
            } else if health.loaded {
                "degraded".into()
            } else {
                "unreachable".into()
            },
            backend_name: health.backend_name,
            last_error: health.last_error,
            latency_ms: Some(latency),
        }
    }

    // --- private helpers ---

    fn all_managers(&self) -> Vec<(&str, &Arc<LlmManager>)> {
        let mut out = Vec::new();
        if let Some(m) = &self.text_primary {
            out.push(("text_primary", m));
        }
        if let Some(m) = &self.text_fallback {
            out.push(("text_fallback", m));
        }
        if let Some(m) = &self.embed_primary {
            out.push(("embed_primary", m));
        }
        if let Some(m) = &self.embed_fallback {
            out.push(("embed_fallback", m));
        }
        out
    }

    async fn any_loaded(&self) -> bool {
        for (_, mgr) in self.all_managers() {
            if mgr.is_loaded().await {
                return true;
            }
        }
        false
    }

    async fn get_text_primary_backend(&self) -> Option<Arc<dyn crate::backend::LlmBackend>> {
        self.text_primary.as_ref()?.backend().await
    }

    async fn get_text_fallback_backend(&self) -> Option<Arc<dyn crate::backend::LlmBackend>> {
        self.text_fallback.as_ref()?.backend().await
    }

    async fn get_embed_primary_backend(&self) -> Option<Arc<dyn crate::backend::LlmBackend>> {
        self.embed_primary.as_ref()?.backend().await
    }

    async fn get_embed_fallback_backend(&self) -> Option<Arc<dyn crate::backend::LlmBackend>> {
        self.embed_fallback.as_ref()?.backend().await
    }
}

fn build_text_managers(
    text: &TextInferenceConfig,
) -> (Option<Arc<LlmManager>>, Option<Arc<LlmManager>>) {
    match text.mode {
        InferenceMode::Disabled => (None, None),
        InferenceMode::LocalOnly => {
            let primary = Arc::new(LlmManager::new(builtin_params(
                &text.builtin_url,
                &text.builtin_model,
                text.max_concurrent_requests,
                text.request_timeout_secs,
                text.max_input_chars,
            )));
            (Some(primary), None)
        }
        InferenceMode::RemoteOnly => {
            if let Some(ref url) = text.remote_url {
                let primary = Arc::new(LlmManager::new(remote_params(
                    url,
                    &text.builtin_model, // use builtin_model as default model name for remote too
                    text.remote_api_key.as_deref(),
                    text.max_concurrent_requests,
                    text.request_timeout_secs,
                    text.max_input_chars,
                )));
                (Some(primary), None)
            } else {
                tracing::warn!("remote_only mode but no remote_url configured");
                (None, None)
            }
        }
        InferenceMode::Hybrid => {
            let builtin = Arc::new(LlmManager::new(builtin_params(
                &text.builtin_url,
                &text.builtin_model,
                text.max_concurrent_requests,
                text.request_timeout_secs,
                text.max_input_chars,
            )));
            let remote = text.remote_url.as_ref().map(|url| {
                Arc::new(LlmManager::new(remote_params(
                    url,
                    &text.builtin_model,
                    text.remote_api_key.as_deref(),
                    text.max_concurrent_requests,
                    text.request_timeout_secs,
                    text.max_input_chars,
                )))
            });

            match text.provider {
                ProviderKind::Builtin | ProviderKind::Disabled => {
                    // Builtin primary, remote fallback
                    (Some(builtin), remote)
                }
                ProviderKind::Remote => {
                    // Remote primary, builtin fallback
                    (remote, Some(builtin))
                }
            }
        }
    }
}

fn build_embed_managers(
    embed: &EmbeddingInferenceConfig,
) -> (Option<Arc<LlmManager>>, Option<Arc<LlmManager>>) {
    match embed.provider {
        ProviderKind::Disabled => (None, None),
        ProviderKind::Builtin => {
            let primary = Arc::new(LlmManager::new(builtin_params(
                &embed.builtin_url,
                &embed.builtin_model,
                embed.max_concurrent_requests,
                embed.request_timeout_secs,
                16_000, // embeddings don't need max_input_chars from text config
            )));
            let fallback = embed
                .remote_url
                .as_ref()
                .filter(|_| embed.fallback_enabled)
                .map(|url| {
                    Arc::new(LlmManager::new(remote_params(
                        url,
                        &embed.builtin_model,
                        embed.remote_api_key.as_deref(),
                        embed.max_concurrent_requests,
                        embed.request_timeout_secs,
                        16_000,
                    )))
                });
            (Some(primary), fallback)
        }
        ProviderKind::Remote => {
            if let Some(ref url) = embed.remote_url {
                let primary = Arc::new(LlmManager::new(remote_params(
                    url,
                    &embed.builtin_model,
                    embed.remote_api_key.as_deref(),
                    embed.max_concurrent_requests,
                    embed.request_timeout_secs,
                    16_000,
                )));
                let fallback = if embed.fallback_enabled {
                    Some(Arc::new(LlmManager::new(builtin_params(
                        &embed.builtin_url,
                        &embed.builtin_model,
                        embed.max_concurrent_requests,
                        embed.request_timeout_secs,
                        16_000,
                    ))))
                } else {
                    None
                };
                (Some(primary), fallback)
            } else {
                (None, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::config::{
        EmbeddingInferenceConfig, InferenceConfig, TextInferenceConfig,
    };

    fn disabled_config() -> InferenceConfig {
        InferenceConfig::default()
    }

    fn local_only_config() -> InferenceConfig {
        InferenceConfig {
            text: TextInferenceConfig {
                mode: InferenceMode::LocalOnly,
                provider: ProviderKind::Builtin,
                ..Default::default()
            },
            embedding: EmbeddingInferenceConfig {
                provider: ProviderKind::Builtin,
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn disabled_mode_summarize_returns_error() {
        let router = InferenceRouter::new(&disabled_config());
        let result = router
            .summarize("some text to summarize for testing purposes here", 100)
            .await;
        assert!(matches!(result, Err(LlmError::NotLoaded)));
    }

    #[tokio::test]
    async fn disabled_mode_embed_returns_error() {
        let router = InferenceRouter::new(&disabled_config());
        let result = router.embed("some text").await;
        assert!(matches!(result, Err(LlmError::NotLoaded)));
    }

    #[tokio::test]
    async fn trivial_input_uses_heuristic() {
        // Even in local_only mode, trivial input should skip the backend
        let config = local_only_config();
        let router = InferenceRouter::new(&config);
        // Don't enable — we want to verify the heuristic path works without a backend
        let result = router.summarize("hi", 100).await;
        // "hi" is trivial → heuristic path
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with("[heuristic]"));
    }

    #[tokio::test]
    async fn choose_packer_profile_heuristic() {
        let router = InferenceRouter::new(&disabled_config());
        assert_eq!(
            router.choose_packer_profile("fix bug"),
            familiar_ai_core::config::BudgetProfile::Minimal
        );
        let long = "word ".repeat(600);
        assert_eq!(
            router.choose_packer_profile(&long),
            familiar_ai_core::config::BudgetProfile::MaxAccuracy
        );
    }

    #[tokio::test]
    async fn score_importance_heuristic() {
        let router = InferenceRouter::new(&disabled_config());
        assert_eq!(
            router.score_importance("auth token refresh"),
            ImportanceScore::High
        );
        assert_eq!(
            router.score_importance("update readme"),
            ImportanceScore::Low
        );
        assert_eq!(
            router.score_importance("refactor database pool"),
            ImportanceScore::Medium
        );
    }

    #[tokio::test]
    async fn test_connection_not_configured() {
        let router = InferenceRouter::new(&disabled_config());
        let result = router.test_connection("text_primary").await;
        assert!(!result.connected);
        assert_eq!(result.status_text, "not configured");
    }

    #[tokio::test]
    async fn health_disabled_mode() {
        let router = InferenceRouter::new(&disabled_config());
        let health = router.health().await;
        assert_eq!(health.text_mode, "disabled");
        assert!(!health.text_primary.loaded);
    }

    #[tokio::test]
    async fn local_only_builds_primary_no_fallback() {
        let config = local_only_config();
        let router = InferenceRouter::new(&config);
        assert!(router.text_primary.is_some());
        assert!(router.text_fallback.is_none());
    }
}
