//! PRD-063 local worker adapter: the PRD-058 [`InferenceAdapter`]
//! implementation over `familiar_ai_llm::local_runtime`'s OpenAI-compatible
//! transport. Implementing this trait is the entire integration surface —
//! adding a local worker changes no loop, routing, accounting, or execution
//! semantics (`docs/contracts/local-worker-runtime.md`).
//!
//! Two `RuntimeId`s (`unsloth`, `ollama`) share this one transport, which is
//! reuse of the wire protocol only — never a claim of OpenAI behavior,
//! capabilities, pricing, or semantics. Each keeps its own runtime id,
//! capability profile, and empirical identity.
//!
//! **Production dispatch status:** this adapter, the PRD-064 reservation
//! glue in `familiar_ai_daemon::local_worker_runtime`, and the PRD-051
//! telemetry sink are complete and exercised end-to-end against fake
//! endpoints (see `tests/local_worker.rs` and the daemon crate's
//! `tests/local_worker_runtime.rs`), but no call site in
//! `familiar_ai_daemon::run`'s worker-selection/execution path constructs
//! this type yet — that path only builds the CLI-driven `CodingAgent`
//! adapters (`codex`, `claude-code`, `ollama`-via-Codex-harness). Wiring a
//! `provider = "local"` registry entry through to a real execution is
//! deferred, matching every other PRD-058 raw-runtime adapter already in
//! this workspace (`AnthropicAdapter`, the OpenAI and xAI adapters): fully
//! implemented and tested, none reachable from production dispatch either.
//! `familiar_ai_daemon::run::resolved_worker_plan` refuses (`Err`) any
//! stage selection that lands on a `local`-profile worker rather than
//! silently building the wrong CLI-driven adapter in this type's place.

use async_trait::async_trait;

use familiar_ai_core::config::{LocalRuntimeKind as ConfigLocalRuntimeKind, LocalWorkerConfig};
use familiar_ai_llm::attempt::{
    AdapterError, InferenceAdapter, StreamObserver, SubmitOutcome, SubmitRequest,
};
pub use familiar_ai_llm::local_runtime::{
    ArtifactVerificationOutcome, LocalAuthToken, LocalChatConfig, LocalResponseMeta,
    LocalRuntimeKind, OLLAMA_RUNTIME_ID, UNSLOTH_RUNTIME_ID,
};
use familiar_ai_llm::local_runtime::{LocalChatClient, LocalChatRequest};

/// The `InferenceAdapter` implementation shared by every PRD-063 local
/// worker. `runtime` selects the `RuntimeId` this instance reports and
/// which backend-specific behaviors (artifact digest support) apply; the
/// transport itself is identical across runtimes.
pub struct LocalInferenceAdapter {
    client: LocalChatClient,
    runtime: LocalRuntimeKind,
}

impl LocalInferenceAdapter {
    /// `token` is a credential already resolved by the caller at the
    /// adapter boundary (BYO-Auth: this constructor never reads an
    /// environment variable, a credential store, or configuration itself).
    /// Loopback endpoints typically pass `LocalAuthToken::new(None)`.
    pub fn new(
        runtime: LocalRuntimeKind,
        token: LocalAuthToken,
        config: LocalChatConfig,
    ) -> Result<Self, String> {
        Ok(Self {
            client: LocalChatClient::new(token, config)?,
            runtime,
        })
    }

    /// Non-executing capability/health probe: the models this endpoint
    /// currently serves.
    pub async fn probe_health(&self) -> Result<Vec<String>, AdapterError> {
        self.client.probe_health().await
    }

    /// Builds the adapter straight from a worker's registered PRD-063
    /// `[worker_registry.workers.<id>.local]` profile: which runtime applies
    /// and where its endpoint is reached. `token` is still resolved by the
    /// caller at the adapter boundary (BYO-Auth) — this constructor never
    /// reads a credential itself, matching [`LocalInferenceAdapter::new`].
    pub fn from_registry_config(
        config: &LocalWorkerConfig,
        token: LocalAuthToken,
    ) -> Result<Self, String> {
        let runtime = match config.runtime_kind {
            ConfigLocalRuntimeKind::Ollama => LocalRuntimeKind::Ollama,
            ConfigLocalRuntimeKind::Unsloth => LocalRuntimeKind::Unsloth,
        };
        Self::new(
            runtime,
            token,
            LocalChatConfig {
                base_url: config.endpoint.base_url.clone(),
                ..LocalChatConfig::default()
            },
        )
    }

    /// Verifies the endpoint's claimed model against a registered PRD-062
    /// artifact digest. `Unverifiable` is the honest degraded path for any
    /// runtime or condition without exposed digest metadata — never treated
    /// as a match by routing policy.
    pub async fn verify_artifact(
        &self,
        model: &str,
        expected_digest: Option<&str>,
    ) -> ArtifactVerificationOutcome {
        self.client
            .verify_artifact(self.runtime, model, expected_digest)
            .await
    }
}

#[async_trait]
impl InferenceAdapter for LocalInferenceAdapter {
    fn runtime_id(&self) -> &str {
        self.runtime.as_str()
    }

    async fn submit(
        &self,
        request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError> {
        let local_request = LocalChatRequest {
            model: &request.model,
            messages: &request.messages,
            tools: &request.tools,
            structured_output: request.structured_output.as_ref(),
        };
        let (outcome, _meta) = self.client.submit(&local_request, observer).await?;
        Ok(outcome)
    }

    // Best-effort only, matching the PRD-058 contract: this client issues
    // one buffered HTTP request per `submit` with no in-flight handle to
    // cancel, so there is nothing more to do here than the trait's own
    // no-op default.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_id_reflects_the_configured_backend() {
        let ollama = LocalInferenceAdapter::new(
            LocalRuntimeKind::Ollama,
            LocalAuthToken::new(None),
            LocalChatConfig::default(),
        )
        .unwrap();
        assert_eq!(ollama.runtime_id(), OLLAMA_RUNTIME_ID);

        let unsloth = LocalInferenceAdapter::new(
            LocalRuntimeKind::Unsloth,
            LocalAuthToken::new(None),
            LocalChatConfig::default(),
        )
        .unwrap();
        assert_eq!(unsloth.runtime_id(), UNSLOTH_RUNTIME_ID);
    }

    #[test]
    fn adapter_builds_from_the_registered_local_worker_profile() {
        use familiar_ai_core::config::{LocalEndpointConfig, LocalResourceProfileConfig};

        let config = LocalWorkerConfig {
            runtime_kind: ConfigLocalRuntimeKind::Ollama,
            endpoint: LocalEndpointConfig {
                base_url: "http://127.0.0.1:11434".into(),
                tls: false,
            },
            resources: LocalResourceProfileConfig::default(),
        };
        let adapter =
            LocalInferenceAdapter::from_registry_config(&config, LocalAuthToken::new(None))
                .unwrap();
        assert_eq!(adapter.runtime_id(), OLLAMA_RUNTIME_ID);
    }
}
