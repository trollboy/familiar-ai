//! PRD-058 daemon-side wiring for the Familiar-owned raw-model agent loop:
//! a SQLite-backed write-ahead tool journal, a sandboxed tool executor
//! (worktree-confined filesystem access, allowlisted subprocess commands,
//! scrubbed environment, process-group timeout/cancellation), a write-scope
//! authorizer derived from a PRD's own PRD-013 Expected Files contract, and
//! persistence of loop evidence and usage into the PRD-051 ledger.
//!
//! The loop core in `familiar_ai_agent::raw_runtime` never touches SQLite,
//! a subprocess, or the network directly; everything here is the concrete
//! implementation the loop core's traits are injected with.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::Utc;
use rusqlite::Connection;

use familiar_ai_agent::raw_runtime::{
    resume_decision_for, AttemptUsage, AuthorityContext, CallRecord, CapabilityId, ExecutionError,
    ExecutionOutcome, JournalIntent, JournalResult, OfferedTool, ResumeDecision, RunOutcome,
    ScopeAuthorizer, SideEffectClass, StopReason, ToolExecutor, ToolJournal, ValidatedCall,
};
#[cfg(unix)]
use familiar_ai_agent::{finish_watchdog, spawn_watchdog};
use familiar_ai_core::config::AgentRuntimeSandboxConfig;
use familiar_ai_review::parse_expected_files;
use familiar_ai_storage::repos::accounting::{AccountingRepository, UsageObservation};
use familiar_ai_storage::repos::agent_runtime::{AgentRuntimeRepository, ToolResultOutcome};
use familiar_ai_storage::repos::worker_selection::{
    WorkerSelectionRecord, WorkerSelectionRepository,
};

fn side_effect_str(class: SideEffectClass) -> &'static str {
    match class {
        SideEffectClass::ReadOnly => "read-only",
        SideEffectClass::IdempotentWrite => "idempotent-write",
        SideEffectClass::Destructive => "destructive",
    }
}

fn stop_reason_key(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Completed { .. } => "completed",
        StopReason::IterationCeiling => "iteration-ceiling",
        StopReason::TokenOrContextCeiling => "token-or-context-ceiling",
        StopReason::BudgetStop => "budget-stop",
        StopReason::Timeout => "timeout",
        StopReason::Cancelled => "cancelled",
        StopReason::ProviderFailure { .. } => "provider-failure",
        StopReason::FatalToolRefusal => "fatal-tool-refusal",
        StopReason::InvalidStructuredOutput => "invalid-structured-output",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn random_hex() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("secure random generation must succeed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------
// Write-ahead tool journal, backed by migration 055's append-only tables.
// ---------------------------------------------------------------------

pub struct SqliteToolJournal<'a> {
    conn: &'a Connection,
    execution_id: String,
}

impl<'a> SqliteToolJournal<'a> {
    pub fn new(conn: &'a Connection, execution_id: impl Into<String>) -> Self {
        Self {
            conn,
            execution_id: execution_id.into(),
        }
    }
}

impl ToolJournal for SqliteToolJournal<'_> {
    fn record_intent(&mut self, intent: &JournalIntent) -> Result<(), String> {
        AgentRuntimeRepository::new(self.conn)
            .record_tool_intent(
                &self.execution_id,
                &intent.call_id,
                intent.capability.as_str(),
                &intent.argument_hash,
                side_effect_str(intent.side_effect_class),
            )
            .map_err(|error| error.to_string())
    }

    fn record_result(&mut self, call_id: &str, result: &JournalResult) -> Result<(), String> {
        let outcome = match result {
            JournalResult::Succeeded { result_hash } => ToolResultOutcome::Succeeded {
                result_hash: result_hash.clone(),
            },
            JournalResult::Failed { detail } => ToolResultOutcome::Failed {
                detail: detail.clone(),
            },
        };
        AgentRuntimeRepository::new(self.conn)
            .record_tool_result(&self.execution_id, call_id, &outcome)
            .map_err(|error| error.to_string())
    }

    fn result_for(&self, call_id: &str) -> Option<JournalResult> {
        // A read failure here must never be treated as "no prior result": on
        // a resumed loop that would let a destructive call re-execute. Fail
        // loud instead of fail open.
        let result = AgentRuntimeRepository::new(self.conn)
            .tool_result(&self.execution_id, call_id)
            .expect("agent runtime tool journal must be readable to make a safe resume decision");
        result.map(|outcome| match outcome {
            ToolResultOutcome::Succeeded { result_hash } => {
                JournalResult::Succeeded { result_hash }
            }
            ToolResultOutcome::Failed { detail } => JournalResult::Failed { detail },
        })
    }

    fn len(&self) -> usize {
        AgentRuntimeRepository::new(self.conn)
            .intent_count(&self.execution_id)
            .unwrap_or(0) as usize
    }
}

