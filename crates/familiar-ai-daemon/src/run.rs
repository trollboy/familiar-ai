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
    builtin_adapter_factories, AgentExecutionError, CodingAgent, ExecutionBudget, ExecutionRequest,
    ExecutionResult, RouteRequest, RouteRule, SelectionRecord, WorkerCapability, WorkerDescriptor,
    WorkerRegistry, WorkerStage,
};
use familiar_ai_context::{
    render_stable_prefix, ContextBudget, ContextBudgetError, ContextBudgeter,
    ContextCompilationError, ContextCompiler, ContextProfile, ContextReferenceKind,
    ContextReferenceRoot, ContextRequest, ExecutionContext,
};
use familiar_ai_core::config::WorkerCapabilityConfig;
use familiar_ai_core::{
    admit_run_prd, resolve_run_prd, structured_prd_metadata, validate_graph, AgentEntryConfig,
    AppPaths, BacklogDiscovery, BacklogStatusStore, Config, ExecutionPrice,
    FilesystemBacklogDiscovery, ScopeClassPolicyConfig, ScopeDeclarationModeConfig,
    ScopeFileClassName,
};
use familiar_ai_review::{
    compile_scope_policy, content_hash, normalize_scope_path, parse_expected_files,
    AgentAssignment, AgentObservation, AgentRole, BlockingPolicy, BoundedDocument,
    CodingRemediationAdapter, CommandVerificationRunner, CoordinationRequest, ExpectedFilesError,
    GitChangeKind, GitEvidenceCollector, ProhibitedRule, ProhibitedRuleKind, ReviewCoordinator,
    ReviewCycle, ReviewCycleState, ReviewDisposition, ReviewPackageBudget, ReviewStopReason,
    ReviewTask, ReviewTier, ReviewTierPolicy, ReviewTierRule, ScopeClassPolicy,
    ScopeClassificationRule, ScopeDeclarationMode, ScopeFileClass, ScopeFileClassPolicies,
    ScopePathEntry, ScopePolicyInput, ScopePolicySnapshot, ScopeRuleSource,
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
    HumanReviewRequired {
        result: Box<ExecutionResult>,
        cycle: Box<ReviewCycle>,
        prd_id: String,
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
            Self::HumanReviewRequired { .. } => f.write_str("human review required"),
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
            Self::HumanReviewRequired { result, .. } => result.exit_code.filter(|code| *code != 0),
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
    pub remediation: &'a dyn CodingAgent,
}

pub fn resolved_remediation_entry(config: &Config) -> Result<AgentEntryConfig, String> {
    let Some(registry) = &config.worker_registry else {
        return resolved_agent_entries(config).map(|entries| entries.0);
    };
    let (_, _, records) = resolved_worker_plan(config, &RouteContext::default())?;
    let selected = records
        .iter()
        .find(|record| record.stage == WorkerStage::Remediation)
        .ok_or_else(|| "remediation worker was not selected".to_owned())?;
    Ok(registry.workers[&selected.selected_worker].as_agent_entry())
}

/// Resolve the configured agent entries: validated when `[agents]` is
/// present (including review-identity consistency), exact historical Codex
/// defaults when absent.
pub fn resolved_agent_entries(
    config: &Config,
) -> Result<(AgentEntryConfig, AgentEntryConfig), String> {
    if config.worker_registry.is_some() {
        let plan = resolved_worker_plan(config, &RouteContext::default())?;
        return Ok((plan.0, plan.1));
    }
    match &config.agents {
        Some(agents) => {
            agents.validate(&config.review)?;
            Ok((agents.implementation.clone(), agents.reviewer.clone()))
        }
        None => Ok((AgentEntryConfig::default(), AgentEntryConfig::default())),
    }
}

/// Per-task inputs to route-rule matching: the target PRD's declared risk
/// classes and expected file count. Default (empty/zero) before a specific
/// PRD is bound, e.g. composition-root agent resolution ahead of execution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteContext {
    pub risk_classes: Vec<String>,
    pub expected_file_count: u64,
}

/// Derive routing inputs from the PRD that is about to be executed: its
/// declared `risk_classes` and expected file count. PRDs without a
/// structured front-matter contract declare neither, matching prior
/// (unrouted) behavior.
fn route_context_for_prd(prd_path: &Path) -> Result<RouteContext, RunError> {
    let prd_bytes = std::fs::read_to_string(prd_path)
        .map_err(|error| RunError::Config(format!("cannot read PRD for routing: {error}")))?;
    let metadata =
        structured_prd_metadata(&prd_bytes).map_err(|error| RunError::Config(error.to_string()))?;
    Ok(match metadata {
        Some(metadata) => RouteContext {
            expected_file_count: metadata.expected_files.len() as u64,
            risk_classes: metadata.risk_classes,
        },
        None => RouteContext::default(),
    })
}

