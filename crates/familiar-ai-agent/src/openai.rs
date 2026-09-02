//! PRD-060 OpenAI Responses API adapter: the PRD-058
//! [`InferenceAdapter`] implementation over
//! `familiar_ai_llm::openai_api`. Implementing this trait is the entire
//! integration surface — adding it changes no loop, routing, accounting,
//! or execution semantics (`docs/contracts/agent-loop.md`,
//! `docs/contracts/openai-adapter.md`).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use familiar_ai_llm::attempt::{
    AdapterError, AttemptId, InferenceAdapter, StreamObserver, SubmitOutcome, SubmitRequest,
};
pub use familiar_ai_llm::openai_api::{
    ApiKey, OpenAiResponseMeta, OpenAiResponsesConfig, DEFAULT_BASE_URL,
};
use familiar_ai_llm::openai_api::{OpenAiResponsesClient, ResponsesRequest};

pub const RUNTIME_ID: &str = "openai-api";

/// The PRD-057 `runtime` identity string every OpenAI raw-API worker spec
/// uses. Never a source of routing/accounting behavior by itself — routing
/// and accounting key on the full spec, per PRD-057.
pub fn runtime_id() -> &'static str {
    RUNTIME_ID
}

/// The `InferenceAdapter` implementation for OpenAI's Responses API.
///
/// Beyond one `submit` call, this type keeps exactly one piece of state:
/// a per-attempt metadata map recording the response-resolved model
/// identity and service tier. `SubmitOutcome`/`UsageCategories` (the
/// PRD-058 contract) already carry every token category and the provider
/// request id distinctly; they have no field for a resolved model or
/// service tier, since those are provider-specific facts, not loop
/// concerns. [`OpenAiInferenceAdapter::response_meta`] exposes them so a
/// host can enrich its own accounting rows without the loop or contract
/// ever needing to know they exist. A moving alias (the *requested*
/// model, which the caller already has from its own `SubmitRequest`) is
/// never overwritten or frozen by this map — both requested and resolved
/// identity stay independently available.
pub struct OpenAiInferenceAdapter {
    client: OpenAiResponsesClient,
    meta: Mutex<HashMap<String, OpenAiResponseMeta>>,
}

impl OpenAiInferenceAdapter {
    /// `api_key` is a credential already resolved by the caller at the
    /// adapter boundary (BYO-Auth: this constructor never reads an
    /// environment variable, a credential store, or configuration itself —
    /// see `docs/contracts/credential-authentication.md`). It is held only
    /// for this adapter instance's lifetime and never logged or serialized.
    pub fn new(api_key: impl Into<String>, config: OpenAiResponsesConfig) -> Result<Self, String> {
        Ok(Self {
            client: OpenAiResponsesClient::new(ApiKey::new(api_key), config)?,
            meta: Mutex::new(HashMap::new()),
        })
    }

    /// The response-resolved model identity and service tier observed for
    /// one past attempt, if that submission reached a definitive
    /// completed/incomplete outcome (an errored or ambiguous attempt
    /// records nothing here, since no such identity was ever confirmed).
    pub fn response_meta(&self, attempt_id: &AttemptId) -> Option<OpenAiResponseMeta> {
        self.meta
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&attempt_id.0)
            .cloned()
    }
}

#[async_trait]
impl InferenceAdapter for OpenAiInferenceAdapter {
    fn runtime_id(&self) -> &str {
        RUNTIME_ID
    }

    async fn submit(
        &self,
        request: &SubmitRequest,
        observer: &mut dyn StreamObserver,
    ) -> Result<SubmitOutcome, AdapterError> {
        let responses_request = ResponsesRequest {
            model: &request.model,
            messages: &request.messages,
            tools: &request.tools,
            structured_output: request.structured_output.as_ref(),
            reasoning_control: request.reasoning_control.as_ref(),
            prompt_cache_key: request.prompt_cache_key.as_deref(),
        };
        let (outcome, meta) = self.client.submit(&responses_request, observer).await?;
        self.meta
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request.attempt_id.0.clone(), meta);
        Ok(outcome)
    }

    // Best-effort only, matching the PRD-058 contract: "resumable" means
    // Familiar resumes its own workflow state, never that the provider
    // resumes the interrupted request. This client issues one buffered
    // HTTP request per `submit` (see `openai_api`'s streaming note) with
    // no in-flight handle to cancel, so there is nothing more to do here
    // than the trait's own no-op default.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_id_is_openai_api() {
        assert_eq!(
            OpenAiInferenceAdapter::new("sk-test", OpenAiResponsesConfig::default())
                .unwrap()
                .runtime_id(),
            "openai-api"
        );
    }

    #[test]
    fn unknown_attempt_has_no_response_meta() {
        let adapter =
            OpenAiInferenceAdapter::new("sk-test", OpenAiResponsesConfig::default()).unwrap();
        assert_eq!(adapter.response_meta(&AttemptId("nope".into())), None);
    }
}
