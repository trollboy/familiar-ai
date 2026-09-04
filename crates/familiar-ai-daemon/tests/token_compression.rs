use std::collections::BTreeMap;

use familiar_ai_compress::{InputTransform, ProviderPart, RegisterId, TransformedPart};
use familiar_ai_core::Config;
use familiar_ai_daemon::run::{configured_output_register, inject_output_register};
use familiar_ai_storage::{
    AccountingRepository, Database, ExecutionHistoryRepository, ExecutionStart, UsageObservation,
};

#[test]
fn input_round_trip_is_byte_exact_and_restart_determinism_is_pinned() {
    let input = b"prefix\0\xffaaaaaaaaaaaaaaaa\r\nbody";
    let encoded = InputTransform.compress(input);
    assert_eq!(encoded, InputTransform.compress(input));
    assert_eq!(InputTransform.decompress(&encoded).unwrap(), input);
    // A pinned vector detects any process/version-dependent encoder state.
    assert_eq!(encoded, b"FAIC\x01\x07prefix\0\xff\x8fa\x05\r\nbody");
}

#[test]
fn provider_cache_breakpoint_is_verbatim_and_keeps_its_part_index() {
    let marker = br#"{"cache_control":{"type":"ephemeral"}}"#;
    let transformed = InputTransform.transform_parts(&[
        ProviderPart::Content(b"stable"),
        ProviderPart::CacheControl(marker),
        ProviderPart::Content(b"volatile"),
    ]);
    assert_eq!(
        transformed[1],
        TransformedPart::CacheControl(marker.to_vec())
    );
}

#[test]
fn disabled_register_preserves_prompt_bytes() {
    let prompt = "historical prompt bytes\n";
    let config = Config::default();
    assert_eq!(configured_output_register(&config, "review").unwrap(), None);
    assert_eq!(
        inject_output_register(prompt, None).as_bytes(),
        prompt.as_bytes()
    );
}

#[test]
fn register_protects_structured_review_findings_contract() {
    let finding = r#"{"findings":[{"path":"src/lib.rs","identifier":"Thing::run","code":"`let x = 1;`","diff":"@@ -1 +1 @@"}]}"#;
    let before: serde_json::Value = serde_json::from_str(finding).unwrap();
    let prompt = inject_output_register(finding, Some(RegisterId::Compact));
    assert!(prompt.contains("structured machine-parsed output byte-for-byte"));
    assert!(prompt.contains("file path, identifier"));
    let protected = prompt.lines().next().unwrap();
    let after: serde_json::Value = serde_json::from_str(protected).unwrap();
    assert_eq!(before, after);
    assert_eq!(protected.as_bytes(), finding.as_bytes());
}

#[test]
fn ledger_partitions_compression_lanes_and_records_explicit_none() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: "execution".into(),
            started_at: "2026-08-30T00:00:00Z".into(),
            repository: "/repo".into(),
            worktree: "/repo".into(),
            git_commit: None,
            prd_path: "docs/prds/PRD-069.md".into(),
            unavailable_fields: BTreeMap::new(),
        })
        .unwrap();
    let repo = AccountingRepository::new(db.conn());
    for (lane, register, tokens) in [("off", "none", 20), ("on", "compact", 9)] {
        repo.append_observation(&UsageObservation {
            execution_id: "execution",
            attempt_id: lane,
            stage: "review",
            session_id: None,
            worker_identity: "fixture",
            adapter: "fixture",
            cli_version: None,
            model_identity: None,
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: Some(tokens),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            output_tokens: Some(tokens),
            reasoning_output_tokens: None,
            unknown_reason: None,
            period_start: "2026-08-30T00:00:00Z",
            period_end: "2026-08-30T00:00:01Z",
            terminal_status: "succeeded",
            source_event_hash: lane,
            provider_cost_lexical: None,
            project_resolution_evidence: None,
            output_register_id: register,
            output_register_version: if register == "none" { "none" } else { "1" },
            input_compression_id: "none",
            input_compression_version: "none",
            compression_experiment: Some("paired"),
            compression_lane: Some(lane),
            edit_form_id: "none",
            edit_form_version: "none",
            truncation_config_id: "none",
            truncation_config_version: "none",
        })
        .unwrap();
    }
    let summary = repo.compression_experiment("paired").unwrap();
    assert_eq!(summary.off.output_tokens, Some(20));
    assert_eq!(summary.on.output_tokens, Some(9));
    let states: i64 = db
        .conn()
        .query_row(
            "SELECT count(DISTINCT output_register_id || '@' || output_register_version) FROM usage_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(states, 2);
}
