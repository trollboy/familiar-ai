use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::Utc;
use familiar_ai_agent::{
    AgentExecutionError, ClaudeCodeAgent, ClaudeCodeSettings, CodexAgent, CodingAgent,
    ExecutionRequest, ExecutionResult,
};
use familiar_ai_context::{
    ContextBudget, ContextBudgetError, ContextBudgeter, ContextCompilationError, ContextCompiler,
    ContextProfile, ContextReferenceKind, ContextReferenceRoot, ContextRequest, ExecutionContext,
};
use familiar_ai_core::{
    admit_run_prd, resolve_run_prd, validate_graph, AgentAdapterKind, AgentEntryConfig, AppPaths,
    BacklogDiscovery, BacklogStatusStore, Config, ExecutionPrice, FilesystemBacklogDiscovery,
    ScopeClassPolicyConfig, ScopeDeclarationModeConfig, ScopeFileClassName,
};
use familiar_ai_review::{
    compile_scope_policy, content_hash, normalize_scope_path, parse_expected_files,
    AgentAssignment, AgentObservation, AgentRole, BlockingPolicy, BoundedDocument,
    CodingRemediationAdapter, CommandVerificationRunner, CoordinationRequest, ExpectedFilesError,
    GitChangeKind, GitEvidenceCollector, ProhibitedRule, ProhibitedRuleKind, ReviewCoordinator,
    ReviewCycle, ReviewCycleState, ReviewDisposition, ReviewPackageBudget, ReviewStopReason,
    ReviewTask, ScopeClassPolicy, ScopeClassificationRule, ScopeDeclarationMode, ScopeFileClass,
    ScopeFileClassPolicies, ScopePathEntry, ScopePolicyInput, ScopePolicySnapshot, ScopeRuleSource,
    StructuredReviewAdapter, VerificationCheck, VerificationPlan, WorkflowLimits,
};
use familiar_ai_storage::{
    Database, ExecutionFinalization, ExecutionHistoryRepository, ExecutionStart, ReviewRepository,
    SqliteBacklogRepository,
};

const EXECUTION_CONSTRAINTS: &str = r#"- Implement the supplied PRD exactly as written and do not broaden its scope.
- Treat repository source and Git state as authoritative.
- Inspect the existing implementation and identify blocking conflicts before editing.
- Do not modify architecture documents, ADRs, contracts, or existing PRDs.
- Do not implement later PRDs or perform unrelated cleanup.
- Preserve existing user changes in the worktree.
- When implementation is complete, audit every acceptance criterion, run focused tests, formatting, static analysis, and attempt the workspace test suite.
- Distinguish implementation-caused failures from pre-existing failures and summarize changed files and deviations.
- Stop after completing and reporting the supplied PRD."#;

static EXECUTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum RunError {
    CurrentDirectory(io::Error),
    Context(ContextCompilationError),
    ContextBudget(ContextBudgetError),
    Config(String),
    Storage(String),
    Agent(AgentExecutionError),
    HistoryFinalize {
        execution_id: String,
        detail: String,
    },
    Workflow {
        result: Option<Box<ExecutionResult>>,
        detail: String,
    },
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(e) => write!(f, "cannot resolve current directory: {e}"),
            Self::Context(error) => error.fmt(f),
            Self::ContextBudget(error) => error.fmt(f),
            Self::Config(m) => write!(f, "configuration failed: {m}"),
            Self::Storage(m) => write!(f, "execution history failed: {m}"),
            Self::Agent(error) => error.fmt(f),
            Self::HistoryFinalize {
                execution_id,
                detail,
            } => write!(f, "history_finalize_failed for {execution_id}: {detail}"),
            Self::Workflow { detail, .. } => f.write_str(detail),
        }
    }
}
impl std::error::Error for RunError {}

impl RunError {
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Workflow {
                result: Some(result),
                ..
            } => result.exit_code.filter(|code| *code != 0),
            Self::Agent(error) => error.result().exit_code.filter(|code| *code != 0),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct RunWorkflowResult {
    pub implementation: ExecutionResult,
}

/// The independently configured implementation and reviewer agents.
/// Orchestration never inspects which concrete adapter it holds.
pub struct AgentSet<'a> {
    pub implementation: &'a dyn CodingAgent,
    pub reviewer: &'a dyn CodingAgent,
}

/// Resolve the configured agent entries: validated when `[agents]` is
/// present (including review-identity consistency), exact historical Codex
/// defaults when absent.
pub fn resolved_agent_entries(
    config: &Config,
) -> Result<(AgentEntryConfig, AgentEntryConfig), String> {
    match &config.agents {
        Some(agents) => {
            agents.validate(&config.review)?;
            Ok((agents.implementation.clone(), agents.reviewer.clone()))
        }
        None => Ok((AgentEntryConfig::default(), AgentEntryConfig::default())),
    }
}

/// Deterministic constructor: adapter enum to concrete agent, nothing else.
/// Performs no probing, filesystem checks, or model calls.
pub fn build_agent(entry: &AgentEntryConfig) -> Box<dyn CodingAgent> {
    match entry.adapter {
        AgentAdapterKind::Codex => Box::new(CodexAgent::new(entry.resolved_executable())),
        AgentAdapterKind::ClaudeCode => Box::new(ClaudeCodeAgent::new(ClaudeCodeSettings {
            executable: entry.resolved_executable(),
            model: entry.model.clone(),
            effort: entry.effort.map(|effort| effort.as_str().to_owned()),
            permission_mode: entry.permission_mode.map(|mode| mode.as_str().to_owned()),
            max_budget_microusd: (entry.max_budget_microusd > 0)
                .then_some(entry.max_budget_microusd),
            extra_args: entry.extra_args.clone(),
        })),
    }
}

pub fn execute_with_config(
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
) -> Result<RunWorkflowResult, RunError> {
    execute_with_config_tracked(prd_path, agents, config, paths).0
}

/// As [`execute_with_config`], additionally reporting the execution id and the
/// exact retained reason when the backlog entry stays `in_progress`. The
/// unattended driver needs both as values; the single-run path is unchanged.
pub fn execute_with_config_tracked(
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
) -> (Result<RunWorkflowResult, RunError>, AttemptTrace) {
    let mut trace = AttemptTrace::default();
    let result = execute_tracked_inner(prd_path, agents, config, paths, &mut trace);
    (result, trace)
}

/// What the driver observes about one attempt beyond success or failure.
#[derive(Debug, Default, Clone)]
pub struct AttemptTrace {
    pub execution_id: Option<String>,
    pub retained_reason: Option<&'static str>,
}

