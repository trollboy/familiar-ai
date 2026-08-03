use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::Utc;
use familiar_agent::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};
use familiar_context::{
    ContextBudget, ContextBudgetError, ContextBudgeter, ContextCompilationError, ContextCompiler,
    ContextRequest, ExecutionContext,
};
use familiar_core::{
    admit_run_prd, resolve_run_prd, validate_graph, AppPaths, BacklogDiscovery, BacklogStatusStore,
    Config, ExecutionPrice, FilesystemBacklogDiscovery,
};
use familiar_review::{
    AgentAssignment, AgentObservation, AgentRole, BlockingPolicy, BoundedDocument,
    CodingRemediationAdapter, CommandVerificationRunner, CoordinationRequest, GitEvidenceCollector,
    ReviewCoordinator, ReviewCycle, ReviewCycleState, ReviewDisposition, ReviewPackageBudget,
    ReviewStopReason, ReviewTask, StructuredReviewAdapter, VerificationCheck, VerificationPlan,
    WorkflowLimits,
};
use familiar_storage::{
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

pub fn execute(prd_path: &Path, agent: &dyn CodingAgent) -> Result<RunWorkflowResult, RunError> {
    let paths = AppPaths::new();
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|e| RunError::Config(e.to_string()))?;
    execute_with_config(prd_path, agent, &config, &paths)
}

