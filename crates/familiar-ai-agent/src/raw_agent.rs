//! PRD-100: `RawAgent` bridges the PRD-058 [`InferenceAdapter`] contract into
//! the [`CodingAgent`] trait every stage-routing/execution path already
//! knows how to select and run. This is the single missing bridge that lets
//! Familiar's own raw-model agent loop (`crate::raw_runtime`) implement a
//! PRD cycle without a vendor CLI: nothing in `raw_runtime` changes, and
//! nothing here re-implements it — `RawAgent::execute` composes the loop
//! core with host-supplied resources and translates the result back into
//! the [`ExecutionResult`]/[`AgentExecutionError`] vocabulary every other
//! `CodingAgent` already uses.
//!
//! `familiar-ai-agent` stays storage-agnostic (no SQLite, no filesystem
//! sandbox policy owned here): every execution-scoped resource — the
//! durable write-ahead journal, the sandboxed tool executor, the PRD-013
//! write-scope authorizer, the PRD-064 budget-reservation gate, and PRD-051
//! evidence/usage persistence — is injected through [`RawAgentHost`], whose
//! only concrete implementation lives in
//! `familiar_ai_daemon::agent_runtime::SqliteRawAgentHost`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use familiar_ai_llm::attempt::{AttemptId, InferenceAdapter};

use crate::raw_runtime::{
    run_loop, AuthorityContext, AuthorizationDecision, AuthorizationRefusal, CancellationToken,
    CapabilityId, LoopCeilings, LoopConfig, RefusalContinuation, RunOutcome, StablePrefix,
    StopReason, ToolAuthorizer, ToolExecutor, ToolJournal, ValidatedCall, VolatileTask,
};
use crate::{
    AgentExecutionError, BudgetCapability, CodingAgent, ExecutionRequest, ExecutionResult,
    FilesystemPolicy,
};

/// Host-supplied, execution-scoped resources a [`RawAgent`] needs beyond
/// what `CodingAgent::execute`'s fixed signature carries. Every method here
/// mirrors the authority the CLI-driven path is bound by
/// (`docs/contracts/agent-loop.md`): PRD-013 write-scope authorization, the
/// sandboxed executor, the write-ahead tool journal, a per-execution budget
/// reservation, and PRD-051 usage attribution.
///
/// Methods return owned, `'static` trait objects rather than borrowing so
/// implementations backed by a real database connection (which
/// `familiar-ai-agent` must never depend on directly) can open and close a
/// connection per call without fighting a borrow checked against this
/// trait's lifetime.
pub trait RawAgentHost: Send + Sync {
    fn journal(&self) -> Box<dyn ToolJournal>;

    /// Confined to `working_directory`, not a root captured when the host
    /// was constructed: the isolated-review contract runs a stage against a
    /// freshly created temporary workspace distinct from the repository
    /// worktree every other stage uses, and the executor's containment root
    /// must agree with the `RequestScopedAuthorizer`'s own
    /// `denied_read_path`/`working_directory` view of the same call — both
    /// derive from [`crate::ExecutionRequest::working_directory`], never
    /// from host construction state.
    fn executor(&self, working_directory: &std::path::Path) -> Box<dyn ToolExecutor>;
    fn authorizer(&self) -> Box<dyn ToolAuthorizer>;
    fn authority(&self) -> AuthorityContext;

    /// Reserves this execution's budget before the loop is ever allowed to
    /// submit a single inference attempt. `run_loop`'s own `mint_attempt_id`
    /// hook cannot itself refuse a submission (it is infallible by
    /// contract), so "a budget reservation per attempt" is enforced the only
    /// way that is structurally possible without changing the loop core:
    /// one reservation, acquired before the loop runs at all, that every
    /// attempt this call may make draws against. `Err` refuses the whole
    /// execution closed — zero attempts run without a reservation.
    fn reserve_execution_budget(&self, max_cost_microusd: Option<u64>) -> Result<(), String>;

    /// Resolves the budget reservation and persists the run's evidence and
    /// PRD-051 usage once the loop reaches a terminal stop reason.
    /// `effective_model` is the model the attempts actually ran against —
    /// the request's override when one was supplied, else the worker's
    /// configured model — so accounting names the model that was billed,
    /// never the one that happened to be configured.
    fn finish(&self, outcome: &RunOutcome, effective_model: &str) -> Result<(), String>;

    /// Releases this execution's budget reservation when the loop never
    /// reached a terminal outcome — any exit between
    /// `reserve_execution_budget` and `finish`, including a panic. Nothing
    /// ran, so nothing is settled and no run outcome is persisted; the
    /// reservation is released outright.
    fn abandon_execution(&self, detail: &str) -> Result<(), String>;
}

/// Releases the execution's reservation on every exit path that does not
/// reach `finish`. Armed when the reservation is acquired, disarmed
/// immediately before `finish` is called; a `?`, an early `return`, or an
/// unwinding panic between those two points drops it armed. This is what
/// makes "no reservation outlives the execution that acquired it" hold by
/// construction rather than by every future fallible step remembering to
/// release.
struct ReservationGuard<'a> {
    host: &'a dyn RawAgentHost,
    armed: bool,
}