fn execute_tracked_inner(
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    trace: &mut AttemptTrace,
) -> Result<RunWorkflowResult, RunError> {
    let current = env::current_dir().map_err(RunError::CurrentDirectory)?;
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(&current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let repository_config = config.repository(&repository.worktree);
    let context = ContextCompiler::new()
        .compile_profiled(
            ContextRequest {
                repository: &current,
                prd: prd_path,
            },
            &context_profile(&repository_config),
        )
        .map_err(RunError::Context)?;
    let context = match config.execution_context.hard_ceiling_tokens {
        Some(hard_ceiling_tokens) => {
            ContextBudgeter::new()
                .budget(
                    context,
                    ContextBudget {
                        hard_ceiling_tokens,
                    },
                )
                .map_err(RunError::ContextBudget)?
                .context
        }
        None => context,
    };
    let prompt = render_prompt(&context);
    let database_path = config.database.resolve_path(&paths.data_dir);
    let mut db = Database::open(&database_path).map_err(|e| RunError::Storage(e.to_string()))?;
    db.run_migrations()
        .map_err(|e| RunError::Storage(e.to_string()))?;
    ReviewRepository::new(db.conn())
        .recover_incomplete()
        .map_err(|e| RunError::Storage(e.to_string()))?;
    let id = new_id();
    trace.execution_id = Some(id.clone());
    // Contradictory agent configuration must fail closed before any claim,
    // regardless of which caller constructed the agents.
    resolved_agent_entries(config).map_err(RunError::Config)?;
    let review_preflight = compute_review_preflight(
        config,
        prd_path,
        &context.prd.path,
        &context.repository.worktree,
        &paths.data_dir,
        &id,
    )?;
    let discovered = discovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| RunError::Config(e.to_string()))?;
    validate_graph(&discovered).map_err(|e| RunError::Config(e.to_string()))?;
    let target = resolve_run_prd(&repository, &discovered, prd_path)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let snapshot = SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&repository, &discovered)
        .map_err(|e| RunError::Storage(e.to_string()))?;
    admit_run_prd(&snapshot, &target).map_err(|e| RunError::Config(e.to_string()))?;
    preflight_execution_prerequisites(agents, config, &context.repository.worktree).map_err(
        |detail| retained_traced(trace, &target, "preflight_failed", RunError::Config(detail)),
    )?;
    let claim_discovered = discovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| RunError::Config(e.to_string()))?;
    validate_graph(&claim_discovered).map_err(|e| RunError::Config(e.to_string()))?;
    let claim_target = resolve_run_prd(&repository, &claim_discovered, prd_path)
        .map_err(|e| RunError::Config(e.to_string()))?;
    if claim_target != target || claim_discovered != discovered {
        return Err(RunError::Config(
            "backlog changed during run admission".into(),
        ));
    }
    if let Some(preflight) = &review_preflight {
        let current = std::fs::read_to_string(prd_path).map_err(|error| {
            RunError::Config(format!("cannot re-read PRD before claim: {error}"))
        })?;
        if current != preflight.prd_bytes {
            return Err(RunError::Config(format!(
                "scope policy compiled against stale PRD {}: content changed during admission",
                preflight.snapshot.prd_path
            )));
        }
    }
    let actor = format!("system:familiar-ai-run:{id}");
    SqliteBacklogRepository::new(db.conn_mut())
        .claim_run(&repository, &discovered, &target, &actor)
        .map_err(|e| RunError::Storage(e.to_string()))?;
    eprintln!(
        "backlog: {} {} pending -> in_progress actor={actor}",
        target.id, target.path
    );
    let started_at = Utc::now().to_rfc3339();
    let timer = Instant::now();
    let mut unavailable = BTreeMap::new();
    for field in [
        "ended_at",
        "duration_ms",
        "agent_version",
        "model",
        "input_tokens",
        "output_tokens",
        "cached_tokens",
        "total_tokens",
        "estimated_cost_microusd",
        "exit_code",
    ] {
        unavailable.insert(field.into(), "runner_interrupted".into());
    }
    if context.repository.git_commit.is_none() {
        unavailable.insert("git_commit".into(), "git_unavailable".into());
    }
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: id.clone(),
            started_at,
            repository: slash(&context.repository.repository),
            worktree: slash(&context.repository.worktree),
            git_commit: context.repository.git_commit.clone(),
            prd_path: context.prd.path.clone(),
            unavailable_fields: unavailable.clone(),
        })
        .map_err(|e| {
            retained_traced(
                trace,
                &target,
                "history_failed",
                RunError::Storage(e.to_string()),
            )
        })?;

    let execution = agents.implementation.execute(
        ExecutionRequest {
            working_directory: &context.repository.worktree,
            denied_read_path: None,
            prompt: &prompt,
            filesystem: familiar_ai_agent::FilesystemPolicy::Normal,
            model: config
                .review
                .enabled
                .then_some(config.review.implementation_agent.model.as_deref())
                .flatten(),
            timeout_ms: None,
        },
        &mut io::stdout(),
    );
    let (result, outcome) = match &execution {
        Ok(result) => (result, outcome(result)),
        Err(AgentExecutionError::Launch { result, .. }) => (result.as_ref(), "launch_failed"),
        Err(AgentExecutionError::Input { result, .. }) => (result.as_ref(), "input_failed"),
        Err(AgentExecutionError::Wait { result, .. }) => (result.as_ref(), "failed"),
        Err(AgentExecutionError::Output { result, .. }) => (result.as_ref(), "output_failed"),
        Err(AgentExecutionError::MalformedOutput { result, .. }) => {
            (result.as_ref(), "malformed_output")
        }
        Err(AgentExecutionError::Timeout { result }) => (result.as_ref(), "timed_out"),
        Err(AgentExecutionError::BudgetExceeded { result, .. }) => {
            (result.as_ref(), "budget_exceeded")
        }
    };
    if result.agent_version.is_none() {
        unavailable.insert("agent_version".into(), "version_probe_failed".into());
    }
    let finalization = terminal(&timer, result, outcome, unavailable, config);
    finalize(&db, &id, &finalization)
        .map_err(|e| retained_traced(trace, &target, "history_failed", e))?;
    let result = execution
        .map_err(|e| retained_traced(trace, &target, agent_reason(&e), RunError::Agent(e)))?;
    if result.exit_code != Some(0) || result.signal.is_some() {
        return Err(retained_traced(
            trace,
            &target,
            "implementation_failed",
            RunError::Workflow {
                result: Some(Box::new(result.clone())),
                detail: "implementation agent did not exit successfully".into(),
            },
        ));
    }
    if !config.review.enabled {
        return Err(retained_traced(
            trace,
            &target,
            "review_disabled",
            RunError::Workflow {
                result: Some(Box::new(result)),
                detail: "review is disabled; backlog completion requires a clean review".into(),
            },
        ));
    }
    let preflight = review_preflight.as_ref().expect("enabled review preflight");
    let cycle = run_review(ReviewRunInput {
        db: &db,
        context: &context,
        execution_id: &id,
        implementation_result: &result,
        implementation_finalization: &finalization,
        implementation_agent: agents.implementation,
        reviewer_agent: agents.reviewer,
        config,
        paths,
        base_revision: &preflight.baseline,
        scope_policy: &preflight.snapshot,
    })
    .map_err(|e| retained_traced(trace, &target, "review_failed", e))?;
    if cycle.state != ReviewCycleState::Completed
        || cycle.disposition != ReviewDisposition::ReadyForHumanApproval
        || cycle.stop_reasons != [ReviewStopReason::CleanReview]
    {
        let reason = review_retained_reason(&cycle);
        return Err(retained_traced(
            trace,
            &target,
            reason,
            RunError::Workflow {
                result: Some(Box::new(result)),
                detail: "review did not produce a clean terminal result".into(),
            },
        ));
    }
    let required_checks = config
        .review
        .verification
        .iter()
        .filter(|c| c.required)
        .map(|c| c.check_id.clone())
        .collect::<Vec<_>>();
    SqliteBacklogRepository::new(db.conn_mut())
        .complete_run(&repository, &target, &id, &actor, &required_checks)
        .map_err(|e| {
            retained_traced(
                trace,
                &target,
                "completion_conflict",
                RunError::Workflow {
                    result: Some(Box::new(result.clone())),
                    detail: e.to_string(),
                },
            )
        })?;
    eprintln!(
        "backlog: {} {} in_progress -> completed actor={actor}",
        target.id, target.path
    );
    Ok(RunWorkflowResult {
        implementation: result,
    })
}

/// Probe every executable that this run is configured to invoke. This is
/// deliberately before backlog admission/claim and performs no model call.
fn preflight_execution_prerequisites(
    agents: &AgentSet<'_>,
    config: &Config,
    repository: &Path,
) -> Result<(), String> {
    let report = crate::preflight::run(agents, config, repository);
    if report.is_valid() {
        Ok(())
    } else {
        Err(report.failure_summary())
    }
}

fn context_profile(config: &familiar_ai_core::RepositoryConfig) -> ContextProfile {
    ContextProfile {
        active_dir: config.active_dir.clone(),
        reference_roots: config
            .resolved_reference_roots()
            .into_iter()
            .map(|root| ContextReferenceRoot {
                prefix: root.prefix,
                kind: match root.kind {
                    familiar_ai_core::ReferenceKind::Prd => ContextReferenceKind::Prd,
                    familiar_ai_core::ReferenceKind::Adr => ContextReferenceKind::Adr,
                    familiar_ai_core::ReferenceKind::Contract => ContextReferenceKind::Contract,
                    familiar_ai_core::ReferenceKind::Supporting => ContextReferenceKind::Supporting,
                },
            })
            .collect(),
    }
}

fn review_retained_reason(cycle: &ReviewCycle) -> &'static str {
    if cycle
        .stop_reasons
        .contains(&ReviewStopReason::ScopeBroadened)
    {
        "scope_broadened"
    } else if cycle
        .stop_reasons
        .contains(&ReviewStopReason::ScopeAmbiguous)
    {
        "scope_ambiguous"
    } else if cycle
        .stop_reasons
        .contains(&ReviewStopReason::VerificationUnsuccessful)
    {
        "verification_failed"
    } else if cycle.stop_reasons.contains(&ReviewStopReason::Interrupted) {
        "interrupted"
    } else {
        "human_review_required"
    }
}

#[derive(Debug)]
struct ReviewPreflight {
    baseline: String,
    snapshot: ScopePolicySnapshot,
    prd_bytes: String,
}