pub fn execute_with_config(
    prd_path: &Path,
    agent: &dyn CodingAgent,
    config: &Config,
    paths: &AppPaths,
) -> Result<RunWorkflowResult, RunError> {
    let current = env::current_dir().map_err(RunError::CurrentDirectory)?;
    let context = ContextCompiler::new()
        .compile(ContextRequest {
            repository: &current,
            prd: prd_path,
        })
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
    let review_baseline = if config.review.enabled {
        config.review.validate().map_err(RunError::Config)?;
        Some(capture_worktree_baseline(
            &context.repository.worktree,
            &paths.data_dir,
            &id,
        )?)
    } else {
        None
    };
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(&current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let discovered = discovery
        .discover(&repository)
        .map_err(|e| RunError::Config(e.to_string()))?;
    validate_graph(&discovered).map_err(|e| RunError::Config(e.to_string()))?;
    let target = resolve_run_prd(&repository, &discovered, prd_path)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let snapshot = SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&repository, &discovered)
        .map_err(|e| RunError::Storage(e.to_string()))?;
    admit_run_prd(&snapshot, &target).map_err(|e| RunError::Config(e.to_string()))?;
    let claim_discovered = discovery
        .discover(&repository)
        .map_err(|e| RunError::Config(e.to_string()))?;
    validate_graph(&claim_discovered).map_err(|e| RunError::Config(e.to_string()))?;
    let claim_target = resolve_run_prd(&repository, &claim_discovered, prd_path)
        .map_err(|e| RunError::Config(e.to_string()))?;
    if claim_target != target || claim_discovered != discovered {
        return Err(RunError::Config(
            "backlog changed during run admission".into(),
        ));
    }
    let actor = format!("system:familiar-run:{id}");
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
        .map_err(|e| retained(&target, "history_failed", RunError::Storage(e.to_string())))?;

    let execution = agent.execute(
        ExecutionRequest {
            working_directory: &context.repository.worktree,
            denied_read_path: None,
            prompt: &prompt,
            filesystem: familiar_agent::FilesystemPolicy::Normal,
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
        Err(AgentExecutionError::Output { result, .. }) => (result.as_ref(), outcome(result)),
        Err(AgentExecutionError::Timeout { result }) => (result.as_ref(), "timed_out"),
    };
    if result.agent_version.is_none() {
        unavailable.insert("agent_version".into(), "version_probe_failed".into());
    }
    let finalization = terminal(&timer, result, outcome, unavailable, config);
    finalize(&db, &id, &finalization).map_err(|e| retained(&target, "history_failed", e))?;
    let result = execution.map_err(|e| retained(&target, agent_reason(&e), RunError::Agent(e)))?;
    if result.exit_code != Some(0) || result.signal.is_some() {
        return Err(retained(
            &target,
            "implementation_failed",
            RunError::Workflow {
                result: Some(Box::new(result.clone())),
                detail: "implementation agent did not exit successfully".into(),
            },
        ));
    }
    if !config.review.enabled {
        return Err(retained(
            &target,
            "review_disabled",
            RunError::Workflow {
                result: Some(Box::new(result)),
                detail: "review is disabled; backlog completion requires a clean review".into(),
            },
        ));
    }
    let cycle = run_review(ReviewRunInput {
        db: &db,
        context: &context,
        execution_id: &id,
        implementation_result: &result,
        implementation_finalization: &finalization,
        agent,
        config,
        paths,
        base_revision: review_baseline.as_deref().expect("enabled review baseline"),
    })
    .map_err(|e| retained(&target, "review_failed", e))?;
    if cycle.state != ReviewCycleState::Completed
        || cycle.disposition != ReviewDisposition::ReadyForHumanApproval
        || cycle.stop_reasons != [ReviewStopReason::CleanReview]
    {
        let reason = review_retained_reason(&cycle);
        return Err(retained(
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
            retained(
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

fn review_retained_reason(cycle: &ReviewCycle) -> &'static str {
    if cycle
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

fn retained(
    target: &familiar_core::DiscoveredPrd,
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
        _ => "implementation_failed",
    }
}

struct ReviewRunInput<'a> {
    db: &'a Database,
    context: &'a ExecutionContext,
    execution_id: &'a str,
    implementation_result: &'a ExecutionResult,
    implementation_finalization: &'a ExecutionFinalization,
    agent: &'a dyn CodingAgent,
    config: &'a Config,
    paths: &'a AppPaths,
    base_revision: &'a str,
}

fn run_review(input: ReviewRunInput<'_>) -> Result<ReviewCycle, RunError> {
    let ReviewRunInput {
        db,
        context,
        execution_id,
        implementation_result,
        implementation_finalization,
        agent,
        config,
        paths,
        base_revision,
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
        allowed_paths: config.review.allowed_paths.clone(),
        prohibited_changes: config.review.prohibited_changes.clone(),
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
        agent,
        context.repository.worktree.clone(),
        reviewer.clone(),
        review_timeout,
    );
    let remediation_adapter = CodingRemediationAdapter::new(
        agent,
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
                familiar_context::DocumentKind::Contract | familiar_context::DocumentKind::Adr
            )
        })
        .map(|document| BoundedDocument {
            source: document.path.clone(),
            content: document.content.clone(),
            content_hash: familiar_review::content_hash(document.content.as_bytes()),
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
        implementation_usage: familiar_review::ExecutionUsage {
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
    Ok(cycle)
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
    markdown_section(document, "Acceptance Criteria")
        .map(|section| {
            section
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let (_, criterion) = line.split_once(". ")?;
                    line.chars()
                        .next()
                        .is_some_and(|value| value.is_ascii_digit())
                        .then(|| criterion.to_owned())
                })
                .collect()
        })
        .unwrap_or_default()
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

fn new_id() -> String {
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
    use familiar_context::{ContextDocument, DocumentKind, InclusionReason, RepositoryContext};
    use familiar_review::{
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
            },
        };

        let error = execute_with_config(
            &repository.join("docs/prds/PRD-004.md"),
            &agent,
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

        execute_with_config(&prd, &agent, &config, &paths).unwrap_err();

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

        execute_with_config(&prd, &agent, &config, &paths).unwrap_err();

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
            &agent,
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
        fn isolation_capability(&self) -> familiar_agent::IsolationCapability {
            familiar_agent::IsolationCapability::FreshProcessPerExecution
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
                                range: familiar_review::LineRange { start: 1, end: 1 },
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
                    reviewer: familiar_review::AgentObservation {
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
                    usage: familiar_review::ExecutionUsage {
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
            })
        }
    }

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
        config.review.implementation_agent = familiar_core::config::ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: Some("fake".into()),
            model: Some("implementation-model".into()),
        };
        config.review.reviewer_agent = familiar_core::config::ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: Some("fake".into()),
            model: Some("review-model".into()),
        };
        config.review.verification = vec![familiar_core::config::ReviewVerificationConfig {
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
        (
            temp,
            database,
            context,
            config,
            paths,
            baseline,
            agent,
            finalization,
        )
    }

    #[test]
    fn production_composition_handles_clean_review_and_one_remediation() {
        for remediation in [false, true] {
            let (_temp, db, context, config, paths, baseline, agent, finalization) =
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
                },
                implementation_finalization: &finalization,
                agent: &agent,
                config: &config,
                paths: &paths,
                base_revision: &baseline,
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
}