// ---------------------------------------------------------------------
// Write-scope authorization derived from the PRD-013 Expected Files grammar
// ---------------------------------------------------------------------

/// Builds the PRD-058 write-scope authorizer directly from a PRD's own
/// `## Expected Files` section, reusing PRD-013's exact grammar and
/// normalization (`familiar_ai_review::expected_files::parse_expected_files`)
/// rather than a second heuristic. An `apply-edit` target that does not
/// match a declared entry is refused before any effect.
pub fn write_scope_authorizer_from_prd(
    prd_markdown: &str,
    granted_capabilities: Vec<CapabilityId>,
    sandbox: &AgentRuntimeSandboxConfig,
) -> Result<ScopeAuthorizer, String> {
    let entries = parse_expected_files(prd_markdown).map_err(|error| error.to_string())?;
    Ok(ScopeAuthorizer {
        granted_capabilities,
        allowed_write_paths: entries.into_iter().map(|entry| entry.normalized).collect(),
        allowed_commands: sandbox.allowed_commands.clone(),
        network_allowed: sandbox.network_allowed,
    })
}

// ---------------------------------------------------------------------
// Sandboxed tool executor
// ---------------------------------------------------------------------

fn hash_outcome(result_text: String) -> ExecutionOutcome {
    let result_hash = sha256_hex(result_text.as_bytes());
    ExecutionOutcome {
        result_text,
        result_hash,
    }
}

/// Executes canonical tool capabilities confined to one execution's
/// worktree. Filesystem capabilities are path-contained beneath
/// `worktree_root`; `run-command` is allowlist-gated, environment-scrubbed
/// (an explicit allowlist only — never the full process environment), and
/// killed by process group at the deadline (matching the isolation layer
/// used elsewhere in this crate for harness subprocesses).
pub struct SandboxedToolExecutor {
    pub worktree_root: PathBuf,
    pub sandbox: AgentRuntimeSandboxConfig,
    pub command_timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl SandboxedToolExecutor {
    fn resolve_within_worktree(&self, relative: &str) -> Result<PathBuf, ExecutionError> {
        if relative.is_empty()
            || Path::new(relative).is_absolute()
            || relative.split('/').any(|part| part == "..")
        {
            return Err(ExecutionError::Failed(format!(
                "path {relative:?} is not a safe worktree-relative path"
            )));
        }
        Ok(self.worktree_root.join(relative))
    }

    fn read_file(&self, call: &ValidatedCall) -> Result<ExecutionOutcome, ExecutionError> {
        let path = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let resolved = self.resolve_within_worktree(path)?;
        let content = std::fs::read_to_string(&resolved).map_err(|error| {
            ExecutionError::Failed(format!("read-file {path:?} failed: {error}"))
        })?;
        Ok(hash_outcome(content))
    }

    fn search_list(&self, call: &ValidatedCall) -> Result<ExecutionOutcome, ExecutionError> {
        let query = call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let subpath = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let root = if subpath.is_empty() {
            self.worktree_root.clone()
        } else {
            self.resolve_within_worktree(subpath)?
        };
        let mut matches = Vec::new();
        collect_matches(&root, &self.worktree_root, query, &mut matches, 500);
        Ok(hash_outcome(matches.join("\n")))
    }

    fn apply_edit(&self, call: &ValidatedCall) -> Result<ExecutionOutcome, ExecutionError> {
        let path = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let content = call
            .arguments
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let resolved = self.resolve_within_worktree(path)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ExecutionError::Failed(format!("apply-edit {path:?} failed: {error}"))
            })?;
        }
        std::fs::write(&resolved, content).map_err(|error| {
            ExecutionError::Failed(format!("apply-edit {path:?} failed: {error}"))
        })?;
        Ok(hash_outcome(format!(
            "wrote {} bytes to {path}",
            content.len()
        )))
    }

    fn run_command(&self, call: &ValidatedCall) -> Result<ExecutionOutcome, ExecutionError> {
        let argv: Vec<String> = call
            .arguments
            .get("argv")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let Some(program) = argv.first().cloned() else {
            return Err(ExecutionError::Failed(
                "run-command requires a non-empty argv".into(),
            ));
        };
        // Defense in depth: the authorizer already refused an unlisted
        // command before this call could reach the executor, but a command
        // never runs here without independently confirming the allowlist.
        if !self
            .sandbox
            .allowed_commands
            .iter()
            .any(|allowed| allowed == &program)
        {
            return Err(ExecutionError::Failed(format!(
                "command {program:?} is not in the sandbox allowlist"
            )));
        }
        let working_directory = match call
            .arguments
            .get("working_directory")
            .and_then(|v| v.as_str())
        {
            Some(sub) => self.resolve_within_worktree(sub)?,
            None => self.worktree_root.clone(),
        };

        let mut command = Command::new(&program);
        command.args(&argv[1..]);
        command.current_dir(&working_directory);
        // Deny-by-default environment: never inherit the daemon's own
        // process environment (which may carry inference/billing/admin
        // credentials); only explicitly allowlisted names cross in.
        command.env_clear();
        for name in &self.sandbox.allowed_environment {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|error| {
            ExecutionError::Failed(format!("failed to launch {program:?}: {error}"))
        })?;

        #[cfg(unix)]
        let watchdog = spawn_watchdog(child.id(), Some(self.command_timeout_ms));

        let mut stderr_pipe = child.stderr.take();
        let stderr_handle = std::thread::spawn(move || {
            let mut buffer = String::new();
            if let Some(mut pipe) = stderr_pipe.take() {
                let _ = pipe.read_to_string(&mut buffer);
            }
            buffer
        });
        let mut stdout = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut stdout);
        }
        let stderr = stderr_handle.join().unwrap_or_default();
        let status = child.wait().map_err(|error| {
            ExecutionError::Failed(format!("failed to wait for {program:?}: {error}"))
        })?;

        #[cfg(unix)]
        let timed_out = finish_watchdog(watchdog);
        #[cfg(not(unix))]
        let timed_out = false;

        if timed_out {
            return Err(ExecutionError::Timeout);
        }

        let mut combined = format!(
            "exit_status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        );
        if combined.len() > self.max_output_bytes {
            combined.truncate(self.max_output_bytes);
        }
        Ok(hash_outcome(combined))
    }
}

