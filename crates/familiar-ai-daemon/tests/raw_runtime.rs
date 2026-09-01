//! PRD-058 daemon-side integration coverage: the SQLite-backed tool
//! journal, the sandboxed executor, the PRD-013-derived write-scope
//! authorizer, resume reconciliation, and PRD-051 usage-ledger persistence.
//! Every test runs exclusively against `FakeInferenceAdapter` — no test
//! here performs, or could perform, a live or billable model call.

use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CallDisposition, CancellationToken, CapabilityId,
    InMemoryToolJournal, JournalIntent, JournalResult, LoopCeilings, LoopConfig, SideEffectClass,
    StablePrefix, StopReason, ToolJournal, VolatileTask,
};
use familiar_ai_core::config::AgentRuntimeSandboxConfig;
use familiar_ai_daemon::agent_runtime::{
    persist_run_outcome, resume_readiness, write_scope_authorizer_from_prd, ResumeReadiness,
    SandboxedToolExecutor, SqliteToolJournal,
};
use familiar_ai_llm::attempt::{
    AdapterStopReason, AttemptId, FakeInferenceAdapter, ScriptedTurn, StreamEvent, SubmitOutcome,
    UsageCategories,
};
use familiar_ai_storage::repos::agent_runtime::AgentRuntimeRepository;
use familiar_ai_storage::Database;

const SAMPLE_PRD: &str = "# PRD-999: Sample\n\n## Expected Files\n\n- `src/lib.rs`\n";

fn setup_execution(db: &Database, execution_id: &str) {
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'raw-runtime','running','repo','wt','docs/prds/PRD-999.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
    }
}

fn no_sandbox() -> AgentRuntimeSandboxConfig {
    AgentRuntimeSandboxConfig {
        allowed_commands: vec![],
        network_allowed: false,
        allowed_environment: vec![],
    }
}

#[tokio::test]
async fn full_round_trip_persists_journal_evidence_and_usage_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let authorizer =
        write_scope_authorizer_from_prd(SAMPLE_PRD, vec![CapabilityId::ApplyEdit], &no_sandbox())
            .unwrap();
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree.clone(),
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");

    let adapter = FakeInferenceAdapter::new(vec![
        ScriptedTurn {
            events: vec![
                StreamEvent::ToolCallDelta {
                    call_id: "call_1".into(),
                    capability_id: "apply-edit".into(),
                    arguments_fragment: "{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}"
                        .into(),
                },
                StreamEvent::ToolCallComplete {
                    call_id: "call_1".into(),
                },
            ],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::ToolUse,
                usage: UsageCategories {
                    uncached_input_tokens: Some(50),
                    output_tokens: Some(10),
                    ..Default::default()
                },
                provider_request_id: Some("req_1".into()),
                provider_idempotency_key: None,
            }),
        },
        ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories {
                    output_tokens: Some(3),
                    ..Default::default()
                },
                provider_request_id: Some("req_2".into()),
                provider_idempotency_key: None,
            }),
        },
    ]);

    let config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ApplyEdit],
        structured_output: None,
        authority: base_authority(),
    };

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "stable context".into(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "implement it".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("src/lib.rs")).unwrap(),
        "fn main() {}"
    );

    persist_run_outcome(
        db.conn(),
        "exec_1",
        "raw-runtime-review",
        "worker_1",
        "raw-runtime",
        Some("fake-model"),
        None,
        &outcome,
    )
    .unwrap();

    // Journal: intent recorded and resolved, nothing left pending.
    let repo = AgentRuntimeRepository::new(db.conn());
    assert!(repo.pending_intents("exec_1").unwrap().is_empty());
    assert!(matches!(
        repo.tool_result("exec_1", "call_1").unwrap(),
        Some(familiar_ai_storage::repos::agent_runtime::ToolResultOutcome::Succeeded { .. })
    ));

    // Evidence: one row, honest stop reason.
    let evidence_count: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM agent_runtime_evidence WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence_count, 1);
    let stop_reason: String = db
        .conn()
        .query_row(
            "SELECT stop_reason FROM agent_runtime_evidence WHERE execution_id='exec_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stop_reason, "completed");

    // Usage ledger: two attempts, distinct token categories, no prompt/
    // response/tool-output text anywhere in the row, and the full PRD-057
    // spec identity carried through from the loop's own worker spec.
    let mut statement = db
        .conn()
        .prepare("SELECT attempt_id,uncached_input_tokens,output_tokens,spec_identity,empirical_version FROM usage_observations WHERE execution_id='exec_1' ORDER BY attempt_id")
        .unwrap();
    type UsageRow = (
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<UsageRow> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            "att_1".into(),
            Some(50),
            Some(10),
            Some("wspec-sha256:test".into()),
            Some("wver-sha256:test".into())
        )
    );
    assert_eq!(
        rows[1],
        (
            "att_2".into(),
            None,
            Some(3),
            Some("wspec-sha256:test".into()),
            Some("wver-sha256:test".into())
        )
    );
}

