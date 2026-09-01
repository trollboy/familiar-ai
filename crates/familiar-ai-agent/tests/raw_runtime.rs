//! PRD-058 integration coverage for the raw-runtime agent loop. Every test
//! runs exclusively against [`FakeInferenceAdapter`] — no test in this file
//! performs, or could perform, a live or billable model call.

use std::sync::{Arc, Mutex};

use familiar_ai_agent::raw_runtime::{
    canonical_capabilities, run_loop, AttemptUsage, AuthorityContext, AuthorizationDecision,
    AuthorizationRefusal, CallDisposition, CancellationToken, CapabilityId, ExecutionError,
    ExecutionOutcome, InMemoryToolJournal, JournalIntent, JournalResult, LoopCeilings, LoopConfig,
    RefusalContinuation, ScopeAuthorizer, StablePrefix, StopReason, ToolAuthorizer, ToolExecutor,
    ToolJournal, ValidatedCall, ValidationRefusal, VolatileTask,
};
use familiar_ai_llm::attempt::{
    AdapterError, AdapterStopReason, AttemptId, FakeInferenceAdapter, NonRetryableKind,
    ScriptedTurn, StreamEvent, SubmitOutcome, UsageCategories,
};

fn authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
    }
}

fn prefix() -> StablePrefix {
    StablePrefix {
        bytes: "stable repository context".into(),
        version: "prefix-v1".into(),
    }
}

fn task() -> VolatileTask {
    VolatileTask {
        bytes: "review this diff".into(),
    }
}

/// Records every call it receives so tests can assert a refused call is
/// never executed.
#[derive(Default)]
struct SpyExecutor {
    calls: Vec<ValidatedCall>,
}

impl ToolExecutor for SpyExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        self.calls.push(call.clone());
        Ok(ExecutionOutcome {
            result_text: "ok".into(),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_config() -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 10,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        },
        offered_capabilities: vec![
            CapabilityId::ReadFile,
            CapabilityId::ApplyEdit,
            CapabilityId::ReportProgress,
        ],
        structured_output: None,
        authority: authority(),
    }
}

#[tokio::test]
async fn full_loop_completes_against_fake_adapter_with_no_live_call() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![StreamEvent::TextDelta("all good".into())],
        outcome: Ok(SubmitOutcome {
            stop_reason: AdapterStopReason::EndTurn,
            usage: UsageCategories {
                uncached_input_tokens: Some(120),
                output_tokens: Some(8),
                ..Default::default()
            },
            provider_request_id: Some("req_1".into()),
            provider_idempotency_key: None,
        }),
    }]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ReadFile, CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(outcome.final_text.as_deref(), Some("all good"));
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.attempts[0].usage.output_tokens, Some(8));
    assert!(!outcome.attempts[0].ambiguous);
    assert_eq!(outcome.evidence.iterations, 1);
    assert!(outcome.evidence.calls.is_empty());
    assert_eq!(outcome.evidence.worker_spec_identity, "wspec-sha256:test");
    assert_eq!(adapter.remaining_turns(), 0);
    assert!(executor.calls.is_empty());
}

#[tokio::test]
async fn tool_round_trip_executes_authorized_write_and_then_completes() {
    let adapter = FakeInferenceAdapter::new(vec![
        ScriptedTurn {
            events: vec![
                StreamEvent::ToolCallDelta {
                    call_id: "call_1".into(),
                    capability_id: "apply-edit".into(),
                    arguments_fragment: "{\"path\":\"src/lib.rs\",".into(),
                },
                StreamEvent::ToolCallDelta {
                    call_id: "call_1".into(),
                    capability_id: "apply-edit".into(),
                    arguments_fragment: "\"content\":\"fn main() {}\"}".into(),
                },
                StreamEvent::ToolCallComplete {
                    call_id: "call_1".into(),
                },
            ],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::ToolUse,
                usage: UsageCategories {
                    output_tokens: Some(20),
                    ..Default::default()
                },
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        },
        ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories {
                    output_tokens: Some(4),
                    ..Default::default()
                },
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        },
    ]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(outcome.evidence.iterations, 2);
    assert_eq!(executor.calls.len(), 1);
    assert_eq!(executor.calls[0].call_id, "call_1");
    assert_eq!(outcome.evidence.calls.len(), 1);
    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::Executed { .. }
    ));
    // Write-ahead: an intent exists and a result was recorded for this call.
    assert_eq!(journal.len(), 1);
    assert!(matches!(
        journal.result_for("call_1"),
        Some(JournalResult::Succeeded { .. })
    ));
    // Two submissions happened, so two distinct billable attempts exist.
    assert_eq!(outcome.attempts.len(), 2);
    assert_ne!(
        outcome.attempts[0].attempt_id,
        outcome.attempts[1].attempt_id
    );
}