impl Drop for ReservationGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(detail) = self
                .host
                .abandon_execution("execution aborted before the loop reached a terminal outcome")
            {
                tracing::warn!(
                    detail = %detail,
                    "failed to release an abandoned execution's budget reservation"
                );
            }
        }
    }
}

/// Everything a [`crate::AdapterFactory`] needs to construct one [`RawAgent`]
/// for a specific selected worker and execution: the host, the resolved
/// credential/endpoint its `InferenceAdapter` needs, and this stage's
/// declared ceilings/offered capabilities. Built fresh per execution by the
/// daemon crate (`familiar_ai_daemon::run`), never cached across PRD
/// attempts, since `execution_id` and the PRD-013 write-scope it derives
/// from are themselves per-attempt facts.
pub struct RawWorkerContext {
    pub host: Arc<dyn RawAgentHost>,
    pub ceilings: LoopCeilings,
    pub offered_capabilities: Vec<CapabilityId>,
    pub prompt_template_version: String,
    /// A hosted API's resolved API key/token, already fetched via PRD-074's
    /// credential-store descriptors. `None` for a local endpoint that needs
    /// no credential (e.g. an unauthenticated loopback Ollama server).
    pub credential: Option<RawCredential>,
    /// The PRD-063 local endpoint a local (`ollama`/`unsloth`) runtime
    /// reaches. `None` for a hosted API runtime.
    pub local_endpoint: Option<familiar_ai_core::config::LocalEndpointConfig>,
}

/// A PRD-074 credential-store value, held only long enough to construct one
/// `InferenceAdapter`'s own resolver/token. Mirrors the daemon crate's
/// `ResolvedCredential` (redacted `Debug`, exposed only through a named
/// accessor) at the `familiar-ai-agent` boundary, since `RawWorkerContext`
/// cannot depend on the daemon crate that resolves the credential in the
/// first place. The backing bytes are overwritten before the allocation is
/// freed — the credential is not left sitting in freed memory once this
/// value (and every `String` copied from it at request-construction time)
/// goes out of scope.
pub struct RawCredential(String);

impl RawCredential {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose_for_request(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RawCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RawCredential([REDACTED])")
    }
}

impl Drop for RawCredential {
    fn drop(&mut self) {
        // SAFETY: overwriting every byte with 0 keeps the buffer valid UTF-8
        // (NUL is a one-byte code point), so the string's own invariant
        // holds for the instant before it is deallocated.
        for byte in unsafe { self.0.as_bytes_mut() } {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::hint::black_box(&self.0);
    }
}

/// The fixed, per-worker facts a [`RawAgent`] needs on every call. Distinct
/// from [`RawWorkerContext`] only in that these come straight from the
/// worker's own PRD-057 spec identity rather than the execution the worker
/// was selected for.
pub struct RawAgentSpec {
    pub worker_spec_identity: String,
    pub worker_empirical_version: String,
    pub model: String,
    pub prompt_template_version: String,
    pub ceilings: LoopCeilings,
    pub offered_capabilities: Vec<CapabilityId>,
}

/// PRD-100 remediation (N2): [`ExecutionRequest`] carries per-execution
/// isolation the CLI-driven adapters (`ClaudeCodeAgent`/`CodexAgent`) honour
/// at the OS-process boundary — `denied_read_path` (a tree the caller has
/// ruled out for this stage, e.g. the original repository during isolated
/// review) and `filesystem` (`FilesystemPolicy::ReadOnly` disabling writes
/// for the call). `RawAgent`'s tool calls never cross a process boundary, so
/// there is no OS sandbox to hand these to; instead this wraps the
/// host-supplied [`ToolAuthorizer`] and enforces both at the same
/// authorize-before-execute chokepoint every other refusal in this loop goes
/// through, so a denied read or a write attempted under a read-only policy
/// is refused with a named diagnostic and journaled — never silently
/// dropped.
struct RequestScopedAuthorizer {
    inner: Box<dyn ToolAuthorizer>,
    /// Checked against every capability that names a filesystem target —
    /// `read-file`/`search-list`'s `path`, `apply-edit`'s `path`, and
    /// `run-command`'s `working_directory`/`argv` — not only the two read
    /// capabilities the name suggests. Canonicalized best-effort; falls back
    /// to the as-given path when the target does not exist yet, since the
    /// goal is containment, not existence.
    denied_read_path: Option<PathBuf>,
    working_directory: PathBuf,
    filesystem: FilesystemPolicy,
}

impl RequestScopedAuthorizer {
    /// Resolves `candidate_path` (interpreted relative to this request's
    /// `working_directory`, exactly like the executor will) and reports
    /// whether it lands inside `denied`. Best-effort canonicalization: a
    /// path that does not exist yet is compared lexically, since the goal is
    /// containment, not existence.
    fn resolves_into_denied(&self, denied: &std::path::Path, candidate_path: &str) -> bool {
        let candidate = self.working_directory.join(candidate_path);
        let resolved = candidate.canonicalize().unwrap_or(candidate);
        resolved.starts_with(denied)
    }