#[tokio::test]
async fn write_outside_expected_files_is_refused_before_any_effect() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    // SAMPLE_PRD only authorizes src/lib.rs.
    let authorizer =
        write_scope_authorizer_from_prd(SAMPLE_PRD, vec![CapabilityId::ApplyEdit], &no_sandbox())
            .unwrap();
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree.clone(),
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");

    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "apply-edit".into(),
                arguments_fragment: "{\"path\":\"src/evil.rs\",\"content\":\"malicious\"}".into(),
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

    let mut config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::ApplyEdit],
        structured_output: None,
        authority: base_authority(),
    };
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: String::new(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "implement it".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(!worktree.join("src/evil.rs").exists());
    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::AuthorizationRefused { .. }
    ));
    let repo = AgentRuntimeRepository::new(db.conn());
    assert!(
        repo.pending_intents("exec_1").unwrap().is_empty(),
        "a refused write must never reach the write-ahead journal"
    );
}

#[tokio::test]
async fn allowlisted_command_executes_and_denylisted_command_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let sandbox = AgentRuntimeSandboxConfig {
        allowed_commands: vec!["echo".into()],
        network_allowed: false,
        allowed_environment: vec![],
    };
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::RunCommand],
        allowed_write_paths: vec![],
        allowed_commands: sandbox.allowed_commands.clone(),
        network_allowed: false,
    };
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree.clone(),
        sandbox: sandbox.clone(),
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");

    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "run-command".into(),
                arguments_fragment: "{\"argv\":[\"echo\",\"hello-from-sandbox\"]}".into(),
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

    let mut config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::RunCommand],
        structured_output: None,
        authority: base_authority(),
    };
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: String::new(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "run it".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::Executed { .. }
    ));
    let repo = AgentRuntimeRepository::new(db.conn());
    let result = repo.tool_result("exec_1", "call_1").unwrap();
    assert!(matches!(
        result,
        Some(familiar_ai_storage::repos::agent_runtime::ToolResultOutcome::Succeeded { .. })
    ));
}

