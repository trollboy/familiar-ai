//! PRD-061 agent-loop-facing xAI glue.
//!
//! The wire adapter itself (`familiar_ai_llm::xai_api::XaiAdapter`) is a
//! complete, independently testable `InferenceAdapter` — this module adds
//! nothing to its wire behavior. `raw_runtime::run_loop` already builds
//! provider-neutral tool definitions from the canonical capability table
//! (`offered_tool_definitions`), so no xAI-specific tool-schema translation
//! belongs here either. What this module owns is the loop-facing surface
//! that is xAI's own and does not belong in the provider-neutral
//! `familiar-ai-llm` crate: runtime/model-family identity constants and the
//! capability-provenance record PRD-057 requires — declared honestly,
//! never borrowed from OpenAI's protocol resemblance.

pub use familiar_ai_llm::xai_api::{
    XaiAdapter, XaiAdapterConfig, XAI_DEFAULT_BASE_URL, XAI_RUNTIME_ID,
};

/// xAI's model family for PRD-057 worker identity. A worker's own `model`
/// field (e.g. `grok-4`, `grok-4.3`) selects a specific member; this
/// constant is never itself a model identity.
pub const GROK_MODEL_FAMILY: &str = "grok";

/// One PRD-057 capability's provenance, as verified (or not) against
/// `docs.x.ai` on 2026-09-01. Mirrors the vocabulary in
/// `familiar_ai_core::config::registry_workers::RuntimeCapabilityConfig` —
/// duplicated as plain data here (rather than referencing that type)
/// because `familiar-ai-agent` does not depend on `familiar-ai-core`; the
/// authoritative typed record lives in a worker's own
/// `[worker_registry.capability_profiles.*]` configuration, which an
/// operator populates from this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XaiCapabilityProvenance {
    /// Confirmed against current official xAI documentation.
    Declared,
    /// Exercised against the real API and observed to behave a specific
    /// way, without a documentation citation confirming it as guaranteed.
    Probed,
    /// Not established either way; resemblance to another provider's
    /// protocol is never treated as evidence.
    Unknown,
}

/// `(capability name, provenance, note)`. See
/// `docs/contracts/xai-adapter.md` for the full verification record and
/// sources.
pub const XAI_CAPABILITY_PROFILE: &[(&str, XaiCapabilityProvenance, &str)] = &[
    (
        "native-tool-calling",
        XaiCapabilityProvenance::Declared,
        "docs.x.ai function-calling guide: call_id/name/arguments, parallel tool calls on by default",
    ),
    (
        "streaming",
        XaiCapabilityProvenance::Declared,
        "docs.x.ai: SSE chunks over /v1/chat/completions, `data: [DONE]` termination",
    ),
    (
        "parallel-tool-calls",
        XaiCapabilityProvenance::Declared,
        "docs.x.ai: parallel function calling is enabled by default",
    ),
    (
        "usage-reporting-categories",
        XaiCapabilityProvenance::Declared,
        "docs.x.ai: prompt_tokens_details.{text_tokens,cached_tokens}, completion_tokens_details.reasoning_tokens; no cache-write count is documented",
    ),
    (
        "cost-reporting-mode",
        XaiCapabilityProvenance::Declared,
        "docs.x.ai cost-tracking guide: per-request cost_in_usd_ticks is vendor-reported; no admin/org billing API was found — that remains unsupported",
    ),
    (
        "structured-output",
        XaiCapabilityProvenance::Probed,
        "response_format json_schema wire shape exercised against mocked responses only; not confirmed against official xAI documentation",
    ),
    (
        "reasoning-controls",
        XaiCapabilityProvenance::Unknown,
        "no request-side reasoning-control parameter was verifiable in the consulted xAI documentation; the adapter sends none",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_id_matches_the_wire_adapter() {
        assert_eq!(XAI_RUNTIME_ID, "xai-api");
    }

    #[test]
    fn every_capability_entry_has_a_non_empty_note() {
        for (name, _, note) in XAI_CAPABILITY_PROFILE {
            assert!(!name.is_empty());
            assert!(!note.is_empty());
        }
    }

    #[test]
    fn reasoning_controls_and_structured_output_are_never_declared() {
        let entry = |name: &str| {
            XAI_CAPABILITY_PROFILE
                .iter()
                .find(|(n, _, _)| *n == name)
                .unwrap()
        };
        assert_ne!(
            entry("reasoning-controls").1,
            XaiCapabilityProvenance::Declared
        );
        assert_ne!(
            entry("structured-output").1,
            XaiCapabilityProvenance::Declared
        );
    }
}