/// Enabled-review preflight: validate configuration, parse the active PRD's
/// Expected Files contract, capture the worktree baseline, and compile the
/// scope-policy snapshot — all before any backlog claim or agent launch.
/// Fails closed without touching backlog state or launching an agent.
fn compute_review_preflight(
    config: &Config,
    prd_path: &Path,
    prd_repository_path: &str,
    worktree: &Path,
    data_dir: &Path,
    execution_id: &str,
) -> Result<Option<ReviewPreflight>, RunError> {
    if !config.review.enabled {
        return Ok(None);
    }
    config.review.validate().map_err(RunError::Config)?;
    let prd_bytes = std::fs::read_to_string(prd_path).map_err(|error| {
        RunError::Config(format!(
            "cannot read PRD for scope policy compilation: {error}"
        ))
    })?;
    let contract = match parse_expected_files(&prd_bytes) {
        Ok(contract) => contract,
        Err(ExpectedFilesError::MissingHeading)
            if config.review.scope.declaration_mode
                == ScopeDeclarationModeConfig::ExpectedOrConfigured =>
        {
            Vec::new()
        }
        Err(error) => {
            return Err(RunError::Config(format!(
                "PRD {prd_repository_path} Expected Files contract is invalid: {error}"
            )))
        }
    };
    let baseline = capture_worktree_baseline(worktree, data_dir, execution_id)?;
    let snapshot = build_scope_policy(
        config,
        prd_repository_path,
        contract,
        content_hash(prd_bytes.as_bytes()),
        &baseline,
    )?;
    Ok(Some(ReviewPreflight {
        baseline,
        snapshot,
        prd_bytes,
    }))
}

fn build_scope_policy(
    config: &Config,
    prd_repository_path: &str,
    contract: Vec<familiar_ai_review::ExpectedFileEntry>,
    prd_content_hash: String,
    baseline: &str,
) -> Result<ScopePolicySnapshot, RunError> {
    let mut prohibited = Vec::new();
    for entry in &config.review.prohibited_changes {
        for rule in entry.resolve().map_err(RunError::Config)? {
            let kind = match (&rule.path, rule.class) {
                (Some(path), None) => {
                    let (normalized, match_kind) = normalize_scope_path(path).map_err(|rule| {
                        RunError::Config(format!("prohibited path '{path}': {rule}"))
                    })?;
                    ProhibitedRuleKind::Path {
                        entry: ScopePathEntry {
                            normalized,
                            match_kind,
                        },
                    }
                }
                (None, Some(class)) => ProhibitedRuleKind::FileClass {
                    class: map_file_class(class),
                },
                _ => {
                    return Err(RunError::Config(format!(
                        "prohibited rule '{}' must declare exactly one of path or class",
                        rule.id
                    )))
                }
            };
            prohibited.push(ProhibitedRule {
                rule_id: rule.id,
                rule: kind,
                change_kinds: rule
                    .change_kinds
                    .iter()
                    .map(|value| map_change_kind(value))
                    .collect::<Result<_, _>>()?,
                description: rule.description,
            });
        }
    }
    let classification = config
        .review
        .scope
        .classification
        .iter()
        .map(|rule| {
            let (normalized, match_kind) = normalize_scope_path(&rule.path).map_err(|error| {
                RunError::Config(format!("scope classification rule '{}': {error}", rule.id))
            })?;
            Ok(ScopeClassificationRule {
                rule_id: rule.id.clone(),
                class: map_file_class(rule.class),
                entry: ScopePathEntry {
                    normalized,
                    match_kind,
                },
                source: ScopeRuleSource::Configuration,
                precedence: rule.precedence,
            })
        })
        .collect::<Result<Vec<_>, RunError>>()?;
    compile_scope_policy(ScopePolicyInput {
        prd_path: prd_repository_path.into(),
        prd_content_hash,
        contract,
        allowed_paths: config.review.allowed_paths.clone(),
        allow_prd_expected_file_expansion: config.review.scope.allow_prd_expected_file_expansion,
        declaration_mode: match config.review.scope.declaration_mode {
            ScopeDeclarationModeConfig::ExpectedOrConfigured => {
                ScopeDeclarationMode::ExpectedOrConfigured
            }
            ScopeDeclarationModeConfig::ExpectedRequired => ScopeDeclarationMode::ExpectedRequired,
        },
        prohibited_rules: prohibited,
        file_class_policies: ScopeFileClassPolicies {
            dependency_manifest: map_class_policy(
                config.review.scope.file_classes.dependency_manifest,
            ),
            dependency_lockfile: map_class_policy(
                config.review.scope.file_classes.dependency_lockfile,
            ),
            migration: map_class_policy(config.review.scope.file_classes.migration),
            configuration: map_class_policy(config.review.scope.file_classes.configuration),
            test: map_class_policy(config.review.scope.file_classes.test),
            generated_artifact: map_class_policy(
                config.review.scope.file_classes.generated_artifact,
            ),
        },
        classification_rules: classification,
        baseline_revision: baseline.into(),
        config_provenance: "config:[review]".into(),
    })
    .map_err(|error| RunError::Config(format!("scope policy compilation failed: {error}")))
}

fn map_file_class(class: ScopeFileClassName) -> ScopeFileClass {
    match class {
        ScopeFileClassName::DependencyManifest => ScopeFileClass::DependencyManifest,
        ScopeFileClassName::DependencyLockfile => ScopeFileClass::DependencyLockfile,
        ScopeFileClassName::Migration => ScopeFileClass::Migration,
        ScopeFileClassName::Configuration => ScopeFileClass::Configuration,
        ScopeFileClassName::Test => ScopeFileClass::Test,
        ScopeFileClassName::GeneratedArtifact => ScopeFileClass::GeneratedArtifact,
    }
}

fn map_class_policy(policy: ScopeClassPolicyConfig) -> ScopeClassPolicy {
    match policy {
        ScopeClassPolicyConfig::Deny => ScopeClassPolicy::Deny,
        ScopeClassPolicyConfig::HumanReview => ScopeClassPolicy::HumanReview,
        ScopeClassPolicyConfig::AllowWhenExpected => ScopeClassPolicy::AllowWhenExpected,
        ScopeClassPolicyConfig::AllowWhenConfigured => ScopeClassPolicy::AllowWhenConfigured,
        ScopeClassPolicyConfig::Allow => ScopeClassPolicy::Allow,
    }
}

fn map_change_kind(value: &str) -> Result<GitChangeKind, RunError> {
    match value {
        "added" => Ok(GitChangeKind::Added),
        "modified" => Ok(GitChangeKind::Modified),
        "deleted" => Ok(GitChangeKind::Deleted),
        "renamed" => Ok(GitChangeKind::Renamed),
        "copied" => Ok(GitChangeKind::Copied),
        "type_changed" => Ok(GitChangeKind::TypeChanged),
        "unmerged" => Ok(GitChangeKind::Unmerged),
        other => Err(RunError::Config(format!(
            "unsupported prohibited change kind '{other}'"
        ))),
    }
}

fn retained_traced(
    trace: &mut AttemptTrace,
    target: &familiar_ai_core::DiscoveredPrd,
    reason: &'static str,
    error: RunError,
) -> RunError {
    trace.retained_reason = Some(reason);
    retained(target, reason, error)
}

fn retained(
    target: &familiar_ai_core::DiscoveredPrd,
    reason: &'static str,
    error: RunError,
) -> RunError {
    eprintln!(
        "backlog: {} {} remains in_progress reason={reason}",
        target.id, target.path
    );
    error
}
fn agent_reason(error: &AgentExecutionError) -> &'static str {
    match error {
        AgentExecutionError::Timeout { .. } => "interrupted",
        AgentExecutionError::BudgetExceeded { .. } => "budget_exceeded",
        AgentExecutionError::MalformedOutput { .. } => "malformed_output",
        _ => "implementation_failed",
    }
}

struct ReviewRunInput<'a> {
    db: &'a Database,
    context: &'a ExecutionContext,
    execution_id: &'a str,
    implementation_result: &'a ExecutionResult,
    implementation_finalization: &'a ExecutionFinalization,
    implementation_agent: &'a dyn CodingAgent,
    reviewer_agent: &'a dyn CodingAgent,
    config: &'a Config,
    paths: &'a AppPaths,
    base_revision: &'a str,
    scope_policy: &'a ScopePolicySnapshot,
}