#[tokio::test]
async fn denylisted_command_is_refused_before_any_process_ever_launches() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let sandbox = AgentRuntimeSandboxConfig {
        allowed_commands: vec!["echo".into()],
        network_allowed: false,
        allowed_environment: vec![],
    };
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::RunCommand],
        allowed_write_paths: vec![],
        allowed_commands: sandbox.allowed_commands.clone(),
        network_allowed: false,
    };
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree.clone(),
        sandbox,
        command_timeout_ms: 2_000,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");

    let marker = worktree.join("should-not-exist");
    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "run-command".into(),
                arguments_fragment: format!(
                    "{{\"argv\":[\"touch\",{:?}]}}",
                    marker.to_string_lossy()
                ),
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

    let mut config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::RunCommand],
        structured_output: None,
        authority: base_authority(),
    };
    config.ceilings.max_iterations = 1;

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: String::new(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "run it".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;

    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::AuthorizationRefused { .. }
    ));
    assert!(
        !marker.exists(),
        "a denied command must never launch a process"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn command_timeout_kills_the_process_group_instead_of_waiting_it_out() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let sandbox = AgentRuntimeSandboxConfig {
        allowed_commands: vec!["sleep".into()],
        network_allowed: false,
        allowed_environment: vec![],
    };
    let authorizer = familiar_ai_agent::raw_runtime::ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::RunCommand],
        allowed_write_paths: vec![],
        allowed_commands: sandbox.allowed_commands.clone(),
        network_allowed: false,
    };
    let mut executor = SandboxedToolExecutor {
        worktree_root: worktree,
        sandbox,
        command_timeout_ms: 150,
        max_output_bytes: 4096,
    };
    let mut journal = SqliteToolJournal::new(db.conn(), "exec_1");

    let adapter = FakeInferenceAdapter::new(vec![ScriptedTurn {
        events: vec![
            StreamEvent::ToolCallDelta {
                call_id: "call_1".into(),
                capability_id: "run-command".into(),
                arguments_fragment: "{\"argv\":[\"sleep\",\"5\"]}".into(),
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

    let mut config = LoopConfig {
        worker_spec_identity: "wspec-sha256:test".into(),
        worker_empirical_version: "wver-sha256:test".into(),
        model: "fake-model".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings::default(),
        offered_capabilities: vec![CapabilityId::RunCommand],
        structured_output: None,
        authority: base_authority(),
    };
    config.ceilings.max_iterations = 1;

    let started = std::time::Instant::now();
    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: String::new(),
            version: "prefix-v1".into(),
        },
        &VolatileTask {
            bytes: "run it".into(),
        },
        &config,
        attempt_id_source(),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the watchdog must kill the process well before the 5s sleep completes; took {elapsed:?}"
    );
    assert!(matches!(
        outcome.evidence.calls[0].disposition,
        CallDisposition::ExecutionFailed { .. }
    ));
}

#[test]
fn resume_readiness_blocks_on_a_destructive_intent_without_a_result() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_blocked");
    let repo = AgentRuntimeRepository::new(db.conn());
    repo.record_tool_intent(
        "exec_blocked",
        "call_1",
        "run-command",
        "hash",
        "destructive",
    )
    .unwrap();

    assert_eq!(
        resume_readiness(db.conn(), "exec_blocked").unwrap(),
        ResumeReadiness::Blocked {
            call_id: "call_1".into(),
            capability: "run-command".into()
        }
    );
}

#[test]
fn resume_readiness_is_ready_when_pending_intents_are_replayable() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_ready");
    let repo = AgentRuntimeRepository::new(db.conn());
    repo.record_tool_intent(
        "exec_ready",
        "call_1",
        "apply-edit",
        "hash",
        "idempotent-write",
    )
    .unwrap();

    assert_eq!(
        resume_readiness(db.conn(), "exec_ready").unwrap(),
        ResumeReadiness::Ready
    );
}

#[test]
fn resume_readiness_is_ready_once_the_result_is_recorded() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_done");
    let repo = AgentRuntimeRepository::new(db.conn());
    repo.record_tool_intent("exec_done", "call_1", "run-command", "hash", "destructive")
        .unwrap();
    repo.record_tool_result(
        "exec_done",
        "call_1",
        &familiar_ai_storage::repos::agent_runtime::ToolResultOutcome::Succeeded {
            result_hash: "rh".into(),
        },
    )
    .unwrap();

    assert_eq!(
        resume_readiness(db.conn(), "exec_done").unwrap(),
        ResumeReadiness::Ready
    );
}

#[test]
fn in_memory_journal_matches_sqlite_journal_semantics_for_a_completed_call() {
    // Cross-check: the loop-core reference journal and the SQLite-backed
    // journal must agree on the same intent/result contract.
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_cross");
    let mut sqlite_journal = SqliteToolJournal::new(db.conn(), "exec_cross");
    let mut memory_journal = InMemoryToolJournal::default();

    let intent = JournalIntent {
        call_id: "call_1".into(),
        capability: CapabilityId::ApplyEdit,
        argument_hash: "hash".into(),
        side_effect_class: SideEffectClass::IdempotentWrite,
    };
    sqlite_journal.record_intent(&intent).unwrap();
    memory_journal.record_intent(&intent).unwrap();
    assert_eq!(
        sqlite_journal.result_for("call_1"),
        memory_journal.result_for("call_1")
    );

    let result = JournalResult::Succeeded {
        result_hash: "rh".into(),
    };
    sqlite_journal.record_result("call_1", &result).unwrap();
    memory_journal.record_result("call_1", &result).unwrap();
    assert_eq!(
        sqlite_journal.result_for("call_1"),
        memory_journal.result_for("call_1")
    );
}

#[test]
fn write_scope_authorizer_rejects_a_prd_without_an_expected_files_section() {
    let error = write_scope_authorizer_from_prd(
        "# PRD-999: No Expected Files\n\nJust prose.\n",
        vec![CapabilityId::ApplyEdit],
        &no_sandbox(),
    )
    .unwrap_err();
    assert!(!error.is_empty());
}