#[tokio::test]
async fn malformed_tool_call_is_refused_and_never_reaches_the_executor() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "apply-edit".into(),
                arguments_fragment: "{not valid json".into(),
            },
            StreamEvent::ToolCallComplete {
                call_id: "call_1".into(),
            },
        ],
        outcome: Ok(SubmitOutcome {
            stop_reason: AdapterStopReason::ToolUse,
            usage: UsageCategories::default(),
            provider_request_id: None,
            provider_idempotency_key: None,
        }),
    }]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    let mut config = base_config();
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(
        executor.calls.is_empty(),
        "a malformed call must never execute"
    );
    assert!(
        journal.is_empty(),
        "a refused call must never reach the journal"
    );
    assert_eq!(outcome.evidence.calls.len(), 1);
    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::ValidationRefused(ValidationRefusal::MalformedArguments { .. })
    ));
}

#[tokio::test]
async fn unauthorized_write_is_refused_before_any_effect() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "apply-edit".into(),
                arguments_fragment: "{\"path\":\"/etc/passwd\",\"content\":\"x\"}".into(),
            },
            StreamEvent::ToolCallComplete {
                call_id: "call_1".into(),
            },
        ],
        outcome: Ok(SubmitOutcome {
            stop_reason: AdapterStopReason::ToolUse,
            usage: UsageCategories::default(),
            provider_request_id: None,
            provider_idempotency_key: None,
        }),
    }]);
    let mut executor = SpyExecutor::default();
    // Scope only authorizes writes beneath src/, so /etc/passwd is refused.
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    let mut config = base_config();
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(
        executor.calls.is_empty(),
        "an out-of-scope write must never reach the executor"
    );
    assert!(journal.is_empty());
    assert_eq!(outcome.evidence.calls.len(), 1);
    assert!(matches!(
        &outcome.evidence.calls[0].disposition,
        CallDisposition::AuthorizationRefused {
            reason: AuthorizationRefusal::OutOfWriteScope { path },
            ..
        } if path == "/etc/passwd"
    ));
}

/// An authorizer whose refusals are always fatal, to exercise the
/// stop-closed continuation path.
struct StopClosedAuthorizer;
impl ToolAuthorizer for StopClosedAuthorizer {
    fn authorize(&self, _call: &ValidatedCall, _ctx: &AuthorityContext) -> AuthorizationDecision {
        AuthorizationDecision::Refused {
            reason: AuthorizationRefusal::OutOfAuthorityScope {
                capability: CapabilityId::RunCommand,
            },
            continuation: RefusalContinuation::StopClosed,
        }
    }
}

#[tokio::test]
async fn fatal_tool_refusal_stops_the_loop_closed() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "run-command".into(),
                arguments_fragment: "{\"argv\":[\"rm\"]}".into(),
            },
            StreamEvent::ToolCallComplete {
                call_id: "call_1".into(),
            },
        ],
        outcome: Ok(SubmitOutcome {
            stop_reason: AdapterStopReason::ToolUse,
            usage: UsageCategories::default(),
            provider_request_id: None,
            provider_idempotency_key: None,
        }),
    }]);
    let mut executor = SpyExecutor::default();
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    let mut config = base_config();
    config.offered_capabilities = vec![CapabilityId::RunCommand];

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &StopClosedAuthorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::FatalToolRefusal);
    assert!(executor.calls.is_empty());
}

#[tokio::test]
async fn iteration_ceiling_stops_honestly_rather_than_looping_forever() {
    // Every scripted turn requests another (authorized, harmless) tool call,
    // so without a ceiling the loop would never terminate.
    let turns: Vec<ScriptedTurn> = (0..5)
        .map(|i| ScriptedTurn {
            events: vec![
                StreamEvent::ToolCallDelta {
                    call_id: format!("call_{i}"),
                    capability_id: "report-progress".into(),
                    arguments_fragment: "{\"message\":\"working\"}".into(),
                },
                StreamEvent::ToolCallComplete {
                    call_id: format!("call_{i}"),
                },
            ],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::ToolUse,
                usage: UsageCategories::default(),
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        })
        .collect();
    let adapter = FakeInferenceAdapter::new(turns);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ReportProgress],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    let mut config = base_config();
    config.ceilings.max_iterations = 2;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::IterationCeiling);
    assert_eq!(outcome.evidence.iterations, 2);
    assert_eq!(executor.calls.len(), 2);
}

#[tokio::test]
async fn ambiguous_provider_timeout_records_unknown_usage_never_zero() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![],
        outcome: Err(AdapterError::Ambiguous {
            reason: "connection reset before response arrived".into(),
        }),
    }]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert!(matches!(
        outcome.stop_reason,
        StopReason::ProviderFailure { .. }
    ));
    assert_eq!(outcome.attempts.len(), 1);
    let AttemptUsage {
        usage, ambiguous, ..
    } = &outcome.attempts[0];
    assert!(
        ambiguous,
        "an ambiguous attempt must be flagged, not zeroed"
    );
    assert!(
        usage.is_entirely_unknown(),
        "unknown usage must stay unknown, never fabricated as zero"
    );
}