/// Resolve every stage before execution. Selection is pure: it neither probes
/// executables nor invokes an adapter. Preflight performs those probes next.
pub fn resolved_worker_plan(
    config: &Config,
    route_context: &RouteContext,
) -> Result<(AgentEntryConfig, AgentEntryConfig, Vec<SelectionRecord>), String> {
    let configured = config
        .worker_registry
        .as_ref()
        .ok_or_else(|| "worker registry is not configured".to_owned())?;
    let risk_vocabulary: std::collections::BTreeSet<&str> = config
        .repositories
        .values()
        .flat_map(|entry| entry.risk_vocabulary.iter().map(String::as_str))
        .collect();
    configured.validate(&risk_vocabulary)?;
    let mut registry = WorkerRegistry::default();
    for (id, worker) in &configured.workers {
        let capabilities = worker
            .capabilities
            .iter()
            .map(|capability| match capability {
                WorkerCapabilityConfig::Planning => WorkerCapability::Planning,
                WorkerCapabilityConfig::Implementation => WorkerCapability::Implementation,
                WorkerCapabilityConfig::Review => WorkerCapability::Review,
                WorkerCapabilityConfig::Remediation => WorkerCapability::Remediation,
                WorkerCapabilityConfig::NarrowTask => WorkerCapability::NarrowTask,
            })
            .collect();
        registry.register(worker_descriptor(id, worker, capabilities))?;
    }
    let routing = &configured.routing;
    for rule in &routing.rules {
        registry.add_rule(RouteRule {
            id: rule.id.clone(),
            worker: rule.worker.clone(),
            risk_classes: rule.risk_classes.iter().cloned().collect(),
            max_expected_files: rule.max_expected_files,
        })?;
    }
    let select = |stage, pin: &Option<String>, independent_from| {
        registry
            .select(&RouteRequest {
                stage,
                pinned_worker: pin.clone(),
                max_cost_microusd: routing.max_stage_cost_microusd,
                required_context_tokens: routing.required_context_tokens,
                require_isolation: stage == WorkerStage::Review,
                independent_from,
                risk_classes: route_context.risk_classes.clone(),
                expected_file_count: route_context.expected_file_count,
            })
            .map_err(|e| e.to_string())
    };
    let implementation = select(
        WorkerStage::Implementation,
        &routing.implementation_pin,
        None,
    )?;
    let implementation_worker = registry.get(&implementation.selected_worker).unwrap();
    let mut records = vec![implementation.clone()];
    records.push(select(
        WorkerStage::Remediation,
        &routing.remediation_pin,
        None,
    )?);
    let review = if config.review.enabled {
        Some(select(
            WorkerStage::Review,
            &routing.review_pin,
            Some((
                implementation_worker.provider.clone(),
                implementation_worker.model.clone(),
            )),
        )?)
    } else {
        None
    };
    if let Some(record) = &review {
        records.push(record.clone());
    }
    let reviewer_id = review
        .as_ref()
        .map(|r| r.selected_worker.as_str())
        .unwrap_or(&implementation.selected_worker);
    let implementation_entry = configured.workers[&implementation.selected_worker].as_agent_entry();
    let reviewer_entry = configured.workers[reviewer_id].as_agent_entry();
    Ok((implementation_entry, reviewer_entry, records))
}

/// Deterministic constructor: adapter enum to concrete agent, nothing else.
/// Performs no probing, filesystem checks, or model calls.
pub fn build_agent(entry: &AgentEntryConfig) -> Box<dyn CodingAgent> {
    let descriptor = WorkerDescriptor {
        id: "legacy".into(),
        adapter_id: entry.adapter.as_str().into(),
        provider: entry.adapter.as_str().into(),
        model: entry.model.clone().unwrap_or_default(),
        executable: entry.resolved_executable(),
        capabilities: Default::default(),
        fresh_process_isolation: true,
        context_tokens: 0,
        estimated_cost_microusd: 0,
        available: true,
        effort: entry.effort.map(|value| value.as_str().into()),
        permission_mode: entry.permission_mode.map(|value| value.as_str().into()),
        extra_args: entry.extra_args.clone(),
    };
    builtin_adapter_factories()
        .build(&descriptor)
        .expect("built-in adapter factory must be registered")
}

fn worker_descriptor(
    id: &str,
    worker: &familiar_ai_core::config::RegistryWorkerConfig,
    capabilities: std::collections::BTreeSet<WorkerCapability>,
) -> WorkerDescriptor {
    WorkerDescriptor {
        id: id.into(),
        adapter_id: worker.adapter.as_str().into(),
        provider: worker.provider.clone(),
        model: worker.as_agent_entry().model.unwrap_or_default(),
        executable: worker
            .executable
            .clone()
            .unwrap_or_else(|| worker.adapter.default_executable().into()),
        capabilities,
        fresh_process_isolation: worker.fresh_process_isolation,
        context_tokens: worker.context_tokens,
        estimated_cost_microusd: worker.estimated_cost_microusd,
        available: worker.available,
        effort: worker.effort.map(|value| value.as_str().into()),
        permission_mode: worker.permission_mode.map(|value| value.as_str().into()),
        extra_args: worker.extra_args.clone(),
    }
}

type OwnedAgentSet = (
    Box<dyn CodingAgent>,
    Box<dyn CodingAgent>,
    Box<dyn CodingAgent>,
);

fn build_selected_agents(
    config: &Config,
    route_context: &RouteContext,
) -> Result<Option<OwnedAgentSet>, RunError> {
    let Some(registry) = &config.worker_registry else {
        return Ok(None);
    };
    let (_, _, records) = resolved_worker_plan(config, route_context).map_err(RunError::Config)?;
    let build_stage = |stage| -> Result<Box<dyn CodingAgent>, RunError> {
        let record = records
            .iter()
            .find(|record| record.stage == stage)
            .ok_or_else(|| RunError::Config(format!("{stage:?} worker was not selected")))?;
        let worker = &registry.workers[&record.selected_worker];
        let capabilities = worker
            .capabilities
            .iter()
            .map(|capability| match capability {
                WorkerCapabilityConfig::Planning => WorkerCapability::Planning,
                WorkerCapabilityConfig::Implementation => WorkerCapability::Implementation,
                WorkerCapabilityConfig::Review => WorkerCapability::Review,
                WorkerCapabilityConfig::Remediation => WorkerCapability::Remediation,
                WorkerCapabilityConfig::NarrowTask => WorkerCapability::NarrowTask,
            })
            .collect();
        builtin_adapter_factories()
            .build(&worker_descriptor(
                &record.selected_worker,
                worker,
                capabilities,
            ))
            .map_err(RunError::Config)
    };
    let implementation = build_stage(WorkerStage::Implementation)?;
    let remediation = build_stage(WorkerStage::Remediation)?;
    let reviewer = if config.review.enabled {
        build_stage(WorkerStage::Review)?
    } else {
        build_stage(WorkerStage::Implementation)?
    };
    Ok(Some((implementation, reviewer, remediation)))
}