fn run_review(input: ReviewRunInput<'_>) -> Result<ReviewCycle, RunError> {
    let ReviewRunInput {
        db,
        context,
        execution_id,
        implementation_result,
        implementation_finalization,
        implementation_agent,
        reviewer_agent,
        config,
        paths,
        base_revision,
        scope_policy,
    } = input;
    let implementation = AgentAssignment {
        adapter_id: config.review.implementation_agent.adapter_id.clone(),
        agent_id: config.review.implementation_agent.agent_id.clone(),
        provider: config.review.implementation_agent.provider.clone(),
        requested_model: config
            .review
            .implementation_agent
            .model
            .clone()
            .or_else(|| implementation_result.model.clone()),
        role: AgentRole::Implementation,
        session_id: None,
    };
    let reviewer = AgentAssignment {
        adapter_id: config.review.reviewer_agent.adapter_id.clone(),
        agent_id: config.review.reviewer_agent.agent_id.clone(),
        provider: config.review.reviewer_agent.provider.clone(),
        requested_model: config.review.reviewer_agent.model.clone(),
        role: AgentRole::Review,
        session_id: None,
    };
    let criteria = acceptance_criteria(&context.prd.content);
    if criteria.is_empty() {
        return Err(RunError::Config(
            "enabled review requires an explicit PRD Acceptance Criteria section".into(),
        ));
    }
    let task = ReviewTask {
        task_id: execution_id.into(),
        objective: markdown_section(&context.prd.content, "Objective")
            .unwrap_or_else(|| context.prd.content.clone()),
        acceptance_criteria: criteria,
        base_revision: base_revision.into(),
        allowed_paths: scope_policy
            .allowed_paths
            .iter()
            .map(|entry| entry.normalized.clone())
            .collect(),
        prohibited_changes: scope_policy
            .prohibited_rules
            .iter()
            .map(|rule| rule.rule_id.clone())
            .collect(),
        verification_plan_id: format!("{execution_id}-verification"),
    };
    let policy = BlockingPolicy::default();
    let repository = ReviewRepository::new(db.conn());
    repository
        .insert_task(&task, &policy)
        .map_err(|e| RunError::Storage(e.to_string()))?;
    let artifact_directory = paths.data_dir.join("review-artifacts");
    let collector =
        GitEvidenceCollector::new(artifact_directory.clone(), config.review.max_evidence_bytes);
    let verifier = CommandVerificationRunner::new(
        artifact_directory,
        config.review.max_verification_log_bytes,
    );
    let review_timeout = nonzero(config.review.max_total_duration_ms)
        .map(|value| value / u64::from(config.review.max_review_attempts))
        .unwrap_or(300_000)
        .max(1);
    let reviewer_adapter = StructuredReviewAdapter::new(
        reviewer_agent,
        context.repository.worktree.clone(),
        reviewer.clone(),
        review_timeout,
    );
    let remediation_adapter = CodingRemediationAdapter::new(
        implementation_agent,
        context.repository.worktree.clone(),
        implementation.clone(),
    );
    let checks = config
        .review
        .verification
        .iter()
        .map(|check| VerificationCheck {
            check_id: check.check_id.clone(),
            argv: check.argv.clone(),
            working_directory: check.working_directory.clone(),
            environment: check.environment.clone(),
            timeout_ms: check.timeout_ms,
            required: check.required,
            path_prefixes: check.path_prefixes.clone(),
        })
        .collect();
    let coordinator = ReviewCoordinator {
        collector: &collector,
        verifier: &verifier,
        reviewer: &reviewer_adapter,
        implementer: &remediation_adapter,
        store: &repository,
        policy,
    };
    let contracts = context
        .documents
        .iter()
        .filter(|document| {
            matches!(
                document.kind,
                familiar_ai_context::DocumentKind::Contract
                    | familiar_ai_context::DocumentKind::Adr
            )
        })
        .map(|document| BoundedDocument {
            source: document.path.clone(),
            content: document.content.clone(),
            content_hash: familiar_ai_review::content_hash(document.content.as_bytes()),
            selection_reason: "direct task reference".into(),
            estimated_tokens: document.estimated_tokens,
        })
        .collect();
    let request = CoordinationRequest {
        cycle_id: format!("{execution_id}-cycle"),
        task,
        implementation: AgentObservation {
            assignment: implementation,
            agent_version: implementation_result.agent_version.clone(),
            reported_model: implementation_result.model.clone(),
            unavailable_fields: BTreeMap::new(),
        },
        reviewer,
        contracts,
        invariants: Vec::new(),
        verification_plan: VerificationPlan {
            plan_id: format!("{execution_id}-verification"),
            checks,
            full_after_remediation: false,
        },
        package_budget: ReviewPackageBudget {
            max_bytes: config.review.max_package_bytes,
            max_estimated_tokens: config.review.max_package_tokens,
        },
        limits: WorkflowLimits {
            max_review_attempts: config.review.max_review_attempts,
            max_remediation_attempts: config.review.max_remediation_attempts,
            max_total_tokens: nonzero(config.review.max_total_tokens),
            max_total_cost_microusd: nonzero(config.review.max_total_cost_microusd),
            max_total_duration_ms: nonzero(config.review.max_total_duration_ms),
            review_reservation_tokens: nonzero(config.review.max_total_tokens)
                .map(|v| v / u64::from(config.review.max_review_attempts)),
            remediation_reservation_tokens: nonzero(config.review.max_total_tokens)
                .map(|v| v / u64::from(config.review.max_remediation_attempts)),
            action_reservation_cost_microusd: nonzero(config.review.max_total_cost_microusd).map(
                |v| {
                    v / u64::from(
                        config.review.max_review_attempts + config.review.max_remediation_attempts,
                    )
                },
            ),
            action_reservation_duration_ms: nonzero(config.review.max_total_duration_ms)
                .map(|v| {
                    v / u64::from(
                        config.review.max_review_attempts
                            + config.review.max_remediation_attempts
                            + 1,
                    )
                })
                .unwrap_or(300_000)
                .max(1),
        },
        allow_same_model_fallback: config.review.allow_isolated_same_model_fallback,
        scope_policy: scope_policy.clone(),
        implementation_usage: familiar_ai_review::ExecutionUsage {
            input_tokens: implementation_result.input_tokens,
            output_tokens: implementation_result.output_tokens,
            cached_tokens: implementation_result.cached_tokens,
            total_tokens: implementation_finalization.total_tokens,
            estimated_cost_microusd: implementation_finalization.estimated_cost_microusd,
            pricing_provenance: implementation_finalization
                .estimated_cost_microusd
                .map(|_| "execution_history_pricing".into()),
            unavailable_fields: implementation_finalization.unavailable_fields.clone(),
        },
        implementation_duration_ms: implementation_finalization.duration_ms,
    };
    let cycle = coordinator
        .run(&context.repository.worktree, request, &mut io::stdout())
        .map_err(|e| RunError::Storage(format!("review workflow failed: {e}")))?;
    println!(
        "Review disposition: {:?}; independence: {:?}; stop reasons: {:?}",
        cycle.disposition,
        cycle.independence.as_ref().map(|value| value.kind),
        cycle.stop_reasons
    );
    report_scope_findings(&cycle);
    Ok(cycle)
}

const SCOPE_FINDING_RENDER_LIMIT: usize = 50;

/// Render the exact per-file scope decisions for a scope-terminated cycle.
/// Canonical findings are already persisted; rendering is bounded and any
/// omission is reported with the exact count and snapshot artifact reference.
fn report_scope_findings(cycle: &ReviewCycle) {
    if !cycle
        .stop_reasons
        .contains(&ReviewStopReason::ScopeBroadened)
        && !cycle
            .stop_reasons
            .contains(&ReviewStopReason::ScopeAmbiguous)
    {
        return;
    }
    let Some(evaluation) = cycle.scope_evaluations.last() else {
        return;
    };
    println!(
        "Scope evaluation '{}' under policy {}:",
        evaluation.phase, evaluation.policy_snapshot_hash
    );
    for finding in evaluation.findings.iter().take(SCOPE_FINDING_RENDER_LIMIT) {
        let old = finding
            .old_path
            .as_deref()
            .map(|old| format!(" (from {old})"))
            .unwrap_or_default();
        println!(
            "scope: {:?} {:?} {}{} rule={}",
            finding.decision, finding.change_kind, finding.path, old, finding.rule_id
        );
    }
    if evaluation.findings.len() > SCOPE_FINDING_RENDER_LIMIT {
        println!(
            "scope: {} additional findings omitted from rendering; all findings persisted under snapshot artifact {}",
            evaluation.findings.len() - SCOPE_FINDING_RENDER_LIMIT,
            cycle
                .scope_policy_snapshot
                .as_ref()
                .map(|artifact| artifact.content_hash.as_str())
                .unwrap_or("<unpersisted>")
        );
    }
}

fn markdown_section(document: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}");
    let start = document.lines().position(|line| line.trim() == marker)?;
    let lines: Vec<_> = document
        .lines()
        .skip(start + 1)
        .take_while(|line| !line.starts_with("## "))
        .collect();
    let value = lines.join("\n").trim().to_owned();
    (!value.is_empty()).then_some(value)
}
fn acceptance_criteria(document: &str) -> Vec<String> {
    let lines: Vec<_> = document.lines().collect();
    let Some(start) = lines.iter().position(|line| {
        let Some(heading) = line.trim().strip_prefix("## ") else {
            return false;
        };
        let heading = heading.trim().to_ascii_lowercase();
        heading == "acceptance criteria" || heading.ends_with(" acceptance criteria")
    }) else {
        return Vec::new();
    };

    let mut criteria = Vec::new();
    for raw in lines
        .into_iter()
        .skip(start + 1)
        .take_while(|line| !line.starts_with("## "))
    {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let numbered = line.split_once(". ").and_then(|(number, value)| {
            number
                .chars()
                .all(|character| character.is_ascii_digit())
                .then_some(value)
        });
        let bullet = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "));
        if let Some(criterion) = numbered.or(bullet) {
            criteria.push(criterion.to_owned());
        } else if let Some(criterion) = criteria.last_mut() {
            criterion.push(' ');
            criterion.push_str(line);
        }
    }
    criteria
}

