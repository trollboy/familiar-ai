use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;

use crate::backend::LlmBackend;
use crate::error::LlmError;
use crate::factory::{build_http, build_stub, BackendParams};
use crate::types::{HealthStatus, LlmHealthState};

/// Owns the lifecycle and health state of a single LLM backend.
/// Backends themselves are dumb adapters; the manager decides when to
/// load, unload, and report health.
///
/// The `InferenceRouter` wraps two of these (one for text, one for
/// embedding) and handles mode/fallback logic.
pub struct LlmManager {
    params: Arc<RwLock<Option<BackendParams>>>,
    backend: Arc<RwLock<Option<Arc<dyn LlmBackend>>>>,
    health: Arc<RwLock<LlmHealthState>>,
    is_stub: bool,
}

impl LlmManager {
    /// Create a manager that will build an HTTP backend from the given params.
    pub fn new(params: BackendParams) -> Self {
        Self {
            params: Arc::new(RwLock::new(Some(params))),
            backend: Arc::new(RwLock::new(None)),
            health: Arc::new(RwLock::new(LlmHealthState::default())),
            is_stub: false,
        }
    }

    /// Create a manager that uses the stub backend (no network).
    pub fn new_stub() -> Self {
        Self {
            params: Arc::new(RwLock::new(None)),
            backend: Arc::new(RwLock::new(None)),
            health: Arc::new(RwLock::new(LlmHealthState::default())),
            is_stub: true,
        }
    }

    /// Attempts to construct and load the configured backend.
    /// Idempotent: calling twice replaces the current backend.
    pub async fn enable(&self) -> Result<(), LlmError> {
        let backend: Arc<dyn LlmBackend> = if self.is_stub {
            build_stub()
        } else {
            let params_guard = self.params.read().await;
            let params = params_guard
                .as_ref()
                .ok_or_else(|| LlmError::Config("no backend params configured".into()))?;
            build_http(params)?
        };

        let backend_name = backend.name().to_string();
        let health_status = backend.health_check().await?;
        let now = Utc::now();

        match health_status {
            HealthStatus::Healthy => {
                *self.backend.write().await = Some(backend);
                *self.health.write().await = LlmHealthState {
                    loaded: true,
                    healthy: true,
                    backend_name: Some(backend_name),
                    last_check: Some(now),
                    last_error: None,
                    loaded_at: Some(now),
                };
                Ok(())
            }
            HealthStatus::Degraded(reason) => {
                // Backend is still loaded; degraded states (e.g. rate limiting)
                // should not prevent loading. backend() must still return Some(...).
                *self.backend.write().await = Some(backend);
                *self.health.write().await = LlmHealthState {
                    loaded: true,
                    healthy: false,
                    backend_name: Some(backend_name),
                    last_check: Some(now),
                    last_error: Some(reason),
                    loaded_at: Some(now),
                };
                Ok(())
            }
            HealthStatus::Unavailable(reason) => {
                *self.backend.write().await = None;
                *self.health.write().await = LlmHealthState {
                    loaded: false,
                    healthy: false,
                    backend_name: Some(backend_name),
                    last_check: Some(now),
                    last_error: Some(reason.clone()),
                    loaded_at: None,
                };
                Err(LlmError::Unhealthy(reason))
            }
        }
    }

    /// Drops the backend instance and resets health state to the neutral
    /// disabled state (not an error).
    pub async fn disable(&self) {
        *self.backend.write().await = None;
        *self.health.write().await = LlmHealthState::disabled();
    }

    pub async fn is_loaded(&self) -> bool {
        self.backend.read().await.is_some()
    }

    pub async fn backend(&self) -> Option<Arc<dyn LlmBackend>> {
        self.backend.read().await.clone()
    }