fn borrowed_agent_set(owned: &OwnedAgentSet) -> AgentSet<'_> {
    AgentSet {
        implementation: owned.0.as_ref(),
        reviewer: owned.1.as_ref(),
        remediation: owned.2.as_ref(),
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
    let current = match env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return (
                Err(RunError::CurrentDirectory(error)),
                AttemptTrace::default(),
            )
        }
    };
    execute_with_config_tracked_from(&current, prd_path, agents, config, paths)
}

/// Repository-explicit execution used by isolated worktree workers. Unlike
/// the CLI wrapper, this never reads or mutates process-wide current_dir.
pub fn execute_with_config_tracked_from(
    current: &Path,
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
) -> (Result<RunWorkflowResult, RunError>, AttemptTrace) {
    execute_with_config_tracked_from_preflighted(current, prd_path, agents, config, paths, false)
}

/// Driver-only entry point after an identical session-level prerequisite
/// report has passed. Context and admission remain per-attempt and fresh.
pub fn execute_with_config_tracked_from_preflighted(
    current: &Path,
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    prerequisites_preflighted: bool,
) -> (Result<RunWorkflowResult, RunError>, AttemptTrace) {
    execute_with_config_tracked_from_preflighted_with_route_context(
        current,
        prd_path,
        agents,
        config,
        paths,
        prerequisites_preflighted,
        None,
    )
}

/// Driver-only entry point that preserves the routing inputs derived during
/// backlog discovery, including legacy document expected-file declarations.
pub fn execute_with_config_tracked_from_preflighted_with_route_context(
    current: &Path,
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    prerequisites_preflighted: bool,
    route_context: Option<RouteContext>,
) -> (Result<RunWorkflowResult, RunError>, AttemptTrace) {
    let mut trace = AttemptTrace::default();
    let result = execute_tracked_inner(
        current,
        prd_path,
        agents,
        config,
        paths,
        prerequisites_preflighted,
        route_context,
        &mut trace,
    );
    (result, trace)
}

/// Continue a validated implementation checkpoint at review. The implementation
/// adapter is never invoked on this path.
pub fn resume_implemented_checkpoint(
    current: &Path,
    prd_id: &str,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
) -> Result<RunWorkflowResult, RunError> {
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let database_path = config.database.resolve_path(&paths.data_dir);
    let mut db = Database::open(&database_path).map_err(|e| RunError::Storage(e.to_string()))?;
    db.run_migrations()
        .map_err(|e| RunError::Storage(e.to_string()))?;
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, prd_id)
        .map_err(|e| RunError::Storage(e.to_string()))?
        .ok_or_else(|| RunError::Config(format!("no durable checkpoint for {prd_id}")))?;
    let candidate = crate::resume::one(&db, &repository.key, prd_id).map_err(RunError::Config)?;
    if !candidate.valid {
        return Err(RunError::Config(
            candidate
                .reason
                .unwrap_or_else(|| "invalid checkpoint".into()),
        ));
    }
    if !matches!(
        checkpoint.phase.as_str(),
        "implemented" | "implemented_pending_review" | "blocked"
    ) {
        return Err(RunError::Config(format!(
            "checkpoint phase {} cannot start review",
            checkpoint.phase
        )));
    }
    let repository_config = config.repository(&repository.worktree);
    let discovered = discovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| RunError::Config(e.to_string()))?;
    validate_graph(&discovered).map_err(|e| RunError::Config(e.to_string()))?;
    let target = discovered
        .into_iter()
        .find(|p| p.id.to_string() == prd_id)
        .ok_or_else(|| {
            RunError::Config(format!("checkpoint PRD {prd_id} is no longer discoverable"))
        })?;
    let prd_path = candidate.worktree.join(target.path.as_str());
    let profile = context_profile(&repository_config);
    let context = ContextCompiler::new()
        .compile_profiled(
            ContextRequest {
                repository: &candidate.worktree,
                prd: &prd_path,
            },
            &profile,
        )
        .map_err(RunError::Context)?;
    let execution_id = checkpoint
        .execution_id
        .as_deref()
        .ok_or_else(|| RunError::Config("checkpoint execution identity is unknown".into()))?;
    let record = ExecutionHistoryRepository::new(db.conn())
        .get(execution_id)
        .map_err(|e| RunError::Storage(e.to_string()))?
        .ok_or_else(|| {
            RunError::Config(format!("checkpoint execution {execution_id} is missing"))
        })?;
    let result = ExecutionResult {
        agent_version: record.agent_version.clone(),
        model: record.model.clone(),
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        exit_code: record.exit_code,
        signal: record.signal,
        session_id: None,
        reported_cost_microusd: None,
    };
    let finalization = ExecutionFinalization {
        ended_at: record.ended_at.unwrap_or_default(),
        duration_ms: record.duration_ms.unwrap_or_default(),
        agent_version: record.agent_version,
        model: record.model,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        total_tokens: record.total_tokens,
        estimated_cost_microusd: record.estimated_cost_microusd,
        input_rate: record.input_rate,
        cached_input_rate: record.cached_input_rate,
        output_rate: record.output_rate,
        outcome: record.outcome,
        exit_code: record.exit_code,
        signal: record.signal,
        unavailable_fields: record.unavailable_fields,
    };
    let mut preflight = compute_review_preflight(
        config,
        &prd_path,
        target.path.as_str(),
        &candidate.worktree,
        &paths.data_dir,
        execution_id,
    )?
    .ok_or_else(|| RunError::Config("review is disabled".into()))?;
    preflight.baseline = checkpoint.base_revision.clone();
    preflight.snapshot = build_scope_policy(
        config,
        target.path.as_str(),
        preflight.snapshot.contract.clone(),
        content_hash(preflight.prd_bytes.as_bytes()),
        &checkpoint.base_revision,
    )?;
    // Completion is owner-gated: the latest backlog event must carry the
    // claiming actor, so a resume acts under the original run's identity
    // rather than minting its own.
    let actor = format!("system:familiar-ai-run:{execution_id}");
    let mut trace = AttemptTrace {
        execution_id: Some(execution_id.into()),
        retained_reason: None,
    };
    let route_context = route_context_for_prd(&prd_path)?;
    let owned_agents = build_selected_agents(config, &route_context)?;
    let selected_agents = owned_agents.as_ref().map(borrowed_agent_set);
    let agents = selected_agents.as_ref().unwrap_or(agents);
    let completed = finish_implementation(
        &mut db,
        &repository,
        &target,
        execution_id,
        &actor,
        result,
        &finalization,
        Some(&preflight),
        &context,
        agents,
        config,
        paths,
        &mut trace,
    )?;
    let checkpoints = familiar_ai_storage::CheckpointRepository::new(db.conn());
    for (phase, detail) in [
        ("verified", "required_verification_passed"),
        ("reviewed", "independent_review_clean"),
        ("approved", "review_disposition_ready"),
        ("integrated", "backlog_completion_committed"),
        ("completed", "resume_completed"),
    ] {
        checkpoints
            .transition(&checkpoint.checkpoint_id, phase, detail)
            .map_err(|e| RunError::Storage(e.to_string()))?;
    }
    Ok(completed)
}