fn capture_worktree_baseline(
    worktree: &Path,
    data_dir: &Path,
    execution_id: &str,
) -> Result<String, RunError> {
    std::fs::create_dir_all(data_dir).map_err(|error| RunError::Config(error.to_string()))?;
    let index = data_dir.join(format!("review-baseline-{execution_id}.index"));
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(worktree)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .map_err(|error| RunError::Config(format!("cannot capture review baseline: {error}")))
            .and_then(|output| {
                if output.status.success() {
                    Ok(output.stdout)
                } else {
                    Err(RunError::Config(format!(
                        "cannot capture review baseline: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    )))
                }
            })
    };
    let result = (|| {
        let has_head = Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(worktree)
            .output()
            .is_ok_and(|output| output.status.success());
        if has_head {
            run(&["read-tree", "HEAD"])?;
        } else {
            run(&["read-tree", "--empty"])?;
        }
        run(&["add", "-A"])?;
        let tree = run(&["write-tree"])?;
        String::from_utf8(tree)
            .map(|value| value.trim().to_owned())
            .map_err(|error| RunError::Config(error.to_string()))
    })();
    match std::fs::remove_file(&index) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(RunError::Config(format!(
                "cannot remove temporary review baseline index: {error}"
            )))
        }
    }
    result
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn outcome(result: &ExecutionResult) -> &'static str {
    match result.exit_code {
        Some(0) => "succeeded",
        Some(_) => "failed",
        None if result.signal.is_some() => "signaled",
        None => "failed",
    }
}

fn finalize(db: &Database, id: &str, value: &ExecutionFinalization) -> Result<(), RunError> {
    ExecutionHistoryRepository::new(db.conn())
        .finalize(id, value)
        .map_err(|e| RunError::HistoryFinalize {
            execution_id: id.into(),
            detail: e.to_string(),
        })
}

fn terminal(
    timer: &Instant,
    result: &ExecutionResult,
    outcome: &str,
    mut missing: BTreeMap<String, String>,
    config: &Config,
) -> ExecutionFinalization {
    for field in ["ended_at", "duration_ms", "signal"] {
        missing.remove(field);
    }
    if result.agent_version.is_some() {
        missing.remove("agent_version");
    }
    if result.exit_code.is_some() {
        missing.remove("exit_code");
    } else {
        missing.insert("exit_code".into(), "no_normal_exit_code".into());
    }
    if result.model.is_none() {
        missing.insert("model".into(), "agent_not_reported".into());
    } else {
        missing.remove("model");
    }
    for (name, value) in [
        ("input_tokens", result.input_tokens),
        ("output_tokens", result.output_tokens),
        ("cached_tokens", result.cached_tokens),
    ] {
        if value.is_none() {
            missing.insert(name.into(), "usage_not_reported".into());
        } else {
            missing.remove(name);
        }
    }
    let total = match (result.input_tokens, result.output_tokens) {
        (Some(a), Some(b)) => a.checked_add(b).filter(|value| *value <= i64::MAX as u64),
        _ => None,
    };
    if total.is_none() {
        missing.insert(
            "total_tokens".into(),
            if result.input_tokens.is_some() && result.output_tokens.is_some() {
                "arithmetic_overflow"
            } else {
                "usage_incomplete"
            }
            .into(),
        );
    } else {
        missing.remove("total_tokens");
    }
    let price = result
        .model
        .as_ref()
        .and_then(|m| config.execution_history.pricing.get(m));
    let (cost, rates, reason) = calculate_cost(
        result.input_tokens,
        result.cached_tokens,
        result.output_tokens,
        price,
    );
    if cost.is_none() {
        missing.insert("estimated_cost_microusd".into(), reason.into());
    } else {
        missing.remove("estimated_cost_microusd");
    }
    ExecutionFinalization {
        ended_at: Utc::now().to_rfc3339(),
        duration_ms: u64::try_from(timer.elapsed().as_millis())
            .unwrap_or(i64::MAX as u64)
            .min(i64::MAX as u64),
        agent_version: result.agent_version.clone(),
        model: result.model.clone(),
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        cached_tokens: result.cached_tokens,
        total_tokens: total,
        estimated_cost_microusd: cost,
        input_rate: rates.0,
        cached_input_rate: rates.1,
        output_rate: rates.2,
        outcome: outcome.into(),
        exit_code: result.exit_code,
        signal: result.signal,
        unavailable_fields: missing,
    }
}

#[allow(clippy::type_complexity)]
pub fn calculate_cost(
    input: Option<u64>,
    cached: Option<u64>,
    output: Option<u64>,
    price: Option<&ExecutionPrice>,
) -> (
    Option<u64>,
    (Option<u64>, Option<u64>, Option<u64>),
    &'static str,
) {
    let Some(price) = price else {
        return (None, (None, None, None), "pricing_not_configured");
    };
    let rates = (
        price.input_microusd_per_million,
        price.cached_input_microusd_per_million,
        price.output_microusd_per_million,
    );
    if [rates.0, rates.1, rates.2]
        .into_iter()
        .flatten()
        .any(|rate| rate > i64::MAX as u64)
    {
        return (None, (None, None, None), "arithmetic_overflow");
    }
    let (Some(ir), Some(cr), Some(or)) = rates else {
        return (None, rates, "pricing_rate_incomplete");
    };
    let (Some(i), Some(c), Some(o)) = (input, cached, output) else {
        return (None, rates, "usage_incomplete");
    };
    let Some(u) = i.checked_sub(c) else {
        return (None, rates, "usage_incomplete");
    };
    let numerator = u
        .checked_mul(ir)
        .and_then(|x| c.checked_mul(cr).and_then(|y| x.checked_add(y)))
        .and_then(|x| o.checked_mul(or).and_then(|y| x.checked_add(y)));
    let Some(n) = numerator else {
        return (None, rates, "arithmetic_overflow");
    };
    let Some(rounded) = n.checked_add(500_000) else {
        return (None, rates, "arithmetic_overflow");
    };
    let cost = rounded / 1_000_000;
    if cost > i64::MAX as u64 {
        return (None, rates, "arithmetic_overflow");
    }
    (Some(cost), rates, "")
}