    /// Runs a health check against the current backend and updates the
    /// health state. If no backend is loaded, returns the neutral disabled
    /// state without populating `last_error`.
    pub async fn check_health(&self) -> LlmHealthState {
        let backend_opt = self.backend.read().await.clone();
        match backend_opt {
            None => {
                let state = LlmHealthState::disabled();
                *self.health.write().await = state.clone();
                state
            }
            Some(backend) => {
                let now = Utc::now();
                let result = backend.health_check().await;
                let name = backend.name().to_string();
                let mut state = self.health.read().await.clone();
                state.last_check = Some(now);
                state.backend_name = Some(name);
                match result {
                    Ok(HealthStatus::Healthy) => {
                        state.loaded = true;
                        state.healthy = true;
                        state.last_error = None;
                    }
                    Ok(HealthStatus::Degraded(reason)) => {
                        state.loaded = true;
                        state.healthy = false;
                        state.last_error = Some(reason);
                    }
                    Ok(HealthStatus::Unavailable(reason)) => {
                        state.loaded = true;
                        state.healthy = false;
                        state.last_error = Some(reason);
                    }
                    Err(e) => {
                        state.loaded = true;
                        state.healthy = false;
                        state.last_error = Some(e.to_string());
                    }
                }
                *self.health.write().await = state.clone();
                state
            }
        }
    }

    pub async fn health(&self) -> LlmHealthState {
        self.health.read().await.clone()
    }

    /// Replace the stored params. Does NOT automatically reload —
    /// caller must call `enable()` again.
    pub async fn update_params(&self, params: BackendParams) {
        *self.params.write().await = Some(params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::BackendParams;

    fn default_params() -> BackendParams {
        BackendParams {
            url: "http://127.0.0.1:11434/v1".into(),
            model_name: "test".into(),
            api_key: None,
            max_concurrent_requests: 4,
            request_timeout_secs: 60,
            max_input_chars: 16_000,
        }
    }

    #[tokio::test]
    async fn new_starts_empty() {
        let manager = LlmManager::new_stub();
        assert!(!manager.is_loaded().await);
        let health = manager.health().await;
        assert!(!health.loaded);
        assert!(!health.healthy);
        assert!(health.backend_name.is_none());
    }

    #[tokio::test]
    async fn enable_stub_succeeds() {
        let manager = LlmManager::new_stub();
        manager.enable().await.unwrap();
        assert!(manager.is_loaded().await);
        let health = manager.health().await;
        assert!(health.loaded);
        assert!(health.healthy);
        assert_eq!(health.backend_name.as_deref(), Some("stub"));
        assert!(health.loaded_at.is_some());
    }

    #[tokio::test]
    async fn enable_http_missing_url_errors() {
        let params = BackendParams {
            url: "".into(),
            model_name: "test".into(),
            ..default_params()
        };
        let manager = LlmManager::new(params);
        let result = manager.enable().await;
        assert!(matches!(result, Err(LlmError::Config(_))));
        assert!(!manager.is_loaded().await);
    }

    #[tokio::test]
    async fn enable_is_idempotent() {
        let manager = LlmManager::new_stub();
        manager.enable().await.unwrap();
        manager.enable().await.unwrap();
        assert!(manager.is_loaded().await);
    }

    #[tokio::test]
    async fn disable_drops_backend() {
        let manager = LlmManager::new_stub();
        manager.enable().await.unwrap();
        assert!(manager.is_loaded().await);
        manager.disable().await;
        assert!(!manager.is_loaded().await);
        let health = manager.health().await;
        assert!(!health.loaded);
        assert!(health.last_error.is_none());
    }

    #[tokio::test]
    async fn check_health_on_no_backend_returns_disabled() {
        let manager = LlmManager::new_stub();
        let health = manager.check_health().await;
        assert!(!health.loaded);
        assert!(!health.healthy);
        assert!(health.last_error.is_none());
        assert!(health.last_check.is_some());
    }

    #[tokio::test]
    async fn check_health_on_stub_is_healthy() {
        let manager = LlmManager::new_stub();
        manager.enable().await.unwrap();
        let health = manager.check_health().await;
        assert!(health.healthy);
        assert!(health.loaded);
        assert!(health.last_error.is_none());
    }

    #[tokio::test]
    async fn backend_returns_arc_clone() {
        let manager = LlmManager::new_stub();
        manager.enable().await.unwrap();
        let b1 = manager.backend().await.unwrap();
        let b2 = manager.backend().await.unwrap();
        assert_eq!(b1.name(), b2.name());
    }

    #[tokio::test]
    async fn backend_none_before_enable() {
        let manager = LlmManager::new_stub();
        assert!(manager.backend().await.is_none());
    }

    #[tokio::test]
    async fn update_params_does_not_auto_enable() {
        let manager = LlmManager::new_stub();
        manager.update_params(default_params()).await;
        assert!(!manager.is_loaded().await);
    }
}