impl ToolExecutor for SandboxedToolExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        match call.capability {
            CapabilityId::ReadFile => self.read_file(call),
            CapabilityId::SearchList => self.search_list(call),
            CapabilityId::ApplyEdit => self.apply_edit(call),
            CapabilityId::RunCommand => self.run_command(call),
            // report-progress, submit-evidence, and request-escalation
            // never touch the filesystem or a subprocess: the tool journal
            // (intent + result, written by the loop core around this call)
            // is itself the durable record; this executor only
            // acknowledges receipt and never grants anything.
            CapabilityId::ReportProgress
            | CapabilityId::SubmitEvidence
            | CapabilityId::RequestEscalation => Ok(hash_outcome(format!(
                "acknowledged {}: {}",
                call.capability.as_str(),
                call.arguments
            ))),
        }
    }
}

fn collect_matches(
    dir: &Path,
    worktree_root: &Path,
    query: &str,
    out: &mut Vec<String>,
    limit: usize,
) {
    if out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if out.len() >= limit {
            return;
        }
        let relative = path
            .strip_prefix(worktree_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            if relative.split('/').next_back() == Some(".git") {
                continue;
            }
            collect_matches(&path, worktree_root, query, out, limit);
        } else if relative.contains(query) {
            out.push(relative);
        }
    }
}

// ---------------------------------------------------------------------
// Evidence and PRD-051 usage persistence
// ---------------------------------------------------------------------

fn offered_tools_json(tools: &[OfferedTool]) -> String {
    let items: Vec<serde_json::Value> = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "capability": tool.capability.as_str(),
                "schema_version": tool.schema_version,
            })
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
}

fn calls_json(calls: &[CallRecord]) -> String {
    let items: Vec<serde_json::Value> = calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "call_id": call.call_id,
                "capability_name": call.capability_name,
                "disposition": format!("{:?}", call.disposition),
            })
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
}