    /// Every argument shape across the canonical capability vocabulary that
    /// names a filesystem target, checked against `denied`. A capability
    /// this function does not recognise, or a named target this function
    /// cannot extract from a schema-valid call, refuses rather than falling
    /// through unchecked — the same fail-closed rule `search-list`'s
    /// optional `path` already followed.
    fn escapes_denied_tree(&self, denied: &std::path::Path, call: &ValidatedCall) -> bool {
        match call.capability {
            CapabilityId::ReadFile | CapabilityId::ApplyEdit => {
                match call.arguments.get("path").and_then(|v| v.as_str()) {
                    Some(path) => self.resolves_into_denied(denied, path),
                    // Both capabilities declare `path` required; a
                    // schema-valid call missing it is not a shape this
                    // authorizer can reason about — fail closed.
                    None => true,
                }
            }
            CapabilityId::SearchList => {
                match call.arguments.get("path").and_then(|v| v.as_str()) {
                    Some(path) => self.resolves_into_denied(denied, path),
                    // `search-list`'s `path` is optional; an absent path
                    // searches from the worktree root, which necessarily
                    // walks the denied subtree too.
                    None => true,
                }
            }
            CapabilityId::RunCommand => {
                let working_directory_escapes = match call
                    .arguments
                    .get("working_directory")
                    .and_then(|v| v.as_str())
                {
                    Some(dir) => self.resolves_into_denied(denied, dir),
                    // No explicit working_directory means the command runs
                    // in this request's own working_directory, which is
                    // never itself the denied tree (the caller sets one or
                    // the other).
                    None => false,
                };
                let argv_escapes = match call.arguments.get("argv").and_then(|v| v.as_array()) {
                    Some(argv) => argv
                        .iter()
                        .filter_map(|item| item.as_str())
                        // argv[0] is the command name (checked against the
                        // command allowlist elsewhere), not a filesystem
                        // target.
                        .skip(1)
                        .any(|argument| self.resolves_into_denied(denied, argument)),
                    // `argv` is required; a schema-valid call always has it.
                    None => true,
                };
                working_directory_escapes || argv_escapes
            }
            _ => false,
        }
    }
}

impl ToolAuthorizer for RequestScopedAuthorizer {
    fn authorize(&self, call: &ValidatedCall, ctx: &AuthorityContext) -> AuthorizationDecision {
        if self.filesystem == FilesystemPolicy::ReadOnly
            && matches!(
                call.capability,
                CapabilityId::ApplyEdit | CapabilityId::RunCommand
            )
        {
            return AuthorizationDecision::Refused {
                reason: AuthorizationRefusal::OutOfAuthorityScope {
                    capability: call.capability,
                },
                continuation: RefusalContinuation::InformModelAndContinue,
            };
        }
        if let Some(denied) = &self.denied_read_path {
            if self.escapes_denied_tree(denied, call) {
                return AuthorizationDecision::Refused {
                    reason: AuthorizationRefusal::OutOfAuthorityScope {
                        capability: call.capability,
                    },
                    continuation: RefusalContinuation::InformModelAndContinue,
                };
            }
        }
        self.inner.authorize(call, ctx)
    }
}

/// Bridges a PRD-058 [`InferenceAdapter`] into [`CodingAgent`] over the
/// existing `raw_runtime::run_loop` core. Constructed fresh per execution by
/// an [`crate::AdapterFactory`] (`familiar_ai_agent::registry`); holds no
/// mutable state of its own beyond what `Arc`-shared, interior-mutable
/// `host` resources own.
pub struct RawAgent {
    adapter: Arc<dyn InferenceAdapter>,
    host: Arc<dyn RawAgentHost>,
    spec: RawAgentSpec,
}

impl RawAgent {
    pub fn new(
        adapter: Arc<dyn InferenceAdapter>,
        host: Arc<dyn RawAgentHost>,
        spec: RawAgentSpec,
    ) -> Self {
        Self {
            adapter,
            host,
            spec,
        }
    }

    fn execution_result(&self, outcome: &RunOutcome, effective_model: &str) -> ExecutionResult {
        let usage = outcome.attempts.iter().fold(
            Default::default(),
            |acc: familiar_ai_llm::attempt::UsageCategories, attempt| acc.merge(&attempt.usage),
        );
        ExecutionResult {
            agent_version: Some(self.spec.worker_empirical_version.clone()),
            model: Some(effective_model.to_owned()),
            input_tokens: usage.uncached_input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            model_usage: Vec::new(),
            exit_code: None,
            signal: None,
            session_id: None,
            reported_cost_microusd: None,
            reported_cost_usd_lexical: None,
            accounting_source_hash: None,
        }
    }
}

impl CodingAgent for RawAgent {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let max_cost_microusd = request.budget.max_cost_microusd.map(|value| value.get());
        self.host
            .reserve_execution_budget(max_cost_microusd)
            .map_err(|detail| {
                tracing::warn!(
                    detail = %detail,
                    "budget reservation refused before any raw-loop attempt ran"
                );
                AgentExecutionError::BudgetStopped {
                    result: Box::new(ExecutionResult {
                        agent_version: Some(self.spec.worker_empirical_version.clone()),
                        ..ExecutionResult::default()
                    }),
                }
            })?;
        // Armed from here until the line before `finish`. Every early exit
        // below — the runtime builder failing, a panic in a host hook —
        // releases the reservation through the guard's `Drop`.
        let mut reservation_guard = ReservationGuard {
            host: self.host.as_ref(),
            armed: true,
        };