/// Record an attached human's explicit acceptance of the exact retained
/// review evidence, then complete the already-implemented checkpoint. This is
/// never called by the unattended driver.
pub fn accept_review_risk(
    current: &Path,
    prd_id: &str,
    actor: &str,
    cycle: &ReviewCycle,
    config: &Config,
    paths: &AppPaths,
) -> Result<(), RunError> {
    if !actor.trim().starts_with("human:") {
        return Err(RunError::Config(
            "risk acceptance requires a human:<identity> actor".into(),
        ));
    }
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let repository_config = config.repository(&repository.worktree);
    let discovered = discovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| RunError::Config(e.to_string()))?;
    let target = discovered
        .into_iter()
        .find(|target| target.id.to_string() == prd_id)
        .ok_or_else(|| {
            RunError::Config(format!("checkpoint PRD {prd_id} is no longer discoverable"))
        })?;
    let mut db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| RunError::Storage(e.to_string()))?;
    db.run_migrations()
        .map_err(|e| RunError::Storage(e.to_string()))?;
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, prd_id)
        .map_err(|e| RunError::Storage(e.to_string()))?
        .ok_or_else(|| RunError::Config(format!("no durable checkpoint for {prd_id}")))?;
    if checkpoint.phase != "blocked" {
        return Err(RunError::Config(
            "risk acceptance requires a blocked human-review checkpoint".into(),
        ));
    }
    let execution_id = checkpoint
        .execution_id
        .as_deref()
        .ok_or_else(|| RunError::Config("checkpoint execution identity is unknown".into()))?;
    let findings = serde_json::json!({"review": cycle.review_result.as_ref().map(|r| &r.findings), "scope": cycle.scope_evaluations.iter().flat_map(|e| &e.findings).collect::<Vec<_>>()}).to_string();
    let stops =
        serde_json::to_string(&cycle.stop_reasons).map_err(|e| RunError::Storage(e.to_string()))?;
    familiar_ai_storage::DeliveryRepository::new(db.conn())
        .record_authority_decision(
            &format!("review-risk:{execution_id}"),
            &repository.key,
            execution_id,
            prd_id,
            "attached_human",
            actor,
            "accepted_reviewed_risk",
            None,
            &findings,
            &stops,
            None,
            0,
        )
        .map_err(|e| RunError::Storage(e.to_string()))?;
    SqliteBacklogRepository::new(db.conn_mut())
        .recover(
            &repository,
            &target,
            familiar_ai_core::BacklogRecoveryAction::ManualCompleteOverride,
            actor,
            "accepted the exact persisted HumanReviewRequired findings and stop reasons",
        )
        .map_err(|e| RunError::Workflow {
            result: None,
            detail: e.to_string(),
        })?;
    let checkpoints = familiar_ai_storage::CheckpointRepository::new(db.conn());
    for (phase, detail) in [
        ("approved", "attached_human_accepted_reviewed_risk"),
        ("integrated", "backlog_completion_committed"),
        ("completed", "risk_acceptance_completed"),
    ] {
        checkpoints
            .transition(&checkpoint.checkpoint_id, phase, detail)
            .map_err(|e| RunError::Storage(e.to_string()))?;
    }
    Ok(())
}

/// What the driver observes about one attempt beyond success or failure.
#[derive(Debug, Default, Clone)]
pub struct AttemptTrace {
    pub execution_id: Option<String>,
    pub retained_reason: Option<&'static str>,
}

