use familiar_ai_review::{
    select_capability_proven_reviewer, validate_review_capability, ReviewCapabilityProbe,
    ReviewCapabilityReason, STRUCTURED_REVIEW_PROTOCOL,
};

fn probe(tools: bool, version: &str) -> ReviewCapabilityProbe {
    ReviewCapabilityProbe {
        structured_output: true,
        native_tool_calling: tools,
        protocol: STRUCTURED_REVIEW_PROTOCOL.into(),
        runtime_version: version.into(),
        provenance: "probed".into(),
        probed_at: "2026-08-31T00:00:00Z".into(),
    }
}

#[test]
fn llama3_without_tools_is_never_selected_for_structured_review() {
    assert_eq!(
        validate_review_capability("ollama", &probe(false, "0.13.0")),
        Err(ReviewCapabilityReason::ToolCallingUnsupported)
    );
}

#[test]
fn ollama_0_12_3_is_a_typed_version_incompatibility() {
    assert!(matches!(
        validate_review_capability("ollama", &probe(true, "0.12.3")),
        Err(ReviewCapabilityReason::RuntimeTooOld { minimum, observed, .. })
            if minimum == "0.12.4" && observed == "0.12.3"
    ));
}

#[test]
fn deterministic_failure_quarantines_once_and_reroutes() {
    let incapable = probe(false, "0.13.0");
    let capable = probe(true, "0.13.0");
    let selected = select_capability_proven_reviewer([
        ("llama3", "ollama", &incapable),
        ("reviewer-b", "ollama", &capable),
    ])
    .unwrap();
    assert_eq!(selected, "reviewer-b");
}

#[test]
fn exhausted_pool_names_every_quarantined_worker_and_reason() {
    let no_tools = probe(false, "0.13.0");
    let old = probe(true, "0.12.3");
    let error = select_capability_proven_reviewer([
        ("llama3", "ollama", &no_tools),
        ("old-ollama", "ollama", &old),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("review_capability_outage:"));
    assert!(error.contains("llama3: tool_calling_unsupported"));
    assert!(error.contains("old-ollama: runtime_too_old"));
}