/// Persists one loop run's evidence and per-attempt PRD-051 usage.
/// `stage`/`worker_identity`/`adapter`/`model_identity` follow the same
/// vocabulary as every other execution's usage rows so the raw runtime
/// shares one ledger with harness-driven work. Never receives or stores a
/// prompt, a model response, source code, or raw tool output — only
/// capability ids, dispositions, and content hashes.
#[allow(clippy::too_many_arguments)]
pub fn persist_run_outcome(
    conn: &Connection,
    execution_id: &str,
    stage: &str,
    worker_identity: &str,
    adapter: &str,
    model_identity: Option<&str>,
    project_resolution_evidence: Option<&str>,
    outcome: &RunOutcome,
) -> familiar_ai_core::Result<()> {
    let agent_runtime_repo = AgentRuntimeRepository::new(conn);
    let accounting_repo = AccountingRepository::new(conn);
    let terminal_status = stop_reason_key(outcome.stop_reason);

    // The PRD-051 ledger resolves an observation's `spec_identity`/
    // `empirical_version` by joining the latest `worker_selections` row for
    // (execution_id, stage) — record one here so raw-runtime usage carries
    // the full PRD-057 spec identity exactly like harness-driven usage does.
    WorkerSelectionRepository::new(conn).record(&WorkerSelectionRecord {
        selection_id: &format!("agentsel_{}", random_hex()),
        execution_id: Some(execution_id),
        stage,
        rule: "raw-runtime",
        selected_identity: &outcome.evidence.worker_spec_identity,
        selected_empirical_version: &outcome.evidence.worker_empirical_version,
        candidates_json: "[]",
        risk_classes_json: "[]",
        expected_file_count: 0,
    })?;

    for AttemptUsage {
        attempt_id,
        usage,
        ambiguous,
        provider_request_id,
    } in &outcome.attempts
    {
        agent_runtime_repo.record_attempt(
            &attempt_id.0,
            execution_id,
            None,
            None,
            None,
            *ambiguous,
        )?;

        let unknown_reason = if *ambiguous {
            Some("provider timeout with unknown completion")
        } else if usage.is_entirely_unknown() {
            Some("adapter reported no usage for this attempt")
        } else {
            None
        };
        let now = Utc::now().to_rfc3339();
        let source_event_hash = sha256_hex(format!("{execution_id}:{}", attempt_id.0).as_bytes());
        accounting_repo.append_observation(&UsageObservation {
            execution_id,
            attempt_id: &attempt_id.0,
            stage,
            session_id: None,
            worker_identity,
            adapter,
            cli_version: None,
            model_identity,
            service_tier: None,
            provider_request_id: provider_request_id.as_deref(),
            uncached_input_tokens: usage.uncached_input_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            unknown_reason,
            period_start: &now,
            period_end: &now,
            terminal_status,
            source_event_hash: &source_event_hash,
            provider_cost_lexical: None,
            project_resolution_evidence,
            output_register_id: "raw-runtime-none",
            output_register_version: "raw-runtime-none",
            input_compression_id: "raw-runtime-none",
            input_compression_version: "raw-runtime-none",
            compression_experiment: None,
            compression_lane: None,
        })?;
    }

    let stop_reason_detail_json = match outcome.stop_reason {
        StopReason::Completed { structured_output } => {
            Some(serde_json::json!({"structured_output": structured_output}).to_string())
        }
        StopReason::ProviderFailure { taxonomy } => {
            Some(serde_json::json!({"taxonomy": format!("{taxonomy:?}")}).to_string())
        }
        _ => None,
    };
    agent_runtime_repo.record_evidence(
        execution_id,
        &outcome.evidence.prompt_template_version,
        &outcome.evidence.worker_spec_identity,
        &outcome.evidence.worker_empirical_version,
        &offered_tools_json(&outcome.evidence.offered_tools),
        &calls_json(&outcome.evidence.calls),
        terminal_status,
        stop_reason_detail_json.as_deref(),
        outcome.evidence.iterations,
        outcome.evidence.resume_point.conversation_messages as u64,
        outcome.evidence.resume_point.journal_high_water_mark as u64,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------
// Resume reconciliation
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeReadiness {
    Ready,
    /// A durable intent exists with no result and its side-effect class
    /// fails closed — a human must resolve this before the loop resumes.
    Blocked {
        call_id: String,
        capability: String,
    },
}

/// Reads migration 055's write-ahead journal for `execution_id` and applies
/// [`resume_decision_for`] to every intent-without-result. A single
/// destructive intent with no recorded result blocks resume; a resumed loop
/// never unknowingly repeats or silently skips it.
pub fn resume_readiness(
    conn: &Connection,
    execution_id: &str,
) -> familiar_ai_core::Result<ResumeReadiness> {
    let repo = AgentRuntimeRepository::new(conn);
    for intent in repo.pending_intents(execution_id)? {
        let side_effect_class = match intent.side_effect_class.as_str() {
            "read-only" => SideEffectClass::ReadOnly,
            "idempotent-write" => SideEffectClass::IdempotentWrite,
            _ => SideEffectClass::Destructive,
        };
        if resume_decision_for(side_effect_class, false) == ResumeDecision::FailClosed {
            return Ok(ResumeReadiness::Blocked {
                call_id: intent.call_id,
                capability: intent.capability,
            });
        }
    }
    Ok(ResumeReadiness::Ready)
}
