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
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::Connection;

use familiar_ai_agent::raw_agent::RawAgentHost;
use familiar_ai_agent::raw_runtime::{
    resume_decision_for, AttemptUsage, AuthorityContext, CallRecord, CapabilityId, ExecutionError,
    ExecutionOutcome, JournalIntent, JournalResult, OfferedTool, ResumeDecision, RunOutcome,
    ScopeAuthorizer, SideEffectClass, StopReason, ToolAuthorizer, ToolExecutor, ToolJournal,
    ValidatedCall,
};
use familiar_ai_agent::token_discipline::{self, EditForm};
#[cfg(unix)]
use familiar_ai_agent::{finish_watchdog, spawn_watchdog};
use familiar_ai_core::config::{AgentRuntimeSandboxConfig, TokenDisciplineConfig};
use familiar_ai_core::{
    GrantMode, ReservationOwnerIdentity, ResourceRequest, ResourceType, UnknownConsumptionPolicy,
};
use familiar_ai_llm::token_discipline::{
    bound_tool_result, file_read_requirement, slice_lines, FileReadRequirement, ToolResultWindow,
};
use familiar_ai_review::parse_expected_files;
use familiar_ai_storage::repos::accounting::{AccountingRepository, UsageObservation};
use familiar_ai_storage::repos::agent_runtime::{AgentRuntimeRepository, ToolResultOutcome};
use familiar_ai_storage::repos::reservation::{
    AcquireOutcome, ReservationRepository, SettlementObservation,
};
use familiar_ai_storage::repos::worker_selection::{
    WorkerSelectionRecord, WorkerSelectionRepository,
};
use familiar_ai_storage::Database;

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

/// Prefix/suffix of the opaque paging-handle string a bounded `run-command`
/// result names and a later `read-file` call echoes back. It looks
/// path-like for the model's benefit but is resolved against
/// execution-scoped daemon-owned storage (`tool_output_retention_dir`),
/// never against the worktree.
const TOOL_OUTPUT_HANDLE_PREFIX: &str = ".familiar/tool-output/";
const TOOL_OUTPUT_HANDLE_SUFFIX: &str = ".txt";

/// Directory levels `search-list` will descend before it stops. Deep enough
/// that no real checkout reaches it (a vendored `node_modules` bottoms out
/// around 15), shallow enough that the recursion cannot exhaust the stack —
/// which aborts the process rather than unwinding, so this is a crash bound
/// and not a politeness limit.
const MAX_WALK_DEPTH: usize = 64;

fn tool_output_handle(call_id: &str) -> String {
    format!("{TOOL_OUTPUT_HANDLE_PREFIX}{call_id}{TOOL_OUTPUT_HANDLE_SUFFIX}")
}

fn tool_output_call_id(path: &str) -> Option<&str> {
    path.strip_prefix(TOOL_OUTPUT_HANDLE_PREFIX)
        .and_then(|rest| rest.strip_suffix(TOOL_OUTPUT_HANDLE_SUFFIX))
}

/// Whether `call_id` is safe to resolve as a single path component beneath
/// `tool_output_retention_dir`. A tool-output handle round-trips a
/// model-supplied string (via `read-file`'s `path` argument) into this
/// component, so it must never be able to contain a separator or a `.`/`..`
/// segment — that is the entire difference between an opaque per-call
/// identifier and a path-traversal payload like
/// `.familiar/tool-output/../../../../etc/passwd.txt`.
fn is_safe_retention_call_id(call_id: &str) -> bool {
    !call_id.is_empty() && call_id != "." && call_id != ".." && !call_id.contains(['/', '\\', '\0'])
}

/// Creates (or validates) `dir` as a retention directory this process
/// exclusively owns, then locks it down to owner-only access. `dir` sits
/// under the shared, world-readable system temp directory
/// (`tool_output_retention_dir`), keyed by a value any local process can
/// derive (worktree root + execution id) — the directory's own permissions
/// are the only barrier between one local user's retained tool output
/// (build logs, environment echoes, anything a command printed) and every
/// other local user, and the only defense against a hostile process
/// pre-creating the exact path to hijack or block retention. `create_dir_all`
/// alone is not enough: if the path already exists as a directory owned by
/// someone else, it happily "succeeds" without ever telling us. So after
/// creation this verifies the directory is actually a directory, is owned
/// by this process's effective user, and is `chmod`-ed to `0700` — refusing
/// (returning `false`, which the caller treats exactly like any other
/// retention failure: fall back to the unbounded byte-capped result) unless
/// every one of those holds.
#[cfg(unix)]
fn secure_retention_dir(dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(dir) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    // SAFETY: `geteuid` takes no arguments, performs no memory access, and
    // cannot fail.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return false;
    }
    if std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_err() {
        return false;
    }
    let Ok(refreshed) = std::fs::metadata(dir) else {
        return false;
    };
    refreshed.permissions().mode() & 0o777 == 0o700
}

#[cfg(not(unix))]
fn secure_retention_dir(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).is_ok()
}

/// Writes retained tool output with owner-only (`0600`) file permissions on
/// unix, so a file's brief existence at default (umask-derived) permissions
/// is never a window where another local user in the same shared temp
/// directory can read it.
#[cfg(unix)]
fn write_retention_file(path: &Path, content: &str) -> bool {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .is_ok()
}

#[cfg(not(unix))]
fn write_retention_file(path: &Path, content: &str) -> bool {
    std::fs::write(path, content).is_ok()
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
    /// PRD-072 targeted-edit and bounded-result configuration. Disabled
    /// (the default) reproduces pre-PRD-072 behavior byte-for-byte.
    pub token_discipline: TokenDisciplineConfig,
}