fn execute_tracked_inner(
    current: &Path,
    prd_path: &Path,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    prerequisites_preflighted: bool,
    route_context: Option<RouteContext>,
    trace: &mut AttemptTrace,
) -> Result<RunWorkflowResult, RunError> {
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let route_context = match route_context {
        Some(route_context) => route_context,
        None => route_context_for_prd(prd_path)?,
    };
    let repository_config = config.repository(&repository.worktree);
    let effective = config.effective_execution(&repository.worktree);
    let mut effective_config = config.clone();
    effective_config.review = effective.review;
    effective_config.execution_context = effective.execution_context;
    if let Some(registry) = &effective_config.worker_registry {
        let (_, _, records) =
            resolved_worker_plan(&effective_config, &route_context).map_err(RunError::Config)?;
        for (stage, identity) in [
            (
                WorkerStage::Implementation,
                &mut effective_config.review.implementation_agent,
            ),
            (
                WorkerStage::Review,
                &mut effective_config.review.reviewer_agent,
            ),
        ] {
            if let Some(record) = records.iter().find(|record| record.stage == stage) {
                let worker = &registry.workers[&record.selected_worker];
                identity.adapter_id = worker.adapter.as_str().into();
                identity.agent_id = record.selected_worker.clone();
                identity.provider = Some(worker.provider.clone());
                identity.model = Some(worker.as_agent_entry().model.unwrap_or_default());
            }
        }
    }
    let config = &effective_config;
    // Registry selection owns construction of the executors it selected. This
    // prevents library callers from supplying a different trait object than
    // the one that passed routing and preflight.
    let owned_agents = build_selected_agents(config, &route_context)?;
    let selected_agents = owned_agents.as_ref().map(borrowed_agent_set);
    let agents = selected_agents.as_ref().unwrap_or(agents);
    let profile = context_profile(&repository_config);
    let context = ContextCompiler::new()
        .compile_profiled(
            ContextRequest {
                repository: current,
                prd: prd_path,
            },
            &profile,
        )
        .map_err(RunError::Context)?;
    let context = match config.execution_context.hard_ceiling_tokens {
        Some(hard_ceiling_tokens) => {
            if hard_ceiling_tokens < context.prd.estimated_tokens {
                return Err(RunError::ContextBudget(
                    ContextBudgetError::PrdExceedsHardCeiling {
                        path: context.prd.path.clone(),
                        estimated_tokens: context.prd.estimated_tokens,
                        hard_ceiling_tokens,
                    },
                ));
            }
            let complete = render_prompt(&context, &profile);
            let complete_tokens = u64::try_from(familiar_ai_tokens::estimate_tokens(&complete))
                .map_err(|_| RunError::Config("complete prompt token estimate overflow".into()))?;
            let framing_tokens = complete_tokens.saturating_sub(context.estimated_tokens);
            let document_ceiling = hard_ceiling_tokens.checked_sub(framing_tokens).ok_or_else(|| {
                RunError::Config(format!("stable prompt framing requires {framing_tokens} estimated tokens, exceeding hard ceiling {hard_ceiling_tokens}"))
            })?;
            ContextBudgeter::new()
                .budget(
                    context,
                    ContextBudget {
                        hard_ceiling_tokens: document_ceiling,
                    },
                )
                .map_err(RunError::ContextBudget)?
                .context
        }
        None => context,
    };
    let stable_prefix = render_stable_prefix(&context, &profile, EXECUTION_CONSTRAINTS);
    let prompt_cache_key = familiar_ai_review::content_hash(stable_prefix.as_bytes());
    let prompt = render_prompt_with_prefix(&context, &stable_prefix);
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
    let implementation_entry = resolved_agent_entries(config).map_err(RunError::Config)?.0;
    if config.worker_registry.is_some() {
        let (_, _, records) =
            resolved_worker_plan(config, &route_context).map_err(RunError::Config)?;
        let selections = familiar_ai_storage::WorkerSelectionRepository::new(db.conn());
        for (index, record) in records.iter().enumerate() {
            let candidates = record.candidates.iter().map(|candidate| serde_json::json!({
                "worker_id": candidate.worker_id,
                "rejected_reasons": candidate.rejected.iter().map(|reason| format!("{reason:?}")).collect::<Vec<_>>()
            })).collect::<Vec<_>>();
            let selection_id = format!("{id}:worker:{index}");
            let stage = format!("{:?}", record.stage).to_ascii_lowercase();
            let candidates_json =
                serde_json::to_string(&candidates).map_err(|e| RunError::Storage(e.to_string()))?;
            let risk_classes_json = serde_json::to_string(&route_context.risk_classes)
                .map_err(|e| RunError::Storage(e.to_string()))?;
            selections
                .record(
                    &familiar_ai_storage::repos::worker_selection::WorkerSelectionRecord {
                        selection_id: &selection_id,
                        execution_id: Some(&id),
                        stage: &stage,
                        rule: &record.rule,
                        selected_identity: &record.selected_worker,
                        candidates_json: &candidates_json,
                        risk_classes_json: &risk_classes_json,
                        expected_file_count: route_context.expected_file_count,
                    },
                )
                .map_err(|e| RunError::Storage(e.to_string()))?;
        }
    }
    let execution_budget = ExecutionBudget {
        max_cost_microusd: std::num::NonZeroU64::new(
            implementation_entry.max_execution_cost_microusd,
        ),
        max_tokens: std::num::NonZeroU64::new(implementation_entry.max_execution_tokens),
        max_duration_ms: std::num::NonZeroU64::new(implementation_entry.max_execution_duration_ms),
    };
    let capability = agents.implementation.budget_capability();
    if capability.cost_always_zero
        && execution_budget.max_cost_microusd.is_some()
        && execution_budget.max_tokens.is_none()
        && execution_budget.max_duration_ms.is_none()
    {
        return Err(RunError::Config(format!(
            "adapter {} always reports zero cost; this warrant requires a tokens or duration ceiling to bind it",
            implementation_entry.adapter.as_str()
        )));
    }
    if let Some(denomination) = execution_budget
        .denominations()
        .find(|value| !capability.supports(*value))
    {
        return Err(RunError::Config(format!(
            "adapter {} cannot enforce a per-execution {denomination} ceiling",
            implementation_entry.adapter.as_str()
        )));
    }
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
    if !prerequisites_preflighted {
        preflight_execution_prerequisites(agents, config, &context.repository.worktree).map_err(
            |detail| retained_traced(trace, &target, "preflight_failed", RunError::Config(detail)),
        )?;
    }
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
            prompt_cache_key: config
                .execution_context
                .prompt_cache_enabled
                .then_some(prompt_cache_key.as_str()),
            filesystem: familiar_ai_agent::FilesystemPolicy::Normal,
            model: if config.worker_registry.is_some() || config.review.enabled {
                config.review.implementation_agent.model.as_deref()
            } else {
                implementation_entry.model.as_deref()
            },
            timeout_ms: None,
            budget: execution_budget,
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
        Err(AgentExecutionError::BudgetStopped { result }) => (result.as_ref(), "budget_stopped"),
        Err(AgentExecutionError::UnenforceableBudget { result, .. }) => {
            (result.as_ref(), "budget_refused")
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
    if result.exit_code == Some(0) && result.signal.is_none() {
        let total = result
            .input_tokens
            .zip(result.output_tokens)
            .map(|(input, output)| input.saturating_add(output));
        let pending_review = config.driver.max_implementation_tokens > 0
            && total.is_some_and(|tokens| tokens > config.driver.max_implementation_tokens);
        let usage = serde_json::json!({
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "cached_tokens": result.cached_tokens,
            "total_tokens": total,
            "estimated_cost_microusd": finalization.estimated_cost_microusd,
        });
        crate::resume::freeze_implementation(
            &db,
            &repository.key,
            &target.id.to_string(),
            target.path.as_str(),
            &id,
            &context.repository.worktree,
            implementation_entry.adapter.as_str(),
            usage.to_string(),
            pending_review,
        )
        .map_err(|detail| {
            retained_traced(
                trace,
                &target,
                "checkpoint_failed",
                RunError::Storage(detail),
            )
        })?;
    }
    if config.driver.max_implementation_tokens > 0 {
        let total = result
            .input_tokens
            .zip(result.output_tokens)
            .map(|(input, output)| input.saturating_add(output));
        match total {
            None => {
                return Err(retained_traced(
                    trace,
                    &target,
                    "implementation_token_usage_unknown",
                    RunError::Workflow {
                        result: Some(Box::new(result.clone())),
                        detail:
                            "implementation token usage is unknown under a token-bounded policy"
                                .into(),
                    },
                ));
            }
            Some(tokens) if tokens > config.driver.max_implementation_tokens => {
                return Err(retained_traced(
                    trace,
                    &target,
                    "implementation_token_budget_exceeded",
                    RunError::Workflow {
                        result: Some(Box::new(result.clone())),
                        detail: format!(
                            "implementation used {tokens} tokens, exceeding stage ceiling {}",
                            config.driver.max_implementation_tokens
                        ),
                    },
                ));
            }
            Some(_) => {}
        }
    }
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
    finish_implementation(
        &mut db,
        &repository,
        &target,
        &id,
        &actor,
        result,
        &finalization,
        review_preflight.as_ref(),
        &context,
        agents,
        config,
        paths,
        trace,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_implementation(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    target: &familiar_ai_core::DiscoveredPrd,
    id: &str,
    actor: &str,
    result: ExecutionResult,
    finalization: &ExecutionFinalization,
    review_preflight: Option<&ReviewPreflight>,
    context: &ExecutionContext,
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    trace: &mut AttemptTrace,
) -> Result<RunWorkflowResult, RunError> {
    if !config.review.enabled {
        return Err(retained_traced(
            trace,
            target,
            "review_disabled",
            RunError::Workflow {
                result: Some(Box::new(result)),
                detail: "review is disabled; backlog completion requires a clean review".into(),
            },
        ));
    }
    let preflight = review_preflight.expect("enabled review preflight");
    let cycle = run_review(ReviewRunInput {
        db,
        context,
        execution_id: id,
        implementation_result: &result,
        implementation_finalization: finalization,
        implementation_agent: agents.remediation,
        reviewer_agent: agents.reviewer,
        config,
        paths,
        base_revision: &preflight.baseline,
        scope_policy: &preflight.snapshot,
    })
    .map_err(|e| retained_traced(trace, target, "review_failed", e))?;
    if cycle.state != ReviewCycleState::Completed
        || cycle.disposition != ReviewDisposition::ReadyForHumanApproval
        || cycle.stop_reasons != [ReviewStopReason::CleanReview]
    {
        let reason = review_retained_reason(&cycle);
        trace.retained_reason = Some(reason);
        if let Some(checkpoint) = familiar_ai_storage::CheckpointRepository::new(db.conn())
            .get(&repository.key, &target.id.to_string())
            .map_err(|e| RunError::Storage(e.to_string()))?
        {
            let detail = serde_json::json!({"findings": cycle.review_result.as_ref().map(|r| &r.findings), "scope_findings": cycle.scope_evaluations.iter().flat_map(|e| &e.findings).collect::<Vec<_>>(), "stop_reasons": cycle.stop_reasons}).to_string();
            familiar_ai_storage::CheckpointRepository::new(db.conn())
                .transition(&checkpoint.checkpoint_id, "blocked", &detail)
                .map_err(|e| RunError::Storage(e.to_string()))?;
        }
        return Err(RunError::HumanReviewRequired {
            result: Box::new(result),
            cycle: Box::new(cycle),
            prd_id: target.id.to_string(),
        });
    }
    let required_checks = config
        .review
        .verification
        .iter()
        .filter(|c| c.required)
        .map(|c| c.check_id.clone())
        .collect::<Vec<_>>();
    SqliteBacklogRepository::new(db.conn_mut())
        .complete_run(repository, target, id, actor, &required_checks)
        .map_err(|e| {
            retained_traced(
                trace,
                target,
                "completion_conflict",
                RunError::Workflow {
                    result: Some(Box::new(result.clone())),
                    detail: e.to_string(),
                },
            )
        })?;
    if let Some(checkpoint) = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, &target.id.to_string())
        .map_err(|e| RunError::Storage(e.to_string()))?
    {
        let checkpoints = familiar_ai_storage::CheckpointRepository::new(db.conn());
        for (phase, detail) in [
            ("verified", "required_verification_passed"),
            ("reviewed", "independent_review_clean"),
            ("approved", "review_disposition_ready"),
            ("integrated", "backlog_completion_committed"),
            ("completed", "execution_completed"),
        ] {
            checkpoints
                .transition(&checkpoint.checkpoint_id, phase, detail)
                .map_err(|e| RunError::Storage(e.to_string()))?;
        }
    }
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
    let structured_metadata = familiar_ai_core::structured_prd_metadata(&prd_bytes)
        .map_err(|error| RunError::Config(error.to_string()))?;
    let scope_document = structured_metadata.as_ref().map(|metadata| {
        let bullets = metadata
            .expected_files
            .iter()
            .map(|path| format!("- `{path}`\n"))
            .collect::<String>();
        format!("## Expected Files\n\n{bullets}")
    });
    let contract = match parse_expected_files(scope_document.as_deref().unwrap_or(&prd_bytes)) {
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
        AgentExecutionError::BudgetStopped { .. } => "budget_stopped",
        AgentExecutionError::UnenforceableBudget { .. } => "budget_refused",
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
    let standard_reviewer = config.review.tier_policy.as_ref().and_then(|policy| {
        let configured = &policy.standard_reviewer_agent;
        (!configured.adapter_id.is_empty()).then(|| AgentAssignment {
            adapter_id: configured.adapter_id.clone(),
            agent_id: configured.agent_id.clone(),
            provider: configured.provider.clone(),
            requested_model: configured.model.clone(),
            role: AgentRole::Review,
            session_id: None,
        })
    });
    let criteria = familiar_ai_core::structured_prd_metadata(&context.prd.content)
        .map_err(|error| RunError::Config(error.to_string()))?
        .map(|metadata| metadata.acceptance_criteria)
        .unwrap_or_else(|| acceptance_criteria(&context.prd.content));
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
        standard_reviewer,
        tier_policy: configured_tier_policy(&config.review),
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
                .map(|_| {
                    if implementation_result.reported_cost_microusd.is_some() {
                        "vendor_reported".into()
                    } else {
                        "configured_rate".into()
                    }
                }),
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

fn configured_tier_policy(config: &familiar_ai_core::config::ReviewConfig) -> ReviewTierPolicy {
    let Some(policy) = &config.tier_policy else {
        return ReviewTierPolicy::default();
    };
    ReviewTierPolicy {
        rules: policy
            .rules
            .iter()
            .map(|rule| ReviewTierRule {
                id: rule.id.clone(),
                tier: match rule.tier {
                    familiar_ai_core::config::ReviewTierConfig::ChecksOnly => {
                        ReviewTier::ChecksOnly
                    }
                    familiar_ai_core::config::ReviewTierConfig::Standard => ReviewTier::Standard,
                    familiar_ai_core::config::ReviewTierConfig::Full => ReviewTier::Full,
                },
                path_prefixes: rule
                    .path_prefixes
                    .iter()
                    .map(|path| {
                        familiar_ai_core::config::validate_scope_path(path)
                            .expect("validated tier path")
                    })
                    .collect(),
                max_changed_files: rule.max_changed_files,
                max_changed_bytes: rule.max_changed_bytes,
                change_kinds: rule
                    .change_kinds
                    .iter()
                    .map(|kind| match kind.as_str() {
                        "added" => GitChangeKind::Added,
                        "modified" => GitChangeKind::Modified,
                        "deleted" => GitChangeKind::Deleted,
                        "renamed" => GitChangeKind::Renamed,
                        "copied" => GitChangeKind::Copied,
                        "type_changed" => GitChangeKind::TypeChanged,
                        "unmerged" => GitChangeKind::Unmerged,
                        _ => unreachable!("validated change kind"),
                    })
                    .collect(),
                scope_classes: rule
                    .scope_classes
                    .iter()
                    .map(|class| map_file_class(*class))
                    .collect(),
            })
            .collect(),
    }
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
    let (cost, rates, reason) = if let Some(vendor) = result.reported_cost_microusd {
        (Some(vendor), (None, None, None), "")
    } else {
        calculate_cost(
            result.input_tokens,
            result.cached_tokens,
            result.output_tokens,
            price,
        )
    };
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
    Ok(render_prompt(&context, &ContextProfile::default()))
}

fn render_prompt(context: &ExecutionContext, profile: &ContextProfile) -> String {
    let prefix = render_stable_prefix(context, profile, EXECUTION_CONSTRAINTS);
    render_prompt_with_prefix(context, &prefix)
}

fn render_prompt_with_prefix(context: &ExecutionContext, prefix: &str) -> String {
    let mut prompt = prefix.to_owned();
    prompt.push_str("## PRD: ");
    prompt.push_str(&context.prd.path);
    prompt.push_str("\n\n");
    prompt.push_str(&context.prd.content);
    prompt.push('\n');
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
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeAgent {
        request: Mutex<Option<(PathBuf, String)>>,
        result: ExecutionResult,
    }

    fn execution_fixture_repository(root: &Path) -> PathBuf {
        let repository = root.join("repository");
        fs::create_dir_all(repository.join("docs/prds")).unwrap();
        fs::create_dir_all(repository.join("docs/contracts")).unwrap();
        fs::write(
            repository.join("docs/prds/PRD-001.md"),
            "# PRD-001: execution fixture\n\n**Status:** Ready for implementation\n\n## Acceptance Criteria\n\n1. The fixture executes.\n\n## Expected Files\n\n- `src/fixture.rs`\n\n## Reference Context\n\n- `docs/contracts/input.md`\n",
        )
        .unwrap();
        fs::write(
            repository.join("docs/contracts/input.md"),
            "# Fixture contract\n\nAuthoritative fixture content.\n",
        )
        .unwrap();
        for arguments in [
            &["init", "-q"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ][..],
        ] {
            assert!(Command::new("git")
                .args(arguments)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success());
        }
        repository.canonicalize().unwrap()
    }

    fn execute_fixture(
        repository: &Path,
        prd: &Path,
        agents: &AgentSet<'_>,
        config: &Config,
        paths: &AppPaths,
    ) -> Result<RunWorkflowResult, RunError> {
        execute_with_config_tracked_from(repository, prd, agents, config, paths).0
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

    #[test]
    fn prompt_places_stable_context_before_volatile_prd() {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let prd = repository.join("docs/prds/done/PRD-003.md");
        let prompt = build_prompt(&repository, &prd).unwrap();
        let stable = prompt.find("## Stable repository context").unwrap();
        let reference = prompt.find("## Authoritative reference:").unwrap();
        let volatile = prompt.find("## Volatile execution data").unwrap();
        let prd = prompt.find("## PRD:").unwrap();
        assert!(stable < reference && reference < volatile && volatile < prd);
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
        assert!(render_prompt(&context, &ContextProfile::default())
            .contains("\n## Authoritative reference: docs/supporting/input.md\n\nsupport\n"));
    }

    #[test]
    fn orchestration_uses_neutral_agent_and_preserves_history_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let repository = execution_fixture_repository(temp.path());
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

        let error = execute_fixture(
            &repository,
            &repository.join("docs/prds/PRD-001.md"),
            &same_agent(&agent),
            &config,
            &paths,
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), Some(23), "{error:?}");
        let captured = agent.request.lock().unwrap();
        let (working_directory, prompt) = captured.as_ref().unwrap();
        assert_eq!(working_directory, &repository);
        assert_eq!(
            prompt,
            &build_prompt(&repository, &repository.join("docs/prds/PRD-001.md"),).unwrap()
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
        let repository = execution_fixture_repository(temp.path());
        let prd = repository.join("docs/prds/PRD-001.md");
        let expected = build_prompt(&repository, &prd).unwrap();
        let mut config = Config::default();
        config.execution_context.hard_ceiling_tokens = Some(u64::MAX);
        config.database.path = Some(temp.path().join("history.db"));
        let paths = test_paths(temp.path());
        let agent = successful_fake_agent();

        let error =
            execute_fixture(&repository, &prd, &same_agent(&agent), &config, &paths).unwrap_err();

        let captured = agent.request.lock().unwrap();
        assert_eq!(
            captured
                .as_ref()
                .unwrap_or_else(|| panic!("{error:?}"))
                .1
                .as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn selective_budget_renders_only_selected_whole_documents() {
        let temp = tempfile::tempdir().unwrap();
        let repository = execution_fixture_repository(temp.path());
        let prd = repository.join("docs/prds/PRD-001.md");
        let complete = ContextCompiler
            .compile(ContextRequest {
                repository: &repository,
                prd: &prd,
            })
            .unwrap();
        let complete_prompt = render_prompt(&complete, &ContextProfile::default());
        let framing = u64::try_from(familiar_ai_tokens::estimate_tokens(&complete_prompt)).unwrap()
            - complete.estimated_tokens;
        let document_ceiling = complete.prd.estimated_tokens;
        let ceiling = framing + document_ceiling;
        let expected = ContextBudgeter
            .budget(
                complete,
                ContextBudget {
                    hard_ceiling_tokens: document_ceiling,
                },
            )
            .unwrap();
        assert!(expected.context.documents.is_empty());
        assert!(expected.report.decisions.len() > 1);
        assert!(expected.report.excluded_estimated_tokens > 0);
        let expected_prompt = render_prompt(&expected.context, &ContextProfile::default());
        let mut config = Config::default();
        config.execution_context.hard_ceiling_tokens = Some(ceiling);
        config.database.path = Some(temp.path().join("history.db"));
        let paths = test_paths(temp.path());
        let agent = successful_fake_agent();

        execute_fixture(&repository, &prd, &same_agent(&agent), &config, &paths).unwrap_err();

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
            &repository.join("docs/prds/done/PRD-007.md"),
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
            remediation: agent,
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
            remediation: &agent,
        };
        let error = preflight_execution_prerequisites(&agents, &Config::default(), temp.path())
            .unwrap_err();
        assert_eq!(error, "agent.implementation: fixture executable missing");
    }

    struct RecoveryProbeAgent {
        panic_preflight_once: std::sync::atomic::AtomicBool,
        panic_execute_once: std::sync::atomic::AtomicBool,
        invocations: std::sync::atomic::AtomicUsize,
    }

    impl CodingAgent for RecoveryProbeAgent {
        fn preflight(&self) -> Result<(), String> {
            if self
                .panic_preflight_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                panic!("injected termination before model invocation");
            }
            Ok(())
        }

        fn execute(
            &self,
            _request: ExecutionRequest<'_>,
            _output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            self.invocations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .panic_execute_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                panic!("injected termination after model invocation");
            }
            Ok(ExecutionResult {
                exit_code: Some(0),
                ..ExecutionResult::default()
            })
        }
    }

    #[test]
    fn production_recovery_never_reinvokes_model_across_durable_claim() {
        for terminate_after_invocation in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let repository = execution_fixture_repository(temp.path());
            let prd = repository.join("docs/prds/PRD-001.md");
            let mut config = Config::default();
            config.database.path = Some(temp.path().join("recovery.db"));
            let paths = test_paths(temp.path());
            let agent = RecoveryProbeAgent {
                panic_preflight_once: std::sync::atomic::AtomicBool::new(
                    !terminate_after_invocation,
                ),
                panic_execute_once: std::sync::atomic::AtomicBool::new(terminate_after_invocation),
                invocations: std::sync::atomic::AtomicUsize::new(0),
            };

            let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_with_config_tracked_from(
                    &repository,
                    &prd,
                    &same_agent(&agent),
                    &config,
                    &paths,
                )
            }));
            assert!(first.is_err());

            // This is a fresh production entry using only the persisted
            // database/backlog state left by the interrupted process.
            let _ = execute_with_config_tracked_from(
                &repository,
                &prd,
                &same_agent(&agent),
                &config,
                &paths,
            );
            assert_eq!(
                agent.invocations.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "restart must invoke once before a claim and never repeat an uncertain invocation"
            );
        }
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