#[tokio::test]
async fn cancellation_is_observed_between_iterations_preserving_partial_evidence() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![StreamEvent::TextDelta("partial".into())],
        outcome: Ok(SubmitOutcome {
            stop_reason: AdapterStopReason::MaxTokens,
            usage: UsageCategories {
                output_tokens: Some(1),
                ..Default::default()
            },
            provider_request_id: None,
            provider_idempotency_key: None,
        }),
    }]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    assert!(outcome.attempts.is_empty());
    // Cancellation was observed before the first submission, so the scripted
    // turn is never consumed — confirming no request reached the adapter.
    assert_eq!(adapter.remaining_turns(), 1);
}

#[tokio::test]
async fn non_retryable_provider_error_is_an_honest_provider_failure_not_a_ceiling() {
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![],
        outcome: Err(AdapterError::NonRetryable(NonRetryableKind::InvalidRequest)),
    }]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![],
        allowed_write_paths: vec![],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert!(matches!(
        outcome.stop_reason,
        StopReason::ProviderFailure { .. }
    ));
    assert_ne!(outcome.stop_reason, StopReason::TokenOrContextCeiling);
    assert_ne!(outcome.stop_reason, StopReason::IterationCeiling);
}

#[tokio::test]
async fn resumed_loop_never_repeats_a_completed_tool_call() {
    // Simulate a resume: the journal already carries a successful result for
    // call_1 from a prior (interrupted) run of this same execution.
    let adapter = FakeInferenceAdapter::new(vec![
        ScriptedTurn {
            events: vec![
                StreamEvent::ToolCallDelta {
                    call_id: "call_1".into(),
                    capability_id: "apply-edit".into(),
                    arguments_fragment: "{\"path\":\"src/lib.rs\",\"content\":\"x\"}".into(),
                },
                StreamEvent::ToolCallComplete {
                    call_id: "call_1".into(),
                },
            ],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::ToolUse,
                usage: UsageCategories::default(),
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        },
        ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories::default(),
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        },
    ]);
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    journal
        .record_intent(&JournalIntent {
            call_id: "call_1".into(),
            capability: CapabilityId::ApplyEdit,
            argument_hash: "prior-hash".into(),
            side_effect_class: familiar_ai_agent::raw_runtime::SideEffectClass::IdempotentWrite,
        })
        .unwrap();
    journal
        .record_result(
            "call_1",
            &JournalResult::Succeeded {
                result_hash: "prior-result".into(),
            },
        )
        .unwrap();
    let cancel = CancellationToken::new();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &cancel,
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert!(
        executor.calls.is_empty(),
        "a call already completed in the journal must never re-execute"
    );
    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::ResumedFromJournal { .. }
    ));
    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
}

#[test]
fn canonical_capabilities_cover_the_closed_prd_058_vocabulary() {
    let ids: Vec<_> = canonical_capabilities().into_iter().map(|c| c.id).collect();
    for expected in CapabilityId::ALL {
        assert!(
            ids.contains(&expected),
            "missing canonical capability {expected:?}"
        );
    }
    assert_eq!(ids.len(), CapabilityId::ALL.len());
}

#[tokio::test]
async fn concurrent_loops_share_no_hidden_state_across_executions() {
    // Two independent loop runs (as if for two different executions) must
    // not leak journal or evidence state into one another.
    let shared_calls = Arc::new(Mutex::new(0usize));
    struct CountingExecutor(Arc<Mutex<usize>>);
    impl ToolExecutor for CountingExecutor {
        fn execute(
            &mut self,
            call: &ValidatedCall,
            _ctx: &AuthorityContext,
        ) -> Result<ExecutionOutcome, ExecutionError> {
            *self.0.lock().unwrap() += 1;
            Ok(ExecutionOutcome {
                result_text: "ok".into(),
                result_hash: format!("hash-{}", call.call_id),
            })
        }
    }

    let make_adapter = || {
        FakeInferenceAdapter::new(vec![ScriptedTurn {
            events: vec![
                StreamEvent::ToolCallDelta {
                    call_id: "call_1".into(),
                    capability_id: "apply-edit".into(),
                    arguments_fragment: "{\"path\":\"src/a.rs\",\"content\":\"x\"}".into(),
                },
                StreamEvent::ToolCallComplete {
                    call_id: "call_1".into(),
                },
            ],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories::default(),
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        }])
    };
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };

    for _ in 0..2 {
        let adapter = make_adapter();
        let mut executor = CountingExecutor(Arc::clone(&shared_calls));
        let mut journal = InMemoryToolJournal::default();
        let cancel = CancellationToken::new();
        run_loop(
            &adapter,
            &mut executor,
            &authorizer,
            &mut journal,
            &cancel,
            &prefix(),
            &task(),
            &base_config(),
            attempt_id_source(),
        )
        .await;
        // Each fresh loop's own journal starts empty, so the call executes
        // once per independent run.
        assert_eq!(journal.len(), 1);
    }
    assert_eq!(*shared_calls.lock().unwrap(), 2);
}