impl SandboxedToolExecutor {
    /// The single chokepoint every filesystem-touching capability resolves
    /// its path through. Containment is decided on the *resolved* path —
    /// the joined path with every symlink followed, canonicalized against
    /// the deepest ancestor that actually exists for a leaf that does not
    /// yet exist (an `apply-edit` creating a new file) — never on the
    /// spelled path. A lexical check alone cannot see a symlink, and this
    /// runtime offers `run-command`, so a worker can create one and reach
    /// straight through it with a path that is relative and `..`-free.
    ///
    /// This still has a residual TOCTOU window: nothing prevents a
    /// concurrent `run-command` from replacing a path component with a
    /// symlink between this check and the filesystem call that follows it.
    /// Resolving that fully needs O_NOFOLLOW/openat2-style syscalls this
    /// executor does not yet use; what this closes is the deterministic
    /// hole where an already-planted symlink is never even inspected.
    fn resolve_within_worktree(&self, relative: &str) -> Result<PathBuf, ExecutionError> {
        if relative.is_empty()
            || Path::new(relative).is_absolute()
            || relative.split('/').any(|part| part == "..")
        {
            return Err(ExecutionError::Failed(format!(
                "path {relative:?} is not a safe worktree-relative path"
            )));
        }
        let canonical_root = self.worktree_root.canonicalize().map_err(|error| {
            ExecutionError::Failed(format!(
                "worktree root {:?} could not be resolved: {error}",
                self.worktree_root
            ))
        })?;
        let joined = self.worktree_root.join(relative);
        let resolved = canonicalize_partial(&joined).map_err(|error| {
            ExecutionError::Failed(format!("path {relative:?} could not be resolved: {error}"))
        })?;
        if !resolved.starts_with(&canonical_root) {
            return Err(ExecutionError::Failed(format!(
                "path {relative:?} escapes the worktree root"
            )));
        }
        refuse_unprovable_hard_link(&resolved, relative)?;
        Ok(resolved)
    }

    /// Daemon-owned, execution-scoped directory for retained (unbounded)
    /// tool output — deliberately **outside** `worktree_root`. Writing it
    /// inside the worktree would need write-scope authorization no
    /// PRD-013 Expected Files entry could ever cover, and would introduce
    /// untracked files into the very diff/expected-files evidence the
    /// change under review is judged against. `pub` so tests can locate it
    /// directly; the model never sees this path, only the opaque handle
    /// string `tool_output_handle` produces.
    pub fn tool_output_retention_dir(&self, execution_id: &str) -> PathBuf {
        // Keyed by worktree root as well as execution id: an execution id
        // is only unique within its own worktree's journal, so hashing
        // both together (rather than execution id alone) keeps concurrent
        // executions against different worktrees from ever colliding on
        // the same retention directory.
        let key =
            sha256_hex(format!("{}\n{execution_id}", self.worktree_root.display()).as_bytes());
        std::env::temp_dir()
            .join("familiar-ai-tool-output")
            .join(key)
    }

    /// Resolves the daemon-owned path for one call's retained tool output.
    /// `call_id` is validated by `is_safe_retention_call_id` first: it
    /// reaches here either as the loop's own `call.call_id` (the write side,
    /// `run_command`) or as a model-supplied string extracted from a
    /// `read-file` path (`tool_output_call_id`) — in both cases it must be
    /// confined to a single path component before it is ever joined onto a
    /// filesystem path, so this is the one place both call sites route
    /// through rather than each doing its own (and possibly divergent)
    /// check.
    fn tool_output_retention_path(
        &self,
        execution_id: &str,
        call_id: &str,
    ) -> Result<PathBuf, ExecutionError> {
        if !is_safe_retention_call_id(call_id) {
            return Err(ExecutionError::Failed(format!(
                "tool-output call id {call_id:?} is not a valid single path component"
            )));
        }
        Ok(self
            .tool_output_retention_dir(execution_id)
            .join(format!("{call_id}.txt")))
    }

    fn read_file(
        &self,
        call: &ValidatedCall,
        ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        let path = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // A retained-tool-output handle is an opaque identifier, not a real
        // worktree-relative path: the bytes it names live in daemon-owned
        // storage outside the worktree (see `tool_output_retention_dir`), so
        // it is resolved there instead of through `resolve_within_worktree`.
        // This diversion is only ever recognized while token discipline is
        // enabled — it is the feature that hands a handle out in the first
        // place — so with discipline off every `read-file` path resolves
        // through `resolve_within_worktree` exactly as pre-PRD-072, and a
        // genuine worktree file that happens to be named
        // `.familiar/tool-output/<x>.txt` is never diverted.
        let handle_call_id = self
            .token_discipline
            .enabled
            .then(|| tool_output_call_id(path))
            .flatten();
        let content = if let Some(call_id) = handle_call_id {
            let retention_dir = self.tool_output_retention_dir(&ctx.execution_id);
            let retention_path = self.tool_output_retention_path(&ctx.execution_id, call_id)?;
            // Defense in depth beyond the call-id charset validation inside
            // `tool_output_retention_path`: the resolved file must still
            // canonicalize to a location beneath the retention directory
            // before any bytes are read from it.
            let canonical_dir = retention_dir.canonicalize().map_err(|error| {
                ExecutionError::Failed(format!(
                    "read-file {path:?} failed: retained tool output not found: {error}"
                ))
            })?;
            let canonical_path = retention_path.canonicalize().map_err(|error| {
                ExecutionError::Failed(format!(
                    "read-file {path:?} failed: retained tool output not found: {error}"
                ))
            })?;
            if !canonical_path.starts_with(&canonical_dir) {
                return Err(ExecutionError::Failed(format!(
                    "read-file {path:?} failed: resolved outside its retention directory"
                )));
            }
            std::fs::read_to_string(&canonical_path).map_err(|error| {
                ExecutionError::Failed(format!(
                    "read-file {path:?} failed: retained tool output not found: {error}"
                ))
            })?
        } else {
            let resolved = self.resolve_within_worktree(path)?;
            std::fs::read_to_string(&resolved).map_err(|error| {
                ExecutionError::Failed(format!("read-file {path:?} failed: {error}"))
            })?
        };
        if !self.token_discipline.enabled {
            return Ok(hash_outcome(content));
        }
        let start_line = call.arguments.get("start_line").and_then(|v| v.as_u64());
        let end_line = call.arguments.get("end_line").and_then(|v| v.as_u64());
        let total_lines = content.lines().count();
        match (start_line, end_line) {
            (Some(start), Some(end)) => {
                if start == 0 || end < start {
                    return Err(ExecutionError::Failed(format!(
                        "read-file {path:?} failed: invalid range start_line={start} end_line={end}"
                    )));
                }
                Ok(hash_outcome(slice_lines(
                    &content,
                    start as usize,
                    end as usize,
                )))
            }
            (None, None) => {
                match file_read_requirement(total_lines, self.token_discipline.file_read_max_lines, false)
                {
                    FileReadRequirement::FullFileAllowed => Ok(hash_outcome(content)),
                    FileReadRequirement::ExplicitRangeRequired { total_lines, max_lines } => {
                        Err(ExecutionError::Failed(format!(
                            "read-file {path:?} has {total_lines} lines, exceeding the {max_lines}-line span; specify start_line and end_line"
                        )))
                    }
                }
            }
            _ => Err(ExecutionError::Failed(format!(
                "read-file {path:?} failed: start_line and end_line must be given together"
            ))),
        }
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
        // The walk is rooted at, and reported relative to, the *canonical*
        // root. `resolve_within_worktree` already hands back a canonical
        // path, so stripping against a non-canonical `worktree_root` would
        // fail and fall back to emitting absolute host paths for an
        // ordinary in-worktree search whenever the configured root itself
        // traverses a symlink.
        let canonical_root = self.worktree_root.canonicalize().map_err(|error| {
            ExecutionError::Failed(format!(
                "worktree root {:?} could not be resolved: {error}",
                self.worktree_root
            ))
        })?;
        let root = if subpath.is_empty() {
            canonical_root.clone()
        } else {
            self.resolve_within_worktree(subpath)?
        };
        let mut matches = Vec::new();
        collect_matches(&root, &canonical_root, query, &mut matches, 500, 0);
        Ok(hash_outcome(matches.join("\n")))
    }