pub(crate) fn new_id() -> String {
    format!(
        "{:020}-{:010}-{:06}",
        Utc::now().timestamp_micros(),
        std::process::id(),
        EXECUTION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
pub fn build_prompt(repository_root: &Path, supplied_path: &Path) -> Result<String, RunError> {
    let supplied = if supplied_path.is_absolute() {
        supplied_path.to_owned()
    } else {
        env::current_dir()
            .map_err(RunError::CurrentDirectory)?
            .join(supplied_path)
    };
    let context = ContextCompiler::new()
        .compile(ContextRequest {
            repository: repository_root,
            prd: &supplied,
        })
        .map_err(RunError::Context)?;
    Ok(render_prompt(&context))
}

fn render_prompt(context: &ExecutionContext) -> String {
    let mut prompt = String::new();
    prompt.push_str("# Familiar execution request\n\nImplement the PRD below in this repository.\n\n## Fixed execution constraints\n\n");
    prompt.push_str(EXECUTION_CONSTRAINTS);
    prompt.push_str("\n\n## PRD: ");
    prompt.push_str(&context.prd.path);
    prompt.push_str("\n\n");
    prompt.push_str(&context.prd.content);
    prompt.push('\n');
    for document in &context.documents {
        prompt.push_str("\n## Directly referenced document: ");
        prompt.push_str(&document.path);
        prompt.push_str("\n\n");
        prompt.push_str(&document.content);
        prompt.push('\n');
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_context::{ContextDocument, DocumentKind, InclusionReason, RepositoryContext};
    use familiar_ai_review::{
        FindingCategory, FindingEvidence, FindingSeverity, FindingStatus, ReviewDisposition,
        ReviewFinding, ReviewRequest, ReviewResult,
    };
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeAgent {
        request: Mutex<Option<(PathBuf, String)>>,
        result: ExecutionResult,
    }

    impl CodingAgent for FakeAgent {
        fn execute(
            &self,
            request: ExecutionRequest<'_>,
            _output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            *self.request.lock().unwrap() = Some((
                request.working_directory.to_owned(),
                request.prompt.to_owned(),
            ));
            Ok(self.result.clone())
        }
    }

    fn legacy_prompt(repository: &Path, prd_path: &Path) -> String {
        let prd = fs::read_to_string(prd_path).unwrap();
        let mut references = BTreeSet::new();
        for prefix in ["docs/adr/", "docs/contracts/"] {
            let mut rest = prd.as_str();
            while let Some(index) = rest.find(prefix) {
                rest = &rest[index..];
                let end = rest
                    .find(|character: char| {
                        !(character.is_ascii_alphanumeric()
                            || matches!(character, '/' | '-' | '_' | '.'))
                    })
                    .unwrap_or(rest.len());
                let reference = rest[..end].trim_end_matches('.');
                if reference.ends_with(".md") {
                    references.insert(reference.to_owned());
                }
                rest = &rest[end..];
            }
        }
        let identity = prd_path.strip_prefix(repository).unwrap().to_string_lossy();
        let mut prompt = format!(
            "# Familiar execution request\n\nImplement the PRD below in this repository.\n\n## Fixed execution constraints\n\n{EXECUTION_CONSTRAINTS}\n\n## PRD: {identity}\n\n{prd}\n"
        );
        for reference in references {
            prompt.push_str(&format!(
                "\n## Directly referenced document: {reference}\n\n{}\n",
                fs::read_to_string(repository.join(&reference)).unwrap()
            ));
        }
        prompt
    }

    #[test]
    fn prompt_bytes_match_legacy_renderer() {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let prd = repository.join("docs/prds/PRD-003.md");
        assert_eq!(
            build_prompt(&repository, &prd).unwrap().as_bytes(),
            legacy_prompt(&repository, &prd).as_bytes()
        );
    }

    #[test]
    fn supporting_document_uses_existing_rendering_form() {
        let context = ExecutionContext {
            repository: RepositoryContext {
                repository: PathBuf::from("repo"),
                worktree: PathBuf::from("worktree"),
                git_commit: None,
            },
            prd: ContextDocument {
                path: "docs/prds/work.md".into(),
                kind: DocumentKind::Prd,
                content: "prd".into(),
                inclusion: InclusionReason::RequestedPrd,
                estimated_tokens: 1,
            },
            documents: vec![ContextDocument {
                path: "docs/supporting/input.md".into(),
                kind: DocumentKind::Supporting,
                content: "support".into(),
                inclusion: InclusionReason::DirectReference {
                    referenced_by: "docs/prds/work.md".into(),
                },
                estimated_tokens: 2,
            }],
            estimated_tokens: 3,
        };
        assert!(render_prompt(&context)
            .contains("\n## Directly referenced document: docs/supporting/input.md\n\nsupport\n"));
    }

    #[test]
    fn orchestration_uses_neutral_agent_and_preserves_history_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let mut config = Config::default();
        let database_path = temp.path().join("history.db");
        config.database.path = Some(database_path.clone());
        let paths = AppPaths {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            state_dir: temp.path().join("state"),
            runtime_dir: temp.path().join("runtime"),
            log_dir: temp.path().join("log"),
            socket_path: temp.path().join("runtime/socket"),
            pid_path: temp.path().join("state/pid"),
        };
        let agent = FakeAgent {
            request: Mutex::new(None),
            result: ExecutionResult {
                agent_version: Some("test-agent 1".into()),
                model: Some("priced-model".into()),
                input_tokens: Some(10),
                output_tokens: Some(4),
                cached_tokens: Some(3),
                exit_code: Some(23),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            },
        };

        let error = execute_with_config(
            &repository.join("docs/prds/PRD-004.md"),
            &same_agent(&agent),
            &config,
            &paths,
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), Some(23));
        let captured = agent.request.lock().unwrap();
        let (working_directory, prompt) = captured.as_ref().unwrap();
        assert_eq!(working_directory, &repository);
        assert_eq!(
            prompt,
            &build_prompt(&repository, &repository.join("docs/prds/PRD-004.md")).unwrap()
        );

        let database = Database::open(&database_path).unwrap();
        let rows = ExecutionHistoryRepository::new(database.conn())
            .recent(1)
            .unwrap();
        assert_eq!(rows[0].outcome, "failed");
        assert_eq!(rows[0].exit_code, Some(23));
        assert_eq!(rows[0].input_tokens, Some(10));
        assert_eq!(rows[0].total_tokens, Some(14));
    }

    #[test]
    fn all_fit_budget_preserves_prompt_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let prd = repository.join("docs/prds/PRD-003.md");
        let expected = build_prompt(&repository, &prd).unwrap();
        let mut config = Config::default();
        config.execution_context.hard_ceiling_tokens = Some(u64::MAX);
        config.database.path = Some(temp.path().join("history.db"));
        let paths = test_paths(temp.path());
        let agent = successful_fake_agent();

        execute_with_config(&prd, &same_agent(&agent), &config, &paths).unwrap_err();

        let captured = agent.request.lock().unwrap();
        assert_eq!(captured.as_ref().unwrap().1.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn selective_budget_renders_only_selected_whole_documents() {
        let temp = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let prd = repository.join("docs/prds/PRD-003.md");
        let complete = ContextCompiler
            .compile(ContextRequest {
                repository: &repository,
                prd: &prd,
            })
            .unwrap();
        let ceiling = complete.prd.estimated_tokens;
        let expected = ContextBudgeter
            .budget(
                complete,
                ContextBudget {
                    hard_ceiling_tokens: ceiling,
                },
            )
            .unwrap();
        assert!(expected.context.documents.is_empty());
        assert!(expected.report.decisions.len() > 1);
        assert!(expected.report.excluded_estimated_tokens > 0);
        let expected_prompt = render_prompt(&expected.context);
        let mut config = Config::default();
        config.execution_context.hard_ceiling_tokens = Some(ceiling);
        config.database.path = Some(temp.path().join("history.db"));
        let paths = test_paths(temp.path());
        let agent = successful_fake_agent();

        execute_with_config(&prd, &same_agent(&agent), &config, &paths).unwrap_err();

        let captured = agent.request.lock().unwrap();
        assert_eq!(captured.as_ref().unwrap().1, expected_prompt);
    }

    #[test]
    fn prd_over_budget_fails_before_history_and_agent() {
        let temp = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let database_path = temp.path().join("history.db");
        let mut config = Config::default();
        config.execution_context.hard_ceiling_tokens = Some(0);
        config.database.path = Some(database_path.clone());
        let paths = test_paths(temp.path());
        let agent = successful_fake_agent();

        let error = execute_with_config(
            &repository.join("docs/prds/PRD-007.md"),
            &same_agent(&agent),
            &config,
            &paths,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            RunError::ContextBudget(ContextBudgetError::PrdExceedsHardCeiling { .. })
        ));
        assert!(agent.request.lock().unwrap().is_none());
        assert!(!database_path.exists());
    }

    fn same_agent<'a>(agent: &'a dyn CodingAgent) -> AgentSet<'a> {
        AgentSet {
            implementation: agent,
            reviewer: agent,
        }
    }

    fn successful_fake_agent() -> FakeAgent {
        FakeAgent {
            request: Mutex::new(None),
            result: ExecutionResult {
                agent_version: Some("test-agent 1".into()),
                model: None,
                input_tokens: None,
                output_tokens: None,
                cached_tokens: None,
                exit_code: Some(0),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            },
        }
    }

    fn test_paths(root: &Path) -> AppPaths {
        AppPaths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            runtime_dir: root.join("runtime"),
            log_dir: root.join("log"),
            socket_path: root.join("runtime/socket"),
            pid_path: root.join("state/pid"),
        }
    }

    #[test]
    fn cost_uses_cached_rate_and_rounds_half_up() {
        let p = ExecutionPrice {
            input_microusd_per_million: Some(2_000_000),
            cached_input_microusd_per_million: Some(1_000_000),
            output_microusd_per_million: Some(3_000_000),
        };
        assert_eq!(
            calculate_cost(Some(10), Some(4), Some(2), Some(&p)).0,
            Some(22)
        );
    }
    #[test]
    fn missing_cached_usage_is_not_zero() {
        let p = ExecutionPrice {
            input_microusd_per_million: Some(1),
            cached_input_microusd_per_million: Some(1),
            output_microusd_per_million: Some(1),
        };
        assert_eq!(
            calculate_cost(Some(1), None, Some(1), Some(&p)).2,
            "usage_incomplete"
        );
    }

    struct WorkflowFakeAgent {
        repository: PathBuf,
        remediation: bool,
        reviews: Mutex<u32>,
    }
    impl CodingAgent for WorkflowFakeAgent {
        fn isolation_capability(&self) -> familiar_ai_agent::IsolationCapability {
            familiar_ai_agent::IsolationCapability::FreshProcessPerExecution
        }
        fn execute(
            &self,
            request: ExecutionRequest<'_>,
            output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            if request
                .prompt
                .starts_with("You are an independent code reviewer")
            {
                let marker = "REVIEW_PACKAGE_JSON:\n";
                let json = request.prompt.split_once(marker).unwrap().1;
                let package: ReviewRequest = serde_json::from_str(json).unwrap();
                let mut count = self.reviews.lock().unwrap();
                *count += 1;
                let findings = if self.remediation {
                    let status = if *count == 1 {
                        FindingStatus::Open
                    } else {
                        FindingStatus::Resolved
                    };
                    vec![ReviewFinding {
                        finding_id: "finding".into(),
                        category: FindingCategory::CorrectnessDefect,
                        severity: FindingSeverity::High,
                        blocking: false,
                        title: "incorrect value".into(),
                        claim: "implementation value requires remediation".into(),
                        evidence: vec![
                            FindingEvidence::FileRange {
                                path: "src/lib.rs".into(),
                                range: familiar_ai_review::LineRange { start: 1, end: 1 },
                            },
                            FindingEvidence::Verification {
                                check_id: "verify".into(),
                                output: package.verification[0].stdout.clone().unwrap(),
                            },
                        ],
                        remediation: "write the corrected value".into(),
                        status,
                        supersedes: None,
                    }]
                } else {
                    vec![]
                };
                let result = ReviewResult {
                    review_id: package.review_id.clone(),
                    reviewer: familiar_ai_review::AgentObservation {
                        assignment: package.reviewer.clone(),
                        agent_version: None,
                        reported_model: None,
                        unavailable_fields: BTreeMap::new(),
                    },
                    started_at: "2026-08-03T00:00:00Z".into(),
                    ended_at: "2026-08-03T00:00:00Z".into(),
                    duration_ms: 1,
                    findings,
                    reviewed_manifest_hash: package.manifest.manifest_hash,
                    usage: familiar_ai_review::ExecutionUsage {
                        input_tokens: Some(1),
                        output_tokens: Some(1),
                        cached_tokens: Some(0),
                        total_tokens: Some(2),
                        estimated_cost_microusd: None,
                        pricing_provenance: None,
                        unavailable_fields: BTreeMap::new(),
                    },
                    disposition: ReviewDisposition::Pending,
                    unavailable_fields: BTreeMap::new(),
                };
                write!(output, "{}", serde_json::to_string(&result).unwrap()).unwrap();
            } else if request.prompt.starts_with("Remediate only") {
                fs::write(self.repository.join("src/lib.rs"), "corrected\n").unwrap();
            }
            Ok(ExecutionResult {
                agent_version: Some("fake 1".into()),
                model: request.model.map(str::to_owned),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cached_tokens: Some(0),
                exit_code: Some(0),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            })
        }
    }

    #[allow(clippy::type_complexity)]
    fn production_review_fixture(
        remediation: bool,
    ) -> (
        tempfile::TempDir,
        Database,
        ExecutionContext,
        Config,
        AppPaths,
        String,
        WorkflowFakeAgent,
        ExecutionFinalization,
        ScopePolicySnapshot,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repository)
            .status()
            .unwrap();
        fs::write(repository.join("src/lib.rs"), "base\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(&repository)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "base",
            ])
            .current_dir(&repository)
            .status()
            .unwrap();
        let paths = test_paths(&temp.path().join("app"));
        let baseline = capture_worktree_baseline(&repository, &paths.data_dir, "test").unwrap();
        fs::write(repository.join("src/lib.rs"), "implemented\n").unwrap();
        let context = ExecutionContext {
            repository: RepositoryContext {
                repository: repository.join(".git"),
                worktree: repository.clone(),
                git_commit: Some(baseline.clone()),
            },
            prd: ContextDocument {
                path: "docs/prds/test.md".into(),
                kind: DocumentKind::Prd,
                content: "## Objective\nobjective\n\n## Acceptance Criteria\n1. criterion\n".into(),
                inclusion: InclusionReason::RequestedPrd,
                estimated_tokens: 1,
            },
            documents: vec![],
            estimated_tokens: 1,
        };
        let mut config = Config::default();
        config.review.enabled = true;
        config.review.max_review_attempts = 3;
        config.review.max_remediation_attempts = 2;
        config.review.max_total_duration_ms = 10_000;
        config.review.allow_isolated_same_model_fallback = false;
        config.review.allowed_paths = vec!["src/".into()];
        config.review.prohibited_changes = vec!["dependency changes".into()];
        config.review.implementation_agent = familiar_ai_core::config::ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: Some("fake".into()),
            model: Some("implementation-model".into()),
        };
        config.review.reviewer_agent = familiar_ai_core::config::ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: Some("fake".into()),
            model: Some("review-model".into()),
        };
        config.review.verification = vec![familiar_ai_core::config::ReviewVerificationConfig {
            check_id: "verify".into(),
            argv: vec!["/usr/bin/true".into()],
            working_directory: ".".into(),
            timeout_ms: 1_000,
            required: true,
            path_prefixes: vec!["src/".into()],
            environment: BTreeMap::new(),
        }];
        let database = Database::open_in_memory().unwrap();
        database.run_migrations().unwrap();
        let agent = WorkflowFakeAgent {
            repository,
            remediation,
            reviews: Mutex::new(0),
        };
        let finalization = ExecutionFinalization {
            ended_at: "2026-08-03T00:00:00Z".into(),
            duration_ms: 1,
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            total_tokens: Some(2),
            outcome: "succeeded".into(),
            exit_code: Some(0),
            unavailable_fields: BTreeMap::from([(
                "estimated_cost_microusd".into(),
                "pricing_not_configured".into(),
            )]),
            ..Default::default()
        };
        let prd_spec = format!("{}\n## Expected Files\n\n- `src/`\n", context.prd.content);
        let snapshot = build_scope_policy(
            &config,
            &context.prd.path,
            parse_expected_files(&prd_spec).unwrap(),
            content_hash(prd_spec.as_bytes()),
            &baseline,
        )
        .unwrap();
        (
            temp,
            database,
            context,
            config,
            paths,
            baseline,
            agent,
            finalization,
            snapshot,
        )
    }

    #[test]
    fn production_composition_handles_clean_review_and_one_remediation() {
        for remediation in [false, true] {
            let (_temp, db, context, config, paths, baseline, agent, finalization, snapshot) =
                production_review_fixture(remediation);
            run_review(ReviewRunInput {
                db: &db,
                context: &context,
                execution_id: if remediation { "remediation" } else { "clean" },
                implementation_result: &ExecutionResult {
                    agent_version: Some("fake".into()),
                    model: Some("implementation-model".into()),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    cached_tokens: Some(0),
                    exit_code: Some(0),
                    signal: None,
                    session_id: None,
                    reported_cost_microusd: None,
                },
                implementation_finalization: &finalization,
                implementation_agent: &agent,
                reviewer_agent: &agent,
                config: &config,
                paths: &paths,
                base_revision: &baseline,
                scope_policy: &snapshot,
            })
            .unwrap();
            let cycle = ReviewRepository::new(db.conn())
                .get_cycle(if remediation {
                    "remediation-cycle"
                } else {
                    "clean-cycle"
                })
                .unwrap()
                .unwrap();
            assert_eq!(cycle.disposition, ReviewDisposition::ReadyForHumanApproval);
            assert_eq!(
                *agent.reviews.lock().unwrap(),
                if remediation { 2 } else { 1 }
            );
            if remediation {
                assert_eq!(
                    fs::read_to_string(context.repository.worktree.join("src/lib.rs")).unwrap(),
                    "corrected\n"
                );
            }
        }
    }

    /// Delegates to the workflow fake but records calls and asserts it only
    /// ever receives prompts for its configured role.
    struct RoleProbe<'a> {
        inner: &'a WorkflowFakeAgent,
        review_role: bool,
        calls: Mutex<u32>,
    }
    impl CodingAgent for RoleProbe<'_> {
        fn isolation_capability(&self) -> familiar_ai_agent::IsolationCapability {
            self.inner.isolation_capability()
        }
        fn execute(
            &self,
            request: ExecutionRequest<'_>,
            output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            let is_review = request
                .prompt
                .starts_with("You are an independent code reviewer");
            assert_eq!(
                is_review, self.review_role,
                "agent received a prompt for the wrong role"
            );
            *self.calls.lock().unwrap() += 1;
            self.inner.execute(request, output)
        }
    }

    #[test]
    fn split_agents_route_review_and_remediation_to_the_right_role() {
        let (_temp, db, context, config, paths, baseline, agent, finalization, snapshot) =
            production_review_fixture(true);
        let implementer = RoleProbe {
            inner: &agent,
            review_role: false,
            calls: Mutex::new(0),
        };
        let reviewer = RoleProbe {
            inner: &agent,
            review_role: true,
            calls: Mutex::new(0),
        };
        let cycle = run_review(ReviewRunInput {
            db: &db,
            context: &context,
            execution_id: "split",
            implementation_result: &ExecutionResult {
                agent_version: Some("fake".into()),
                model: Some("implementation-model".into()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cached_tokens: Some(0),
                exit_code: Some(0),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            },
            implementation_finalization: &finalization,
            implementation_agent: &implementer,
            reviewer_agent: &reviewer,
            config: &config,
            paths: &paths,
            base_revision: &baseline,
            scope_policy: &snapshot,
        })
        .unwrap();
        assert_eq!(cycle.disposition, ReviewDisposition::ReadyForHumanApproval);
        assert_eq!(*reviewer.calls.lock().unwrap(), 2);
        assert_eq!(*implementer.calls.lock().unwrap(), 1);
    }

    #[test]
    fn budget_exceedance_maps_to_a_distinct_retained_reason() {
        let error = AgentExecutionError::BudgetExceeded {
            limit_microusd: 1,
            reported_microusd: 2,
            result: Box::default(),
        };
        assert_eq!(agent_reason(&error), "budget_exceeded");
        assert_eq!(error.result().exit_code, None);
    }

    #[test]
    fn expansion_enabled_justified_expected_file_reaches_clean_review() {
        let (_temp, db, context, mut config, paths, baseline, agent, finalization, _) =
            production_review_fixture(false);
        fs::write(context.repository.worktree.join("docs-notes.md"), "notes\n").unwrap();
        config.review.scope.allow_prd_expected_file_expansion = true;
        let prd_spec = format!(
            "{}\n## Expected Files\n\n- `src/`\n- `docs-notes.md`\n",
            context.prd.content
        );
        let snapshot = build_scope_policy(
            &config,
            &context.prd.path,
            parse_expected_files(&prd_spec).unwrap(),
            content_hash(prd_spec.as_bytes()),
            &baseline,
        )
        .unwrap();
        let cycle = run_review(ReviewRunInput {
            db: &db,
            context: &context,
            execution_id: "expansion",
            implementation_result: &ExecutionResult {
                agent_version: Some("fake".into()),
                model: Some("implementation-model".into()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cached_tokens: Some(0),
                exit_code: Some(0),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            },
            implementation_finalization: &finalization,
            implementation_agent: &agent,
            reviewer_agent: &agent,
            config: &config,
            paths: &paths,
            base_revision: &baseline,
            scope_policy: &snapshot,
        })
        .unwrap();
        assert_eq!(cycle.state, ReviewCycleState::Completed);
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::CleanReview]);
        let evaluation = cycle.scope_evaluations.last().unwrap();
        let justified = evaluation
            .findings
            .iter()
            .find(|finding| finding.path == "docs-notes.md")
            .unwrap();
        assert_eq!(
            justified.decision,
            familiar_ai_review::ScopeDecision::JustifiedExpectedFileChange
        );
        assert_eq!(justified.rule_id, "prd_expected_file_expansion");
    }

    #[test]
    fn undeclared_file_stops_scope_broadened_before_any_review() {
        let (_temp, db, context, config, paths, baseline, agent, finalization, snapshot) =
            production_review_fixture(false);
        fs::write(context.repository.worktree.join("rogue.md"), "rogue\n").unwrap();
        let error = run_review(ReviewRunInput {
            db: &db,
            context: &context,
            execution_id: "rogue",
            implementation_result: &ExecutionResult {
                agent_version: Some("fake".into()),
                model: Some("implementation-model".into()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cached_tokens: Some(0),
                exit_code: Some(0),
                signal: None,
                session_id: None,
                reported_cost_microusd: None,
            },
            implementation_finalization: &finalization,
            implementation_agent: &agent,
            reviewer_agent: &agent,
            config: &config,
            paths: &paths,
            base_revision: &baseline,
            scope_policy: &snapshot,
        });
        let cycle = error.unwrap();
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::ScopeBroadened]);
        assert_eq!(*agent.reviews.lock().unwrap(), 0);
        assert_eq!(review_retained_reason(&cycle), "scope_broadened");
        let finding = cycle.scope_evaluations[0]
            .findings
            .iter()
            .find(|finding| finding.path == "rogue.md")
            .unwrap();
        assert_eq!(
            finding.decision,
            familiar_ai_review::ScopeDecision::UndeclaredScopeExpansion
        );
        assert_eq!(finding.rule_id, "undeclared_change");
        let stored = ReviewRepository::new(db.conn())
            .get_cycle("rogue-cycle")
            .unwrap()
            .unwrap();
        assert_eq!(stored.scope_evaluations, cycle.scope_evaluations);
        assert!(stored.scope_policy_snapshot.is_some());
    }

    #[test]
    fn missing_expected_files_uses_configured_scope_only_when_mode_allows_it() {
        let (_temp, _db, context, config, paths, _baseline, _agent, _finalization, _snapshot) =
            production_review_fixture(false);
        let prd_file = context.repository.worktree.join("docs-prd.md");
        fs::write(
            &prd_file,
            "## Objective\nobjective\n\n## Acceptance Criteria\n1. criterion\n",
        )
        .unwrap();

        let preflight = compute_review_preflight(
            &config,
            &prd_file,
            "docs/prds/test.md",
            &context.repository.worktree,
            &paths.data_dir,
            "preflight-configured-scope",
        )
        .unwrap()
        .unwrap();
        assert!(preflight.snapshot.contract.is_empty());
        assert!(!preflight.snapshot.allowed_paths.is_empty());

        let mut required = config;
        required.review.scope.declaration_mode = ScopeDeclarationModeConfig::ExpectedRequired;
        let error = compute_review_preflight(
            &required,
            &prd_file,
            "docs/prds/test.md",
            &context.repository.worktree,
            &paths.data_dir,
            "preflight-required-scope",
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("no authoritative `## Expected Files`"));
    }

    #[test]
    fn invalid_expected_files_fails_preflight_before_claim_and_agent() {
        let (_temp, db, context, config, paths, _baseline, _agent, _finalization, _snapshot) =
            production_review_fixture(false);
        let prd_file = context.repository.worktree.join("docs-prd.md");
        fs::write(
            &prd_file,
            "## Objective\nobjective\n\n## Acceptance Criteria\n1. criterion\n\n## Expected Files\n\n- no code span bullet\n",
        )
        .unwrap();

        let error = compute_review_preflight(
            &config,
            &prd_file,
            "docs/prds/test.md",
            &context.repository.worktree,
            &paths.data_dir,
            "preflight",
        )
        .unwrap_err();
        match &error {
            RunError::Config(message) => {
                assert!(message.contains("Expected Files"), "got: {message}");
                assert!(message.contains("line 9"), "got: {message}");
            }
            other => panic!("expected config error, got {other:?}"),
        }
        // Preflight has no storage access: the backlog is untouched, so the
        // PRD necessarily remains pending and no agent was launched.
        let backlog: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM backlog_prds", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backlog, 0);

        let mut disabled = config.clone();
        disabled.review.enabled = false;
        assert!(compute_review_preflight(
            &disabled,
            &prd_file,
            "docs/prds/test.md",
            &context.repository.worktree,
            &paths.data_dir,
            "preflight-disabled",
        )
        .unwrap()
        .is_none());
    }

    struct UnavailableAgent;

    impl CodingAgent for UnavailableAgent {
        fn preflight(&self) -> Result<(), String> {
            Err("fixture executable missing".into())
        }

        fn execute(
            &self,
            _request: ExecutionRequest<'_>,
            _output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            panic!("preflight failure must prevent execution")
        }
    }

    #[test]
    fn unavailable_implementation_agent_fails_deterministically() {
        let temp = tempfile::tempdir().unwrap();
        let agent = UnavailableAgent;
        let agents = AgentSet {
            implementation: &agent,
            reviewer: &agent,
        };
        let error = preflight_execution_prerequisites(&agents, &Config::default(), temp.path())
            .unwrap_err();
        assert_eq!(error, "agent.implementation: fixture executable missing");
    }

    #[test]
    fn acceptance_criteria_accepts_case_and_qualified_spectra_heading() {
        let document = "# PRD\n\n## Measurable acceptance criteria\n\n- First criterion wraps\n  onto another line.\n- Second criterion.\n\n## Tests\nignored\n";
        assert_eq!(
            acceptance_criteria(document),
            vec![
                "First criterion wraps onto another line.".to_owned(),
                "Second criterion.".to_owned(),
            ]
        );

        assert_eq!(
            acceptance_criteria("## Acceptance criteria\n1. Numbered criterion\n"),
            vec!["Numbered criterion".to_owned()]
        );
    }
}