        // Computed exactly once and used for the loop, the result, and the
        // ledger, so the model named in accounting is the model the attempt
        // ran on (remediation of `raw-agent-model-override-not-attributed`).
        let effective_model = request
            .model
            .map(str::to_owned)
            .unwrap_or_else(|| self.spec.model.clone());

        let mut journal = self.host.journal();
        let mut executor = self.host.executor(request.working_directory);
        let authorizer: Box<dyn ToolAuthorizer> = Box::new(RequestScopedAuthorizer {
            inner: self.host.authorizer(),
            denied_read_path: request
                .denied_read_path
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf())),
            working_directory: request.working_directory.to_path_buf(),
            filesystem: request.filesystem,
        });
        let authority = self.host.authority();
        let execution_id = authority.execution_id.clone();
        // Distinguishes repeated `execute()` calls that share one
        // `execution_id` (a remediation round, a review re-run): without
        // this, `attempt_counter` below restarts at 1 on every call and a
        // second `execute()` mints the exact same `AttemptId`s the first
        // one did, colliding on `agent_runtime_attempts`' primary key.
        let attempt_scope = authority.attempt_id.clone();

        // A per-request timeout narrows the worker's own configured ceiling
        // rather than replacing it outright — either one stopping the loop
        // is a legitimate `StopReason::Timeout`.
        let max_wall_clock_ms = match (self.spec.ceilings.max_wall_clock_ms, request.timeout_ms) {
            (Some(configured), Some(requested)) => Some(configured.min(requested)),
            (Some(configured), None) => Some(configured),
            (None, Some(requested)) => Some(requested),
            (None, None) => None,
        };
        let ceilings = LoopCeilings {
            max_wall_clock_ms,
            ..self.spec.ceilings
        };

        let config = LoopConfig {
            worker_spec_identity: self.spec.worker_spec_identity.clone(),
            worker_empirical_version: self.spec.worker_empirical_version.clone(),
            model: effective_model.clone(),
            prompt_template_version: self.spec.prompt_template_version.clone(),
            ceilings,
            offered_capabilities: self.spec.offered_capabilities.clone(),
            structured_output: None,
            authority,
        };

        // PRD-029 prompt-caching economics for raw providers is explicitly
        // out of scope for this bridge (PRD-100 scope section); the whole
        // rendered prompt is the volatile task and the stable prefix is
        // empty, which `raw_runtime::run_loop` already treats as
        // `CacheControl::None`.
        let prefix = StablePrefix {
            bytes: String::new(),
            version: request.prompt_cache_key.unwrap_or("none").to_string(),
        };
        let task = VolatileTask {
            bytes: request.prompt.to_string(),
        };

        let mut attempt_counter = 0u32;
        let mint_attempt_id = move || {
            attempt_counter += 1;
            AttemptId(format!("{execution_id}:{attempt_scope}:{attempt_counter}"))
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AgentExecutionError::Launch {
                executable: "raw-agent-loop".into(),
                source: Box::new(error),
                result: Box::new(ExecutionResult::default()),
            })?;

        let outcome = runtime.block_on(run_loop(
            self.adapter.as_ref(),
            executor.as_mut(),
            authorizer.as_ref(),
            journal.as_mut(),
            &CancellationToken::new(),
            &prefix,
            &task,
            &config,
            mint_attempt_id,
        ));

        let _ = write!(output, "{}", outcome.final_text.as_deref().unwrap_or(""));

        // The loop reached a terminal outcome; `finish` now owns the
        // reservation's settlement.
        reservation_guard.armed = false;
        if let Err(detail) = self.host.finish(&outcome, &effective_model) {
            return Err(AgentExecutionError::Output {
                source: Box::new(std::io::Error::other(detail)),
                result: Box::new(self.execution_result(&outcome, &effective_model)),
            });
        }

        let result = self.execution_result(&outcome, &effective_model);
        match outcome.stop_reason {
            StopReason::Completed { .. } => Ok(result),
            StopReason::Timeout => Err(AgentExecutionError::Timeout {
                result: Box::new(result),
            }),
            StopReason::BudgetStop => Err(AgentExecutionError::BudgetStopped {
                result: Box::new(result),
            }),
            other => Err(AgentExecutionError::MalformedOutput {
                detail: format!("raw agent loop stopped without completing: {other:?}"),
                result: Box::new(result),
            }),
        }
    }

    fn budget_capability(&self) -> BudgetCapability {
        BudgetCapability {
            cost: true,
            tokens: true,
            duration: true,
            cost_always_zero: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw_runtime::{
        ExecutionError, ExecutionOutcome, InMemoryToolJournal, ScopeAuthorizer, ValidatedCall,
    };
    use crate::{ExecutionBudget, FilesystemPolicy};
    use familiar_ai_llm::attempt::{
        AdapterStopReason, FakeInferenceAdapter, ScriptedTurn, StreamEvent, SubmitOutcome,
        UsageCategories,
    };
    use std::sync::Mutex;

    struct NoopExecutor;
    impl ToolExecutor for NoopExecutor {
        fn execute(
            &mut self,
            _call: &ValidatedCall,
            _ctx: &AuthorityContext,
        ) -> Result<ExecutionOutcome, ExecutionError> {
            Ok(ExecutionOutcome {
                result_text: "ok".into(),
                result_hash: "h".into(),
            })
        }
    }

    struct TestHost {
        reservation_allowed: bool,
        granted_capabilities: Vec<CapabilityId>,
        allowed_write_paths: Vec<String>,
        allowed_commands: Vec<String>,
        finish_called: Mutex<Option<StopReason>>,
        finish_model: Mutex<Option<String>>,
        abandoned: Mutex<Option<String>>,
        panic_in_journal: bool,
        last_calls: Mutex<Vec<crate::raw_runtime::CallRecord>>,
        executor_working_directories: Mutex<Vec<PathBuf>>,
    }

    impl TestHost {
        fn new(reservation_allowed: bool) -> Self {
            Self::new_with_scope(
                reservation_allowed,
                vec![
                    CapabilityId::ReportProgress,
                    CapabilityId::ReadFile,
                    CapabilityId::SearchList,
                ],
                vec![],
                vec![],
            )
        }

        /// Lets a test grant the inner (host-supplied) authorizer wide-open
        /// scope for a capability, so a refusal in that test can only be
        /// coming from `RequestScopedAuthorizer`'s own `denied_read_path`
        /// containment check, never from the inner `ScopeAuthorizer` lacking
        /// a grant.
        fn new_with_scope(
            reservation_allowed: bool,
            granted_capabilities: Vec<CapabilityId>,
            allowed_write_paths: Vec<String>,
            allowed_commands: Vec<String>,
        ) -> Self {
            Self {
                reservation_allowed,
                granted_capabilities,
                allowed_write_paths,
                allowed_commands,
                finish_called: Mutex::new(None),
                finish_model: Mutex::new(None),
                abandoned: Mutex::new(None),
                panic_in_journal: false,
                last_calls: Mutex::new(Vec::new()),
                executor_working_directories: Mutex::new(Vec::new()),
            }
        }
    }

    impl RawAgentHost for TestHost {
        fn journal(&self) -> Box<dyn ToolJournal> {
            if self.panic_in_journal {
                panic!("injected host failure after the reservation was acquired");
            }
            Box::new(InMemoryToolJournal::default())
        }
        fn executor(&self, working_directory: &std::path::Path) -> Box<dyn ToolExecutor> {
            self.executor_working_directories
                .lock()
                .unwrap()
                .push(working_directory.to_path_buf());
            Box::new(NoopExecutor)
        }
        fn authorizer(&self) -> Box<dyn ToolAuthorizer> {
            Box::new(ScopeAuthorizer {
                granted_capabilities: self.granted_capabilities.clone(),
                allowed_write_paths: self.allowed_write_paths.clone(),
                allowed_commands: self.allowed_commands.clone(),
                network_allowed: false,
            })
        }
        fn authority(&self) -> AuthorityContext {
            AuthorityContext {
                project_id: "proj".into(),
                execution_id: "exec_1".into(),
                attempt_id: String::new(),
                worker_id: "worker".into(),
            }
        }
        fn reserve_execution_budget(&self, _max_cost_microusd: Option<u64>) -> Result<(), String> {
            if self.reservation_allowed {
                Ok(())
            } else {
                Err("no budget reservation available".into())
            }
        }
        fn finish(&self, outcome: &RunOutcome, effective_model: &str) -> Result<(), String> {
            *self.finish_called.lock().unwrap() = Some(outcome.stop_reason);
            *self.finish_model.lock().unwrap() = Some(effective_model.to_owned());
            *self.last_calls.lock().unwrap() = outcome.evidence.calls.clone();
            Ok(())
        }
        fn abandon_execution(&self, detail: &str) -> Result<(), String> {
            *self.abandoned.lock().unwrap() = Some(detail.to_owned());
            Ok(())
        }
    }

    fn spec() -> RawAgentSpec {
        RawAgentSpec {
            worker_spec_identity: "wspec-sha256:test".into(),
            worker_empirical_version: "wver-sha256:test".into(),
            model: "fake-model".into(),
            prompt_template_version: "agent-loop-prompt/1".into(),
            ceilings: LoopCeilings::default(),
            offered_capabilities: vec![CapabilityId::ReportProgress],
        }
    }

    fn base_request(dir: &std::path::Path) -> ExecutionRequest<'_> {
        ExecutionRequest {
            working_directory: dir,
            denied_read_path: None,
            prompt: "implement it",
            prompt_cache_key: None,
            codex_session: None,
            filesystem: FilesystemPolicy::Normal,
            model: None,
            timeout_ms: None,
            budget: ExecutionBudget::default(),
        }
    }

    #[test]
    fn completed_run_produces_ok_result_and_calls_finish() {
        let adapter = Arc::new(FakeInferenceAdapter::new(vec![ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories {
                    output_tokens: Some(3),
                    ..Default::default()
                },
                provider_request_id: Some("req_1".into()),
                provider_idempotency_key: None,
            }),
        }]));
        let host = Arc::new(TestHost::new(true));
        let agent = RawAgent::new(adapter, host.clone(), spec());
        let temp = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let result = agent
            .execute(base_request(temp.path()), &mut output)
            .unwrap();
        assert_eq!(result.output_tokens, Some(3));
        assert_eq!(
            *host.finish_called.lock().unwrap(),
            Some(StopReason::Completed {
                structured_output: false
            })
        );
        assert_eq!(
            host.abandoned.lock().unwrap().as_deref(),
            None,
            "a run that reached finish must not also be abandoned"
        );
    }

    /// Remediation regression (`raw-agent-model-override-not-attributed`):
    /// an `ExecutionRequest` carrying `model: Some(..)` runs the loop against
    /// that model, so the `ExecutionResult` and the identity handed to the
    /// host's `finish` (and from there to PRD-051 accounting) must name it —
    /// not the worker's configured `spec.model`. Restoring `self.spec.model`
    /// in either place fails this test.
    #[test]
    fn a_request_model_override_is_the_model_attributed_everywhere() {
        let adapter = Arc::new(FakeInferenceAdapter::new(vec![ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories {
                    output_tokens: Some(1),
                    ..Default::default()
                },
                provider_request_id: Some("req_override".into()),
                provider_idempotency_key: None,
            }),
        }]));
        let host = Arc::new(TestHost::new(true));
        let agent = RawAgent::new(adapter, host.clone(), spec());
        let temp = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let request = ExecutionRequest {
            model: Some("override-model"),
            ..base_request(temp.path())
        };
        let result = agent.execute(request, &mut output).unwrap();
        assert_eq!(
            result.model.as_deref(),
            Some("override-model"),
            "the result must name the model the attempt ran on"
        );
        assert_eq!(
            host.finish_model.lock().unwrap().as_deref(),
            Some("override-model"),
            "accounting must be handed the model the attempt ran on"
        );
        assert_ne!(spec().model, "override-model");
    }

    /// Remediation regression (`reservation-leaked-on-pre-loop-failure`):
    /// once the reservation is acquired, every exit that does not reach
    /// `finish` must release it. The failure injected here is a panic in a
    /// host hook after `reserve_execution_budget` succeeded — the same shape
    /// as the tokio runtime builder failing, which cannot be provoked from a
    /// test. Removing the guard's release fails this test.
    #[test]
    fn a_failure_after_the_reservation_and_before_the_loop_releases_it() {
        let adapter = Arc::new(FakeInferenceAdapter::new(vec![]));
        let mut fixture = TestHost::new(true);
        fixture.panic_in_journal = true;
        let host = Arc::new(fixture);
        let agent = RawAgent::new(adapter, host.clone(), spec());
        let temp = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = agent.execute(base_request(temp.path()), &mut output);
        }));
        assert!(unwound.is_err(), "the injected host failure must unwind");
        assert!(
            host.abandoned.lock().unwrap().is_some(),
            "a reservation acquired by an execution that never reached finish must be released"
        );
        assert_eq!(
            *host.finish_called.lock().unwrap(),
            None,
            "finish must not be reported for a loop that never ran"
        );
    }

    /// Remediation regression (`raw-agent-ignores-request-working-directory`):
    /// the isolated-review contract runs a stage against a freshly created
    /// temporary workspace that is never the repository worktree the host
    /// was constructed against — `StructuredReviewAdapter::review`
    /// (`familiar-ai-review`) sets `working_directory` to a fresh temp dir
    /// per call, distinct from every other stage's worktree. Before this
    /// fix, `RawAgentHost::executor()` took no request state, so whatever
    /// root the host was built with was the only root the tool executor
    /// could ever confine to; a stage whose `working_directory` diverges
    /// from that root had no way to make the executor agree. This proves
    /// `RawAgent::execute` hands the executor exactly the calling request's
    /// own `working_directory` — not a fixed value baked in at host
    /// construction — across two calls on one host with two different,
    /// unrelated directories, matching how one `SqliteRawAgentHost` per
    /// stage is reused across an implementation call and a later isolated
    /// review call in production.
    #[test]
    fn executor_root_tracks_the_calling_requests_working_directory() {
        fn scripted_turn() -> ScriptedTurn {
            ScriptedTurn {
                events: vec![StreamEvent::TextDelta("done".into())],
                outcome: Ok(SubmitOutcome {
                    stop_reason: AdapterStopReason::EndTurn,
                    usage: UsageCategories::default(),
                    provider_request_id: None,
                    provider_idempotency_key: None,
                }),
            }
        }
        // One turn per `execute()` call below: `FakeInferenceAdapter`
        // consumes its scripted turns sequentially across every `submit`
        // call `run_loop` makes, regardless of which `execute()` call it
        // came from — this mirrors one host being reused across two calls
        // on the same stage (a review re-run producing a second fresh
        // temporary workspace, exactly as `StructuredReviewAdapter::review`
        // does in production).
        let adapter = Arc::new(FakeInferenceAdapter::new(vec![
            scripted_turn(),
            scripted_turn(),
        ]));
        let host = Arc::new(TestHost::new(true));
        let agent = RawAgent::new(adapter, host.clone(), spec());

        let implementation_dir = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        agent
            .execute(base_request(implementation_dir.path()), &mut output)
            .unwrap();

        let review_workspace = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        agent
            .execute(base_request(review_workspace.path()), &mut output)
            .unwrap();

        assert_ne!(implementation_dir.path(), review_workspace.path());
        assert_eq!(
            *host.executor_working_directories.lock().unwrap(),
            vec![
                implementation_dir.path().to_path_buf(),
                review_workspace.path().to_path_buf(),
            ],
            "each call's executor must be confined to that call's own request.working_directory"
        );
    }

    /// PRD-100 acceptance criterion: an attempt without a reservation cannot
    /// run. `reserve_execution_budget` refusing must stop the whole
    /// execution before `run_loop` ever calls `InferenceAdapter::submit`.
    #[test]
    fn no_reservation_means_the_adapter_is_never_called() {
        let adapter = Arc::new(FakeInferenceAdapter::new(vec![ScriptedTurn {
            events: vec![StreamEvent::TextDelta("done".into())],
            outcome: Ok(SubmitOutcome {
                stop_reason: AdapterStopReason::EndTurn,
                usage: UsageCategories::default(),
                provider_request_id: None,
                provider_idempotency_key: None,
            }),
        }]));
        let host = Arc::new(TestHost::new(false));
        let agent = RawAgent::new(adapter.clone(), host.clone(), spec());
        let temp = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let error = agent
            .execute(base_request(temp.path()), &mut output)
            .unwrap_err();
        assert!(matches!(error, AgentExecutionError::BudgetStopped { .. }));
        assert_eq!(
            adapter.remaining_turns(),
            1,
            "the adapter must never be submitted to without a reservation"
        );
        assert!(host.finish_called.lock().unwrap().is_none());
    }

    /// N2 regression: `ExecutionRequest::denied_read_path` names a tree the
    /// caller has ruled out for this stage (the isolated-review contract).
    /// CLI-driven adapters enforce this at the OS-process sandbox boundary;
    /// before this fix `RawAgent::execute` never read the field at all, so a
    /// model-issued `read-file` targeting the denied tree was silently
    /// served. It must instead be refused at the same authorize-before-
    /// execute chokepoint every other refusal in this loop goes through, and
    /// the refusal must be durably recorded.
    #[test]
    fn denied_read_path_refuses_a_read_targeting_it() {
        let temp = tempfile::tempdir().unwrap();
        let denied_dir = temp.path().join("denied");
        std::fs::create_dir(&denied_dir).unwrap();
        std::fs::write(denied_dir.join("secret.txt"), "top secret").unwrap();

        let adapter = Arc::new(FakeInferenceAdapter::new(vec![
            ScriptedTurn {
                events: vec![
                    StreamEvent::ToolCallDelta {
                        call_id: "call_1".into(),
                        capability_id: "read-file".into(),
                        arguments_fragment: "{\"path\":\"denied/secret.txt\"}".into(),
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
        ]));
        let host = Arc::new(TestHost::new(true));
        let mut spec = spec();
        spec.offered_capabilities = vec![CapabilityId::ReadFile, CapabilityId::ReportProgress];
        let agent = RawAgent::new(adapter, host.clone(), spec);

        let mut request = base_request(temp.path());
        request.denied_read_path = Some(&denied_dir);
        let mut output = Vec::new();
        agent
            .execute(request, &mut output)
            .expect("an authorization refusal is not itself an execution failure");

        let calls = host.last_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the read-file call must be recorded: {calls:?}"
        );
        assert!(
            matches!(
                calls[0].disposition,
                crate::raw_runtime::CallDisposition::AuthorizationRefused { .. }
            ),
            "a read targeting the denied path must be refused, not silently served: {:?}",
            calls[0].disposition
        );
    }

    /// `search-list`'s `path` argument is optional (`raw_runtime`'s
    /// `search-list/1` schema declares it under `optional`, not `required`):
    /// an absent `path` searches from the worktree root, walking the denied
    /// subtree along with everything else. Before this fix,
    /// `RequestScopedAuthorizer::authorize` only compared `denied_read_path`
    /// against an explicit `path` argument — a `search-list` call carrying no
    /// `path` at all fell through the `if let Some(path) = ...` guard
    /// untouched and reached the inner authorizer, which knows nothing about
    /// `denied_read_path`. This is exactly the isolated-review escape: a
    /// reviewer offered `SearchList` could enumerate the denied tree by
    /// simply never naming a `path`, while the equivalent unscoped
    /// `read-file` is refused because `path` is required for that
    /// capability.
    #[test]
    fn denied_read_path_refuses_a_pathless_search_list() {
        let temp = tempfile::tempdir().unwrap();
        let denied_dir = temp.path().join("denied");
        std::fs::create_dir(&denied_dir).unwrap();
        std::fs::write(denied_dir.join("secret.txt"), "top secret").unwrap();

        let adapter = Arc::new(FakeInferenceAdapter::new(vec![
            ScriptedTurn {
                events: vec![
                    StreamEvent::ToolCallDelta {
                        call_id: "call_1".into(),
                        capability_id: "search-list".into(),
                        arguments_fragment: "{\"query\":\"secret\"}".into(),
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
        ]));
        let host = Arc::new(TestHost::new(true));
        let mut spec = spec();
        spec.offered_capabilities = vec![CapabilityId::SearchList, CapabilityId::ReportProgress];
        let agent = RawAgent::new(adapter, host.clone(), spec);

        let mut request = base_request(temp.path());
        request.denied_read_path = Some(&denied_dir);
        let mut output = Vec::new();
        agent
            .execute(request, &mut output)
            .expect("an authorization refusal is not itself an execution failure");

        let calls = host.last_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the search-list call must be recorded: {calls:?}"
        );
        assert!(
            matches!(
                calls[0].disposition,
                crate::raw_runtime::CallDisposition::AuthorizationRefused { .. }
            ),
            "a pathless search-list while a denied path is set must fail closed, not fall \
             through to the inner authorizer: {:?}",
            calls[0].disposition
        );
    }

    /// `denied_read_path` used to be enforced only for `read-file` and
    /// `search-list`. An `apply-edit` targeting the denied tree fell through
    /// to the inner (host-supplied) authorizer, which knows nothing about
    /// `denied_read_path` — under `FilesystemPolicy::Normal` and a write
    /// scope that otherwise covers the target, the write would have
    /// succeeded. The inner authorizer here is granted `ApplyEdit` over the
    /// exact target path, so a refusal can only be coming from
    /// `RequestScopedAuthorizer`'s own containment check.
    #[test]
    fn denied_read_path_refuses_an_apply_edit_targeting_it() {
        let temp = tempfile::tempdir().unwrap();
        let denied_dir = temp.path().join("denied");
        std::fs::create_dir(&denied_dir).unwrap();
        std::fs::write(denied_dir.join("existing.txt"), "old").unwrap();

        let adapter = Arc::new(FakeInferenceAdapter::new(vec![
            ScriptedTurn {
                events: vec![
                    StreamEvent::ToolCallDelta {
                        call_id: "call_1".into(),
                        capability_id: "apply-edit".into(),
                        arguments_fragment:
                            "{\"path\":\"denied/existing.txt\",\"content\":\"new\"}".into(),
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
        ]));
        let host = Arc::new(TestHost::new_with_scope(
            true,
            vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress],
            vec!["denied/existing.txt".to_string()],
            vec![],
        ));
        let mut spec = spec();
        spec.offered_capabilities = vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress];
        let agent = RawAgent::new(adapter, host.clone(), spec);

        let mut request = base_request(temp.path());
        request.denied_read_path = Some(&denied_dir);
        let mut output = Vec::new();
        agent
            .execute(request, &mut output)
            .expect("an authorization refusal is not itself an execution failure");

        let calls = host.last_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the apply-edit call must be recorded: {calls:?}"
        );
        assert!(
            matches!(
                calls[0].disposition,
                crate::raw_runtime::CallDisposition::AuthorizationRefused { .. }
            ),
            "an apply-edit targeting the denied path must be refused, not silently applied: {:?}",
            calls[0].disposition
        );
        assert_eq!(
            std::fs::read_to_string(denied_dir.join("existing.txt")).unwrap(),
            "old",
            "a refused apply-edit must leave its target byte-identical"
        );
    }

    /// As above, for `run-command`: a command whose `argv` names a path
    /// inside the denied tree (the finding's own example — `cat`/`grep`
    /// against the denied tree) used to reach the inner authorizer
    /// unchecked. `cat` is on the allowlist here, so a refusal can only come
    /// from `RequestScopedAuthorizer`'s own containment check.
    #[test]
    fn denied_read_path_refuses_a_run_command_argv_targeting_it() {
        let temp = tempfile::tempdir().unwrap();
        let denied_dir = temp.path().join("denied");
        std::fs::create_dir(&denied_dir).unwrap();
        std::fs::write(denied_dir.join("secret.txt"), "top secret").unwrap();

        let adapter = Arc::new(FakeInferenceAdapter::new(vec![
            ScriptedTurn {
                events: vec![
                    StreamEvent::ToolCallDelta {
                        call_id: "call_1".into(),
                        capability_id: "run-command".into(),
                        arguments_fragment: "{\"argv\":[\"cat\",\"denied/secret.txt\"]}".into(),
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
        ]));
        let host = Arc::new(TestHost::new_with_scope(
            true,
            vec![CapabilityId::RunCommand, CapabilityId::ReportProgress],
            vec![],
            vec!["cat".to_string()],
        ));
        let mut spec = spec();
        spec.offered_capabilities = vec![CapabilityId::RunCommand, CapabilityId::ReportProgress];
        let agent = RawAgent::new(adapter, host.clone(), spec);

        let mut request = base_request(temp.path());
        request.denied_read_path = Some(&denied_dir);
        let mut output = Vec::new();
        agent
            .execute(request, &mut output)
            .expect("an authorization refusal is not itself an execution failure");

        let calls = host.last_calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the run-command call must be recorded: {calls:?}"
        );
        assert!(
            matches!(
                calls[0].disposition,
                crate::raw_runtime::CallDisposition::AuthorizationRefused { .. }
            ),
            "a run-command whose argv names a path inside the denied tree must be refused: {:?}",
            calls[0].disposition
        );
    }
}