    fn apply_edit(&self, call: &ValidatedCall) -> Result<ExecutionOutcome, ExecutionError> {
        let path = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let payload = call
            .arguments
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let change_kind = call
            .arguments
            .get("change_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("whole-file");
        let form = EditForm::parse(change_kind).ok_or_else(|| {
            ExecutionError::Failed(format!(
                "apply-edit {path:?} failed: unknown change_kind {change_kind:?}"
            ))
        })?;
        let resolved = self.resolve_within_worktree(path)?;
        // `None` for a file that does not yet exist — whole-file is the
        // only form that can create one; a targeted edit against a missing
        // file is its own named divergence, not a silent no-op create.
        let current = std::fs::read_to_string(&resolved).ok();
        if form != EditForm::WholeFile && current.is_none() {
            return Err(ExecutionError::Failed(format!(
                "apply-edit {path:?} failed: change_kind {change_kind:?} requires an existing file"
            )));
        }
        let resolved_edit = token_discipline::resolve_edit(current.as_deref(), form, payload)
            .map_err(|error| {
                ExecutionError::Failed(format!("apply-edit {path:?} failed: {error:?}"))
            })?;
        // An idempotent replay (the anchor was already gone because this
        // exact edit already landed) needs no write at all: the model MUST
        // see this distinctly from a genuine write, never the same `wrote N
        // bytes` text a diverged no-op could be confused with.
        if resolved_edit.already_applied {
            return Ok(hash_outcome(format!(
                "apply-edit {path}: already applied, no bytes written (change_kind: {change_kind})"
            )));
        }
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ExecutionError::Failed(format!("apply-edit {path:?} failed: {error}"))
            })?;
        }
        std::fs::write(&resolved, &resolved_edit.content).map_err(|error| {
            ExecutionError::Failed(format!("apply-edit {path:?} failed: {error}"))
        })?;
        // Byte-for-byte with pre-PRD-072 behavior when discipline is off:
        // the result text names the edit form only once the operator has
        // opted in to token discipline.
        let result_text = if self.token_discipline.enabled {
            format!(
                "wrote {} bytes to {path} (change_kind: {change_kind})",
                resolved_edit.content.len()
            )
        } else {
            format!("wrote {} bytes to {path}", resolved_edit.content.len())
        };
        Ok(hash_outcome(result_text))
    }

    fn run_command(
        &self,
        call: &ValidatedCall,
        ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
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

        let combined = format!(
            "exit_status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        );

        if self.token_discipline.enabled {
            let total_lines = combined.lines().count();
            if total_lines > self.token_discipline.tool_result_max_lines {
                // Lossless retention is a *precondition* of bounding, not a
                // best-effort side effect: the full output is durably
                // persisted in daemon-owned storage outside the governed
                // worktree (never as untracked bytes inside it, where it
                // would need write-scope authorization no PRD-013 Expected
                // Files entry could ever cover, and would pollute the
                // worktree's own diff/expected-files evidence) before a
                // paging handle naming it is ever handed to the model. If
                // persistence fails for any reason — including a call id
                // that fails `is_safe_retention_call_id`, or a retention
                // directory this process cannot prove it owns exclusively
                // (see `secure_retention_dir`) — the call falls back to the
                // unbounded, byte-capped result instead of emitting a
                // handle for a region that cannot actually be retrieved.
                let retained = self
                    .tool_output_retention_path(&ctx.execution_id, &call.call_id)
                    .ok()
                    .filter(|retention_path| {
                        retention_path.parent().is_some_and(secure_retention_dir)
                    })
                    .is_some_and(|retention_path| write_retention_file(&retention_path, &combined));
                if !retained {
                    let mut fallback = combined;
                    if fallback.len() > self.max_output_bytes {
                        fallback.truncate(self.max_output_bytes);
                    }
                    return Ok(hash_outcome(fallback));
                }
                let handle = tool_output_handle(&call.call_id);
                let window = ToolResultWindow {
                    max_lines: self.token_discipline.tool_result_max_lines,
                    head_lines: self.token_discipline.tool_result_head_lines,
                    tail_lines: self.token_discipline.tool_result_tail_lines,
                };
                let bounded = bound_tool_result(&combined, &window, Some(handle));
                return Ok(hash_outcome(bounded.visible));
            }
        }

        let mut combined = combined;
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
        ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        match call.capability {
            CapabilityId::ReadFile => self.read_file(call, ctx),
            CapabilityId::SearchList => self.search_list(call),
            CapabilityId::ApplyEdit => self.apply_edit(call),
            CapabilityId::RunCommand => self.run_command(call, ctx),
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

/// Refuses a regular file whose containment cannot be *proven*, which for a
/// hard link means any file carrying more than one name.
///
/// A hard link is not a symlink, and that is exactly why resolved-path
/// containment does not see it: the link *is* the file, so `canonicalize`
/// returns the in-worktree path and `symlink_metadata` reports an ordinary
/// regular file. Every check above passes, and the bytes still belong to a
/// file the worker was never granted — `ln` without `-s` is reachable
/// wherever `ln` is allowlisted, and `fs.protected_hardlinks` does not help
/// because it only refuses to link a file the caller does not own, while the
/// daemon's uid owns its own `~/.ssh`.
///
/// The rule is soundness, not suspicion. `nlink == 1` means the file has
/// exactly one name, and that name is the one just proven inside the root:
/// contained, demonstrably. `nlink > 1` means other names exist, and no
/// portable syscall enumerates them — the kernel offers no inode-to-paths
/// lookup — so containment is *undecidable* rather than merely unchecked.
/// Undecidable fails closed.
///
/// This does refuse a hard link whose every name happens to be inside the
/// worktree. That costs nothing real: git cannot represent a hard link at
/// all — it stores two independent blobs — so no legitimate checkout content
/// depends on one, and a shared inode is precisely the uncontained alias this
/// function exists to deny.
///
/// Only regular files are considered. A directory cannot be hard-linked, and
/// a symlink's own link count says nothing about where it points (that is
/// already settled by canonicalization above).
fn refuse_unprovable_hard_link(resolved: &Path, relative: &str) -> Result<(), ExecutionError> {
    use std::os::unix::fs::MetadataExt;

    // A leaf that does not exist yet (an `apply-edit` creating a new file)
    // has no link count to inspect and nothing to alias.
    let Ok(metadata) = resolved.symlink_metadata() else {
        return Ok(());
    };
    if metadata.file_type().is_file() && metadata.nlink() > 1 {
        return Err(ExecutionError::Failed(format!(
            "path {relative:?} is a hard link with {} names; containment cannot be proven",
            metadata.nlink()
        )));
    }
    Ok(())
}

/// Canonicalizes `path`, following every symlink, even when its leaf (and
/// possibly several of its trailing components) does not exist yet: it
/// walks up to the deepest ancestor that does exist, canonicalizes that
/// ancestor, and re-appends the non-existent tail literally. A path that
/// exists in full is simply canonicalized outright.
///
/// The walk-up only ever fires for `NotFound` — a genuinely absent
/// component. Any other error (permission denied, too many levels of
/// symlinks, not a directory) is returned to the caller rather than
/// silently degrading to a lexical re-append, because that fallback is
/// exactly the lexical-only check this function exists to replace.
///
/// `NotFound` alone is not enough to treat a component as "not created
/// yet", though: a symlink whose target does not exist also fails
/// `canonicalize` with `NotFound`, indistinguishable by error kind alone
/// from a missing file. Before walking past such a component, its
/// `symlink_metadata` is checked — a component with symlink metadata but
/// no resolvable target is a dangling symlink, refused outright, since
/// re-appending it literally would authorize a write straight through it
/// to wherever it points.
fn canonicalize_partial(path: &Path) -> std::io::Result<PathBuf> {
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    loop {
        match current.canonicalize() {
            Ok(mut canonical) => {
                for part in trailing.into_iter().rev() {
                    canonical.push(part);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if current.symlink_metadata().is_ok() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{} is a dangling symlink", current.display()),
                    ));
                }
                let (Some(file_name), Some(parent)) = (current.file_name(), current.parent())
                else {
                    return Err(error);
                };
                trailing.push(file_name.to_os_string());
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Walks `dir`, collecting worktree-relative paths whose spelling contains
/// `query`. Containment is enforced on every entry, not only on the root the
/// walk was handed (PRD-081): `read_dir` yields symlinks and `is_dir` follows
/// them, so an unchecked walk descends straight out of the worktree through a
/// `run-command`-created link and enumerates filenames the worker was never
/// authorized to see — the read-side escape `resolve_within_worktree` closes
/// for `read-file`, reopened one directory level down.
///
/// Two rules keep it closed. An entry that resolves outside `canonical_root`
/// is skipped rather than failing the whole search: an escaping link is the
/// repository's business, and refusing to list an otherwise legitimate tree
/// because one entry points outward would deny the capability rather than
/// contain it. And no symlink is ever *traversed*, even one resolving inside
/// — matching how git treats a link as a leaf rather than a door, and
/// removing the unbounded recursion a `ln -s . loop` would otherwise get when
/// `query` matches nothing and `limit` is therefore never reached.
///
/// Those two rules bound *cycles*; they do not bound *depth*, and an ordinary
/// acyclic tree is enough to exhaust the stack. `limit` only short-circuits
/// once matches accumulate, so a query matching nothing recurses once per
/// directory level all the way down — and a Rust stack overflow aborts the
/// process rather than unwinding, taking the daemon with it. `mkdir -p` plus
/// one `search-list` is the whole exploit, so `depth` caps the descent
/// explicitly: below `MAX_WALK_DEPTH` the walk behaves as before, and at the
/// cap it stops descending rather than dying.
fn collect_matches(
    dir: &Path,
    canonical_root: &Path,
    query: &str,
    out: &mut Vec<String>,
    limit: usize,
    depth: usize,
) {
    if depth >= MAX_WALK_DEPTH {
        return;
    }
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
        // Only a symlink can leave a contained directory; a plain entry is
        // contained by construction. Paying `canonicalize` for links alone
        // leaves the ordinary walk as cheap as it was. An unreadable entry
        // is treated as a link and therefore never traversed — fail closed.
        let is_symlink = path
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(true);
        if is_symlink {
            let Ok(resolved) = path.canonicalize() else {
                continue;
            };
            if !resolved.starts_with(canonical_root) {
                continue;
            }
        }
        let relative = path
            .strip_prefix(canonical_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if !is_symlink && path.is_dir() {
            if relative.split('/').next_back() == Some(".git") {
                continue;
            }
            collect_matches(&path, canonical_root, query, out, limit, depth + 1);
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
    token_discipline: &TokenDisciplineConfig,
    outcome: &RunOutcome,
) -> familiar_ai_core::Result<()> {
    let agent_runtime_repo = AgentRuntimeRepository::new(conn);
    let accounting_repo = AccountingRepository::new(conn);
    let terminal_status = stop_reason_key(outcome.stop_reason);

    // PRD-072: identity/version pair for the discipline configuration that
    // was active for this run, mirroring migration 043's compression
    // attribution (`output_register_id`/`input_compression_id`) so the
    // PRD-051 ledger can partition output/input volume by discipline state.
    // Disabled reproduces the pre-PRD-072 "raw-runtime-none" value exactly.
    let (edit_form_id, edit_form_version) = if token_discipline.enabled {
        ("targeted-edit-preferred", "1")
    } else {
        ("raw-runtime-none", "raw-runtime-none")
    };
    let (truncation_config_id, truncation_config_version) = if token_discipline.enabled {
        ("bounded-window", "1")
    } else {
        ("raw-runtime-none", "raw-runtime-none")
    };

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
            edit_form_id,
            edit_form_version,
            truncation_config_id,
            truncation_config_version,
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

// ---------------------------------------------------------------------
// PRD-100: SqliteRawAgentHost — the concrete
// `familiar_ai_agent::raw_agent::RawAgentHost` every owned-loop worker uses.
// Bundles exactly the same SQLite-backed journal, sandboxed executor,
// PRD-013 write-scope authorizer, and PRD-051 persistence this module
// already supplies to the CLI-driven path, plus a PRD-064 budget
// reservation gate scoped to one execution.
// ---------------------------------------------------------------------

/// Owns a database path and execution id rather than a borrowed
/// `Connection`, opening a fresh connection per call. `RawAgentHost`'s
/// methods return owned, `'static` trait objects (`familiar-ai-agent` never
/// depends on `rusqlite`), so a journal bound to a borrowed connection
/// cannot be handed back across that boundary — this is the owning
/// equivalent of [`SqliteToolJournal`].
struct OwningSqliteToolJournal {
    execution_id: String,
    /// Opened once, when the host hands out this journal, and held for its
    /// whole lifetime rather than reopened per call. A transient failure to
    /// open no longer surfaces as "no result recorded" from a call that ran
    /// moments earlier through the same, then-healthy, connection; it
    /// surfaces once, here, and every subsequent read/write on this journal
    /// instance fails closed from the same recorded error instead of
    /// re-attempting an open that a resumed loop cannot distinguish from
    /// "never happened".
    db: Result<Database, String>,
}

impl ToolJournal for OwningSqliteToolJournal {
    fn record_intent(&mut self, intent: &JournalIntent) -> Result<(), String> {
        let db = self.db.as_ref().map_err(|error| error.clone())?;
        SqliteToolJournal::new(db.conn(), self.execution_id.clone()).record_intent(intent)
    }

    fn record_result(&mut self, call_id: &str, result: &JournalResult) -> Result<(), String> {
        let db = self.db.as_ref().map_err(|error| error.clone())?;
        SqliteToolJournal::new(db.conn(), self.execution_id.clone()).record_result(call_id, result)
    }

    /// The write-ahead journal's whole purpose is to make a destructive
    /// call's already-executed status decisive. An unreadable journal
    /// cannot prove "not done" — treating it as `None` (as a fresh,
    /// never-called journal would report) converts an already-executed
    /// destructive call into a replay candidate the instant the database
    /// becomes briefly unreadable. Reporting a synthetic failed result
    /// instead blocks re-execution unconditionally: the loop treats the
    /// call as already resolved (unfavorably) rather than guessing it is
    /// safe to run again.
    fn result_for(&self, call_id: &str) -> Option<JournalResult> {
        match self.db.as_ref() {
            Ok(db) => SqliteToolJournal::new(db.conn(), self.execution_id.clone()).result_for(call_id),
            Err(error) => Some(JournalResult::Failed {
                detail: format!(
                    "journal database unreadable, refusing to treat call {call_id:?} as not-yet-executed: {error}"
                ),
            }),
        }
    }

    /// Purely an evidence field (`resume_point.journal_high_water_mark`);
    /// actual resume decisions read `pending_intents` straight from
    /// storage, never this count. Still, silently reporting 0 when the
    /// database cannot be read claims "empty journal" for a journal whose
    /// true size is unknown, so an unreadable database reports the
    /// conservative sentinel `usize::MAX` instead of a fabricated count.
    fn len(&self) -> usize {
        match self.db.as_ref() {
            Ok(db) => SqliteToolJournal::new(db.conn(), self.execution_id.clone()).len(),
            Err(_) => usize::MAX,
        }
    }
}

/// The SQLite-backed `RawAgentHost` every PRD-100 owned-loop worker uses.
/// Built fresh per execution (`familiar_ai_daemon::run`), since the
/// PRD-013 write-scope it authorizes against and the execution id its
/// journal/evidence key on are themselves per-attempt facts — never cached
/// or reused across PRD attempts the way a `Box<dyn CodingAgent>` for a CLI
/// worker can be.
pub struct SqliteRawAgentHost {
    pub database_path: PathBuf,
    pub execution_id: String,
    pub project_id: String,
    pub worker_id: String,
    pub stage: String,
    pub runtime_id: String,
    pub model_identity: Option<String>,
    pub worktree_root: PathBuf,
    pub sandbox: AgentRuntimeSandboxConfig,
    pub token_discipline: TokenDisciplineConfig,
    pub allowed_write_paths: Vec<String>,
    pub granted_capabilities: Vec<CapabilityId>,
    pub command_timeout_ms: u64,
    pub max_output_bytes: usize,
    reservation_id: Mutex<Option<String>>,
    /// PRD-100 remediation (N1): `build_selected_agents` constructs one
    /// `SqliteRawAgentHost` per stage and hands it back as a `Box<dyn
    /// CodingAgent>` that is reused for every `execute()` call against that
    /// stage — a remediation round, a review re-run, or a retried
    /// implementation attempt. Scoping `reservation_pool_id`/
    /// `owner_instance_id` by stage alone means the first `execute()` call
    /// defines a pool sized to exactly its own request and consumes it via
    /// `finish`'s settle; a second `execute()` on the same host then
    /// re-acquires against an exhausted pool under a reservation-owner id
    /// migration 042 requires to be globally unique, and is refused before
    /// the adapter is ever reached. This counter is incremented once per
    /// `reserve_execution_budget` call so every attempt within the stage
    /// draws its own freshly-defined pool under its own reservation owner.
    attempt_sequence: Mutex<u64>,
}

impl SqliteRawAgentHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database_path: PathBuf,
        execution_id: String,
        project_id: String,
        worker_id: String,
        stage: String,
        runtime_id: String,
        model_identity: Option<String>,
        worktree_root: PathBuf,
        sandbox: AgentRuntimeSandboxConfig,
        token_discipline: TokenDisciplineConfig,
        allowed_write_paths: Vec<String>,
        granted_capabilities: Vec<CapabilityId>,
        command_timeout_ms: u64,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            database_path,
            execution_id,
            project_id,
            worker_id,
            stage,
            runtime_id,
            model_identity,
            worktree_root,
            sandbox,
            token_discipline,
            allowed_write_paths,
            granted_capabilities,
            command_timeout_ms,
            max_output_bytes,
            reservation_id: Mutex::new(None),
            attempt_sequence: Mutex::new(0),
        }
    }

    /// Scoped by stage as well as execution: `build_selected_agents`
    /// constructs a separate `SqliteRawAgentHost` per stage
    /// (implementation/review/remediation) that all share one
    /// `execution_id`. An execution-id-only pool id means the first stage
    /// to run defines the pool sized to its own request, consumes it, and
    /// every later stage's `acquire` against the same exhausted pool is
    /// refused before it can submit a single inference attempt — silently
    /// defeating cross-provider independent review. Each stage now defines
    /// and draws from its own pool.
    ///
    /// Also scoped by attempt (see `attempt_sequence`'s doc comment): this
    /// returns the pool id the *next* `reserve_execution_budget` call will
    /// define/draw from, without mutating the counter — callers (including
    /// tests) that need to pre-define a pool before reserving read the same
    /// id `reserve_execution_budget` is about to use.
    #[cfg(test)]
    fn reservation_pool_id(&self) -> String {
        let attempt = *self
            .attempt_sequence
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.reservation_pool_id_for_attempt(attempt)
    }

    fn reservation_pool_id_for_attempt(&self, attempt: u64) -> String {
        format!(
            "raw-agent-budget:{}:{}:{attempt}",
            self.execution_id, self.stage
        )
    }
}

impl RawAgentHost for SqliteRawAgentHost {
    fn journal(&self) -> Box<dyn ToolJournal> {
        Box::new(OwningSqliteToolJournal {
            execution_id: self.execution_id.clone(),
            db: Database::open(&self.database_path).map_err(|error| error.to_string()),
        })
    }

    /// Confined to the calling request's own `working_directory`, never
    /// `self.worktree_root`: `build_selected_agents` constructs and reuses
    /// one `SqliteRawAgentHost` per stage across every `execute()` call on
    /// that stage, but the isolated-review contract
    /// (`StructuredReviewAdapter::review`, `familiar-ai-review`) runs a
    /// review call against a freshly created temporary workspace, never the
    /// repository worktree `self.worktree_root` was constructed with. Using
    /// the construction-time root here would authorize every call against
    /// the request's `working_directory` (`RequestScopedAuthorizer`) while
    /// executing against a different tree entirely.
    fn executor(&self, working_directory: &Path) -> Box<dyn ToolExecutor> {
        Box::new(SandboxedToolExecutor {
            worktree_root: working_directory.to_path_buf(),
            sandbox: self.sandbox.clone(),
            command_timeout_ms: self.command_timeout_ms,
            max_output_bytes: self.max_output_bytes,
            token_discipline: self.token_discipline.clone(),
        })
    }

    fn authorizer(&self) -> Box<dyn ToolAuthorizer> {
        Box::new(ScopeAuthorizer {
            granted_capabilities: self.granted_capabilities.clone(),
            allowed_write_paths: self.allowed_write_paths.clone(),
            allowed_commands: self.sandbox.allowed_commands.clone(),
            network_allowed: self.sandbox.network_allowed,
        })
    }

    /// `attempt_id` carries the same per-host attempt sequence number
    /// `reserve_execution_budget` just consumed for this call (see its
    /// `attempt_sequence` doc comment) — `authority()` is always called
    /// after `reserve_execution_budget` within one `RawAgent::execute` call,
    /// so this is stable for that call and distinct from every other
    /// `execute()` call on this same host. `RawAgent::execute` folds it into
    /// every `AttemptId` it mints for this call, so two `execute()` calls
    /// sharing one `execution_id` (a remediation round, a review re-run)
    /// never collide on `agent_runtime_attempts`' primary key the way a
    /// counter that restarted at zero every call once did.
    fn authority(&self) -> AuthorityContext {
        let attempt = *self
            .attempt_sequence
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        AuthorityContext {
            project_id: self.project_id.clone(),
            execution_id: self.execution_id.clone(),
            attempt_id: attempt.to_string(),
            worker_id: self.worker_id.clone(),
        }
    }

    /// `run_loop`'s `mint_attempt_id` hook is infallible, so the loop core
    /// itself offers no seam to refuse an individual submission. This
    /// acquires one PRD-064 reservation, scoped to this execution alone
    /// (`reservation_pool_id`), before the loop is ever invoked — every
    /// attempt the loop may go on to make draws against it, and a refusal
    /// here means zero attempts run. A configured cost ceiling sizes the
    /// reservation exactly; an absent ceiling still requires the gate to
    /// succeed structurally (a minimal one-nanoUSD draw against effectively
    /// unlimited capacity), so "no reservation" is never silently treated
    /// as "unlimited budget, proceed anyway".
    fn reserve_execution_budget(&self, max_cost_microusd: Option<u64>) -> Result<(), String> {
        let (capacity, amount) = match max_cost_microusd {
            Some(value) => {
                let nanousd = value
                    .checked_mul(1_000)
                    .ok_or("cost ceiling exceeds nanoUSD range")?
                    .max(1);
                (nanousd, nanousd)
            }
            None => (i64::MAX as u64, 1),
        };
        // Consumed once per `reserve_execution_budget` call, before the
        // pool/owner ids are built: every `execute()` call this host serves
        // (a fresh attempt within the stage — a remediation round, a review
        // re-run, a retried implementation attempt) gets a pool and owner
        // id no earlier attempt has ever defined or drawn from, so a prior
        // attempt's settled reservation can never refuse this one.
        let attempt = {
            let mut counter = self
                .attempt_sequence
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let current = *counter;
            *counter += 1;
            current
        };
        let pool_id = self.reservation_pool_id_for_attempt(attempt);
        let mut db = Database::open(&self.database_path).map_err(|error| error.to_string())?;
        let mut repo = ReservationRepository::new(db.conn_mut());
        // This pool id is unique per execution/stage/attempt, so in ordinary
        // operation it is never already defined. Guarding on
        // `pool_is_defined` rather than defining unconditionally means a
        // smaller capacity someone else deliberately constrained this pool
        // to (an operator override, or a test simulating exhaustion) is
        // never silently widened back out by this call.
        if !repo
            .pool_is_defined(&pool_id, &ResourceType::NanousdBudget)
            .map_err(|error| error.to_string())?
        {
            repo.define_pool(&pool_id, &ResourceType::NanousdBudget, capacity, false)
                .map_err(|error| error.to_string())?;
        }
        // `owner_instance_id` is globally unique in `resource_reservations`
        // (migration 042), so — like `reservation_pool_id` — it must be
        // scoped by attempt as well as stage and execution: two attempts
        // within one stage are two distinct reservation owners, not one
        // owner acquiring twice.
        let owner_instance_id = format!("raw-agent:{}:{}:{attempt}", self.execution_id, self.stage);
        let owner = ReservationOwnerIdentity {
            owner_instance_id: owner_instance_id.clone(),
            installation_id: None,
            nonce_or_generation: owner_instance_id,
            owner_kind: "raw-agent".into(),
            project_id: self.project_id.clone(),
            execution_id: self.execution_id.clone(),
            component_id: self.stage.clone(),
        };
        let requests = vec![ResourceRequest {
            pool_id: pool_id.clone(),
            resource_type: ResourceType::NanousdBudget,
            amount,
        }];
        match repo
            .acquire(&owner, &requests, GrantMode::AllOrNothing, None)
            .map_err(|error| error.to_string())?
        {
            AcquireOutcome::Granted(grant) => {
                *self
                    .reservation_id
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(grant.reservation_id);
                Ok(())
            }
            AcquireOutcome::Refused { .. } => Err(format!(
                "no budget reservation available for execution {:?}",
                self.execution_id
            )),
        }
    }

    fn finish(&self, outcome: &RunOutcome) -> Result<(), String> {
        let mut db = Database::open(&self.database_path).map_err(|error| error.to_string())?;
        let reservation_id = self
            .reservation_id
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(reservation_id) = reservation_id {
            let mut repo = ReservationRepository::new(db.conn_mut());
            let actor = format!("raw-agent:{}", self.execution_id);
            match outcome.stop_reason {
                StopReason::Cancelled => {
                    repo.release(&reservation_id, &actor)
                        .map_err(|error| error.to_string())?;
                }
                StopReason::Timeout | StopReason::ProviderFailure { .. } => {
                    repo.settle(
                        &reservation_id,
                        SettlementObservation::Unknown {
                            policy: UnknownConsumptionPolicy::HoldReservation,
                        },
                        &actor,
                    )
                    .map_err(|error| error.to_string())?;
                }
                _ => {
                    repo.settle(
                        &reservation_id,
                        SettlementObservation::Unknown {
                            policy: UnknownConsumptionPolicy::SettleReservedAmount,
                        },
                        &actor,
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
        }
        persist_run_outcome(
            db.conn(),
            &self.execution_id,
            &self.stage,
            &self.worker_id,
            &self.runtime_id,
            self.model_identity.as_deref(),
            None,
            &self.token_discipline,
            outcome,
        )
        .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod raw_agent_host_tests {
    use super::*;
    use familiar_ai_agent::raw_runtime::{LoopEvidence, ResumePoint};

    fn setup(execution_id: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let database_path = dir.path().join("db.sqlite");
        let db = Database::open(&database_path).unwrap();
        db.run_migrations().unwrap();
        db.conn()
            .execute(
                "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'raw-runtime','running','repo','wt','docs/prds/PRD-100.md','[]')",
                rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        (dir, database_path)
    }

    fn host(
        database_path: PathBuf,
        execution_id: &str,
        worktree_root: PathBuf,
    ) -> SqliteRawAgentHost {
        host_with_stage(database_path, execution_id, "implementation", worktree_root)
    }

    fn host_with_stage(
        database_path: PathBuf,
        execution_id: &str,
        stage: &str,
        worktree_root: PathBuf,
    ) -> SqliteRawAgentHost {
        SqliteRawAgentHost::new(
            database_path,
            execution_id.into(),
            "proj_1".into(),
            format!("worker_{stage}"),
            stage.into(),
            "anthropic-api".into(),
            Some("claude-sonnet-5".into()),
            worktree_root,
            AgentRuntimeSandboxConfig::default(),
            TokenDisciplineConfig::default(),
            vec!["src/lib.rs".into()],
            vec![CapabilityId::ApplyEdit],
            2_000,
            4096,
        )
    }

    fn stub_outcome(stop_reason: StopReason) -> RunOutcome {
        RunOutcome {
            stop_reason,
            attempts: vec![],
            evidence: LoopEvidence {
                prompt_template_version: "v1".into(),
                worker_spec_identity: "wspec-sha256:test".into(),
                worker_empirical_version: "wver-sha256:test".into(),
                offered_tools: vec![] as Vec<OfferedTool>,
                calls: vec![],
                stop_reason,
                resume_point: ResumePoint {
                    conversation_messages: 0,
                    journal_high_water_mark: 0,
                },
                iterations: 1,
            },
            final_text: None,
        }
    }

    #[test]
    fn reservation_succeeds_with_a_configured_ceiling_and_is_committed_on_completion() {
        let (_dir, database_path) = setup("exec_1");
        let worktree = tempfile::tempdir().unwrap();
        let host = host(
            database_path.clone(),
            "exec_1",
            worktree.path().to_path_buf(),
        );
        host.reserve_execution_budget(Some(1_000)).unwrap();
        host.finish(&stub_outcome(StopReason::Completed {
            structured_output: false,
        }))
        .unwrap();

        let db = Database::open(&database_path).unwrap();
        let state: String = db
            .conn()
            .query_row(
                "SELECT state FROM resource_reservations WHERE execution_id='exec_1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "committed");
    }

    /// PRD-100 acceptance criterion: an attempt without a reservation cannot
    /// run. A pool too small to grant the requested amount refuses the
    /// reservation outright.
    #[test]
    fn insufficient_pool_capacity_refuses_the_reservation() {
        let (_dir, database_path) = setup("exec_2");
        let worktree = tempfile::tempdir().unwrap();
        let host = host(
            database_path.clone(),
            "exec_2",
            worktree.path().to_path_buf(),
        );
        // Pre-define a pool far smaller than the requested ceiling so the
        // acquire step is refused rather than granted.
        {
            let mut db = Database::open(&database_path).unwrap();
            let mut repo = ReservationRepository::new(db.conn_mut());
            repo.define_pool(
                &host.reservation_pool_id(),
                &ResourceType::NanousdBudget,
                10,
                false,
            )
            .unwrap();
        }
        let error = host.reserve_execution_budget(Some(1_000_000)).unwrap_err();
        assert!(error.contains("no budget reservation available"), "{error}");
    }

    /// N1 regression: `build_selected_agents` constructs one
    /// `SqliteRawAgentHost` per stage and `RawAgent::execute` composes
    /// `reserve_execution_budget` + `finish` on that same host for every
    /// attempt within the stage — a remediation round, a review re-run, or a
    /// retried implementation attempt, not just a second stage. Before
    /// scoping the pool id and `owner_instance_id` by attempt as well as
    /// stage, the first attempt defined a pool sized to exactly its own
    /// request and consumed it via `finish`'s settle; the second attempt's
    /// `acquire` against that same exhausted pool, under a reservation-owner
    /// id migration 042 requires to be globally unique, was always refused
    /// before the adapter could ever be reached.
    #[test]
    fn a_second_attempt_on_the_same_host_after_finish_still_gets_a_reservation() {
        let (_dir, database_path) = setup("exec_remediation_round");
        let worktree = tempfile::tempdir().unwrap();
        let host = host(
            database_path,
            "exec_remediation_round",
            worktree.path().to_path_buf(),
        );

        host.reserve_execution_budget(Some(1_000))
            .expect("first attempt must be granted a reservation");
        host.finish(&stub_outcome(StopReason::Completed {
            structured_output: false,
        }))
        .expect("first attempt's reservation must settle cleanly");

        host.reserve_execution_budget(Some(1_000)).expect(
            "a second attempt on the same host, after the first attempt's reservation settled, \
             must still be granted its own reservation rather than refused against an \
             already-exhausted pool",
        );
        host.finish(&stub_outcome(StopReason::Completed {
            structured_output: false,
        }))
        .expect("second attempt's reservation must settle cleanly");
    }

    /// F1 regression: `build_selected_agents` constructs one
    /// `SqliteRawAgentHost` per stage but all of them share one
    /// `execution_id`. Before scoping the reservation pool id by stage, the
    /// first stage to reserve defined a pool sized to its own request,
    /// consumed it, and the second stage's `acquire` against that same
    /// exhausted pool was always refused — defeating cross-provider
    /// independent review before a single inference attempt could run.
    #[test]
    fn implementation_and_review_stages_sharing_one_execution_id_each_get_a_reservation() {
        let (_dir, database_path) = setup("exec_shared");
        let worktree = tempfile::tempdir().unwrap();
        let implementation_host = host_with_stage(
            database_path.clone(),
            "exec_shared",
            "implementation",
            worktree.path().to_path_buf(),
        );
        let review_host = host_with_stage(
            database_path.clone(),
            "exec_shared",
            "review",
            worktree.path().to_path_buf(),
        );

        implementation_host
            .reserve_execution_budget(Some(1_000))
            .expect("implementation stage must be granted its own reservation");
        review_host
            .reserve_execution_budget(Some(1_000))
            .expect("review stage must be granted its own reservation, not refused by the implementation stage's pool");

        assert_ne!(
            implementation_host.reservation_pool_id(),
            review_host.reservation_pool_id(),
            "each stage must draw from its own budget pool"
        );
    }

    #[test]
    fn write_outside_allowed_paths_is_refused_by_the_authorizer() {
        let (_dir, database_path) = setup("exec_3");
        let worktree = tempfile::tempdir().unwrap();
        let host = host(database_path, "exec_3", worktree.path().to_path_buf());
        let authorizer = host.authorizer();
        let authority = host.authority();
        let call = ValidatedCall {
            call_id: "c1".into(),
            capability: CapabilityId::ApplyEdit,
            arguments: serde_json::json!({"path": "secrets/keys.pem", "content": "x"}),
            argument_hash: "h".into(),
        };
        assert!(matches!(
            authorizer.authorize(&call, &authority),
            familiar_ai_agent::raw_runtime::AuthorizationDecision::Refused { .. }
        ));
    }

    /// F2 regression: an unreadable journal database must fail closed, not
    /// report `None` — which `process_tool_call` treats identically to
    /// "this call has never run", making a resumed loop replay an
    /// already-executed (possibly destructive) call the instant its
    /// database briefly cannot be opened.
    #[test]
    fn journal_open_failure_fails_closed_instead_of_reporting_no_result() {
        let dir = tempfile::tempdir().unwrap();
        // A directory can never be opened as a SQLite database file, giving
        // a deterministic, permission-independent open failure.
        let unreadable_path = dir.path().join("not-a-database");
        std::fs::create_dir(&unreadable_path).unwrap();

        let journal = OwningSqliteToolJournal {
            execution_id: "exec_unreadable".into(),
            db: Database::open(&unreadable_path).map_err(|error| error.to_string()),
        };
        assert!(
            journal.db.is_err(),
            "the database open must actually fail for this test to be meaningful"
        );

        let prior = journal.result_for("destructive-call-1");
        assert!(
            matches!(prior, Some(JournalResult::Failed { .. })),
            "an unreadable journal must fail closed instead of reporting 'no result recorded': {prior:?}"
        );

        assert_eq!(
            journal.len(),
            usize::MAX,
            "an unreadable journal must not silently claim to be empty"
        );
    }
}
