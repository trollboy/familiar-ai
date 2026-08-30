# PRD-037 Failure-Injection Coverage Matrix

This file is the durable phase-to-proof matrix required by PRD-037. `Closed`
means the injected condition cannot advance the durable claim named in the row.

| Phase / boundary | Injection | Expected durable result | Executable proof |
|---|---|---|---|
| PRD admission | shell syntax, absolute path, traversal, variable expansion | Closed: no scope authority | `familiar-ai-review/security_burn_in::malicious_scope_expressions_never_grant_authority` |
| Repository read | symlink outside denied tree | Closed: outside data unreadable | `familiar-ai-agent::codex::tests::isolated_execution_cannot_read_denied_repository` |
| Agent launch | prompt containing shell syntax | Closed: prompt remains stdin data | `familiar-ai-agent/security_burn_in::prompt_is_stdin_data_not_command_arguments` |
| Agent stream | EOF/corrupt terminal event | Closed: malformed, never completion | `familiar-ai-agent/security_burn_in::corrupt_or_truncated_stream_cannot_fabricate_completion` |
| Agent stream secret | malformed provider line containing a credential canary | Closed: malformed and redacted before capture | `familiar-ai-agent/security_burn_in::malformed_agent_output_is_redacted_before_forwarding` |
| Verification | failed evidence / persistence failure | Closed: no clean review or approval | `familiar-ai-review::coordinator::tests::storage_failure_around_scope_persistence_prevents_review_invocation` |
| Checkpoint write | SQLite transaction fault | Closed: no partial phase or event | `familiar-ai-storage/security_burn_in::failed_phase_transaction_cannot_fabricate_completion` |
| Model execution recovery | termination immediately before or after invocation behind a durable claimed checkpoint | Closed on uncertain start; resume never invokes the model twice | `familiar-ai-daemon::run::tests::production_recovery_never_reinvokes_model_across_durable_claim` |
| Checkpoint replay | identical phase transition | One event; no repeated execution evidence | `familiar-ai-storage/security_burn_in::checkpoint_replay_is_idempotent` |
| Database recovery | corrupt database bytes | Closed and reportable error | `familiar-ai-storage/security_burn_in::corrupt_database_is_reported_not_reinitialized` |
| Delivery intent | retry after uncertain external result | One stable effect identity | `familiar-ai-storage/security_burn_in::external_effect_intent_is_idempotent` |
| Delivery journal | truncated JSON | Closed before provider command | `familiar-ai-daemon/security_burn_in::corrupt_delivery_journal_runs_no_external_command` |
| Ambiguous delivery recovery | provider accepts PR create but its response is lost; lookup then restart | Lookup reconciles the PR; no duplicate commit, push, PR create, or other external effect | `familiar-ai-daemon/security_burn_in::ambiguous_pr_create_is_reconciled_without_repeating_external_effects` |
| Delivery recovery | replay after published phase | No duplicate commit, push, or PR create | `familiar-ai-daemon/security_burn_in::published_delivery_resume_skips_prior_external_effects` |
| Supervisor | repeated failure | Five starts per 300 seconds, finite delay, durable logs | `familiar-ai-daemon/security_burn_in::restart_storm_is_bounded_and_visible` |
| Privilege boundary | unsafe service label/path/newline | Closed before installation | `familiar-ai-daemon/security_burn_in::supervisor_injection_is_rejected_or_escaped` |
| Credentials / prompt and logs | credential canary in hostile agent output | Canary absent from prompt bytes and captured structured-output log | `familiar-ai-agent/security_burn_in::hostile_agent_output_is_redacted_from_captured_log` |
| Credentials / reports, database, comments | credential canary in hostile provider output during reporting and failed delivery | Canary absent from rendered report, SQLite bytes/rows, and captured provider comment argv | `familiar-ai-daemon/security_burn_in::hostile_provider_output_is_redacted_from_reports_database_rows_and_comments` |
| Credentials / supervisor | credential canary in environment | Canary absent from generated service definition | `familiar-ai-daemon/security_burn_in::supervisor_does_not_copy_ambient_secrets` |

Network partitions and provider rate limits are represented by runner failures
before and after the journaled delivery boundary. Process kill and reboot are
represented by truncated streams and replay from durable checkpoint/delivery
phases. Disk and database faults are represented by failed atomic transactions,
unwritable/corrupt journals, and corrupt SQLite input. These deterministic
equivalents make the suite safe and repeatable while exercising the same
recovery decisions.
