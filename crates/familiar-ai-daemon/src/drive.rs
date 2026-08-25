//! Unattended backlog driver: run eligible PRDs one after another through the
//! unchanged single-PRD workflow until the backlog is empty, nothing is
//! eligible, or the budget warrant is exhausted — recording a durable account
//! of what ran, what stopped, why, and what it cost.
//!
//! The loop adds no execution semantics. Selection, admission, claim,
//! verification, review, and fail-closed completion all remain exactly as
//! `familiar-ai run` performs them.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, BacklogStatus, BacklogStatusStore, Config,
    DiscoveredPrd, FilesystemBacklogDiscovery, PrdId, RepositoryIdentity,
};
use familiar_ai_storage::{Database, DriverRepository, ExecutionHistoryRepository};

use crate::run::{execute_with_config_tracked_from_preflighted, AgentSet};

/// Why a driver session stopped. Closed set; persisted verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveTermination {
    BacklogEmpty,
    NothingEligible,
    BudgetPrdsExhausted,
    BudgetCostExhausted,
    BudgetDurationExhausted,
    CostUnknown,
    StorageFailure,
    Interrupted,
    UnclassifiedResult,
    WorkerHeartbeatLost,
    PreflightFailed,
    DeliveryBlocked,
    BudgetDeliveriesExhausted,
}

impl DriveTermination {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BacklogEmpty => "backlog_empty",
            Self::NothingEligible => "nothing_eligible",
            Self::BudgetPrdsExhausted => "budget_prds_exhausted",
            Self::BudgetCostExhausted => "budget_cost_exhausted",
            Self::BudgetDurationExhausted => "budget_duration_exhausted",
            Self::CostUnknown => "cost_unknown",
            Self::StorageFailure => "storage_failure",
            Self::Interrupted => "interrupted",
            Self::UnclassifiedResult => "unclassified_result",
            Self::WorkerHeartbeatLost => "worker_heartbeat_lost",
            Self::PreflightFailed => "preflight_failed",
            Self::DeliveryBlocked => "delivery_blocked",
            Self::BudgetDeliveriesExhausted => "budget_deliveries_exhausted",
        }
    }

    /// Whether a supervised worker should exit unsuccessfully so launchd can
    /// retry after a transient or crash-like terminal condition. Policy and
    /// budget stops are deliberate finite outcomes and must not create a
    /// restart loop.
    pub fn worker_should_restart(&self) -> bool {
        matches!(
            self,
            Self::StorageFailure
                | Self::Interrupted
                | Self::UnclassifiedResult
                | Self::WorkerHeartbeatLost
                | Self::PreflightFailed
        )
    }
}

/// The session's budget ceilings. Zero means "no ceiling of this kind"; at
/// least one must be finite for a session to start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DriveWarrant {
    pub max_prds: u64,
    pub max_cost_microusd: u64,
    pub max_duration_ms: u64,
}

impl DriveWarrant {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_prds: config.driver.max_prds_per_session,
            max_cost_microusd: config.driver.max_session_cost_microusd,
            max_duration_ms: config.driver.max_session_duration_ms,
        }
    }

    /// Command-line ceilings may only tighten configuration: a supplied value
    /// wins when it is lower, or when configuration set no ceiling at all.
    /// Loosening is never possible.
    pub fn tightened_by(self, prds: Option<u64>, cost: Option<u64>, duration: Option<u64>) -> Self {
        fn tighten(configured: u64, supplied: Option<u64>) -> u64 {
            match (configured, supplied) {
                (_, None) => configured,
                (0, Some(value)) => value,
                (configured, Some(value)) => configured.min(value),
            }
        }
        Self {
            max_prds: tighten(self.max_prds, prds),
            max_cost_microusd: tighten(self.max_cost_microusd, cost),
            max_duration_ms: tighten(self.max_duration_ms, duration),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_prds == 0 && self.max_cost_microusd == 0 && self.max_duration_ms == 0 {
            return Err("unattended drive requires at least one finite ceiling".into());
        }
        Ok(())
    }

    fn as_json(&self) -> String {
        format!(
            "{{\"max_prds\":{},\"max_cost_microusd\":{},\"max_duration_ms\":{}}}",
            self.max_prds, self.max_cost_microusd, self.max_duration_ms
        )
    }

    /// The ceiling breached before starting another attempt, if any.
    fn exhausted(&self, attempted: u64, cost: u64, elapsed_ms: u64) -> Option<DriveTermination> {
        if self.max_prds > 0 && attempted >= self.max_prds {
            return Some(DriveTermination::BudgetPrdsExhausted);
        }
        if self.max_cost_microusd > 0 && cost >= self.max_cost_microusd {
            return Some(DriveTermination::BudgetCostExhausted);
        }
        if self.max_duration_ms > 0 && elapsed_ms >= self.max_duration_ms {
            return Some(DriveTermination::BudgetDurationExhausted);
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveSummary {
    pub session_id: String,
    pub termination: DriveTermination,
    pub attempted: u64,
    pub completed: u64,
    pub known_cost_microusd: u64,
}

#[derive(Debug)]
pub enum DriveError {
    Config(String),
    Storage(String),
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) => write!(f, "drive configuration failed: {message}"),
            Self::Storage(message) => write!(f, "drive storage failed: {message}"),
        }
    }
}
impl std::error::Error for DriveError {}

/// One eligible PRD, chosen by exactly the rules `familiar-ai next` applies,
/// minus anything already attempted in this session.
enum Selection {
    Eligible(Vec<DiscoveredPrd>),
    BacklogEmpty,
    NothingEligible,
}

fn select_batch(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    db: &mut Database,
    attempted: &BTreeSet<PrdId>,
    limit: usize,
) -> Result<Selection, DriveError> {
    if discovered.is_empty() {
        return Ok(Selection::BacklogEmpty);
    }
    let mut entries = familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    entries.sort_by(|a, b| {
        a.prd
            .id
            .cmp(&b.prd.id)
            .then_with(|| a.prd.path.cmp(&b.prd.path))
    });
    let statuses: std::collections::BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.prd.id.clone(), entry.status))
        .collect();
    let mut selected = Vec::new();
    let mut selected_scopes: Vec<Vec<String>> = Vec::new();
    for entry in entries {
        // An attempt that failed before claim leaves the PRD pending; without
        // this exclusion the session would select it forever.
        if attempted.contains(&entry.prd.id) {
            continue;
        }
        let dependencies_met = entry
            .prd
            .dependencies
            .iter()
            .all(|id| statuses.get(id) == Some(&BacklogStatus::Completed));
        if entry.status == BacklogStatus::Pending && dependencies_met {
            let scopes = conflict_scopes(&repository.worktree, &entry.prd)?;
            if selected_scopes
                .iter()
                .all(|existing| !scopes_conflict(existing, &scopes))
            {
                selected.push(entry.prd);
                selected_scopes.push(scopes);
                if selected.len() == limit.max(1) {
                    break;
                }
            }
        }
    }
    if selected.is_empty() {
        Ok(Selection::NothingEligible)
    } else {
        Ok(Selection::Eligible(selected))
    }
}

fn conflict_scopes(repository: &Path, prd: &DiscoveredPrd) -> Result<Vec<String>, DriveError> {
    let path = repository.join(prd.path.as_str());
    let content = std::fs::read_to_string(&path).map_err(|error| {
        DriveError::Config(format!(
            "cannot read conflict scope from {}: {error}",
            path.display()
        ))
    })?;
    let entries = familiar_ai_review::parse_expected_files(&content).map_err(|error| {
        DriveError::Config(format!(
            "cannot parse conflict scope from {}: {error}",
            path.display()
        ))
    })?;
    Ok(entries.into_iter().map(|entry| entry.normalized).collect())
}

fn scopes_conflict(left: &[String], right: &[String]) -> bool {
    left.iter().any(|a| {
        right
            .iter()
            .any(|b| shared_global_path(a) || shared_global_path(b) || path_scopes_overlap(a, b))
    })
}

fn shared_global_path(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path);
    path.contains("migration")
        || path.contains("schema")
        || matches!(
            name,
            "Cargo.toml"
                | "Cargo.lock"
                | "go.mod"
                | "go.sum"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
        )
}

fn path_scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || (left.ends_with('/') && right.starts_with(left))
        || (right.ends_with('/') && left.starts_with(right))
}

fn routed_model(config: &Config, scope_count: usize) -> Option<String> {
    config
        .driver
        .model_routes
        .iter()
        .find(|route| scope_count <= route.max_expected_files)
        .map(|route| route.model.clone())
        .or_else(|| {
            config
                .agents
                .as_ref()
                .and_then(|agents| agents.implementation.model.clone())
        })
}

/// Execute eligible backlog PRDs until a closed termination condition is met.
pub fn drive(
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    warrant: DriveWarrant,
) -> Result<DriveSummary, DriveError> {
    warrant.validate().map_err(DriveError::Config)?;
    if config.driver.max_concurrency == 0 {
        return Err(DriveError::Config(
            "driver.max_concurrency must be positive".into(),
        ));
    }
    if config.driver.max_concurrency > 1 && !config.driver.isolated_worktrees {
        return Err(DriveError::Config(
            "driver.isolated_worktrees must be true when max_concurrency is greater than one"
                .into(),
        ));
    }
    if config.delivery.enabled && !config.driver.isolated_worktrees {
        return Err(DriveError::Config(
            "delivery requires driver.isolated_worktrees=true".into(),
        ));
    }
    let _worker_lock = crate::worker_lock::WorkerLock::acquire(&paths.runtime_dir)
        .map_err(|error| DriveError::Config(format!("cannot acquire driver ownership: {error}")))?;
    let current = std::env::current_dir().map_err(|error| {
        DriveError::Config(format!("cannot resolve current directory: {error}"))
    })?;
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(&current)
        .map_err(|error| DriveError::Config(error.to_string()))?;
    let repository_config = config.repository(&repository.worktree);
    let (implementation_entry, _) =
        crate::run::resolved_agent_entries(config).map_err(DriveError::Config)?;
    let adapter_id = implementation_entry.adapter.as_str();
    let configured_model = implementation_entry.model.as_deref();

    let database_path = config.database.resolve_path(&paths.data_dir);
    let mut db =
        Database::open(&database_path).map_err(|error| DriveError::Storage(error.to_string()))?;
    db.run_migrations()
        .map_err(|error| DriveError::Storage(error.to_string()))?;

    // A prior worker may have been killed while an agent was running. Close
    // those rows before opening this session so `report` never presents an
    // ambiguous "unrecorded" attempt after a restart.
    DriverRepository::new(db.conn())
        .recover_incomplete()
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    crate::worktree::recover_incomplete(&paths.state_dir).map_err(|error| {
        DriveError::Storage(format!("cannot recover worktree evidence: {error}"))
    })?;
    let session_id = format!("drive-{}", crate::run::new_id());
    DriverRepository::new(db.conn())
        .open_session(&session_id, &repository.key, &warrant.as_json())
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    eprintln!(
        "drive: session {session_id} started warrant={}",
        warrant.as_json()
    );
    DriverRepository::new(db.conn())
        .heartbeat(&session_id, &session_id)
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    let heartbeat = HeartbeatGuard::start(
        database_path.clone(),
        session_id.clone(),
        Duration::from_secs(config.daemon.heartbeat_interval_secs.max(1)),
    );

    let timer = Instant::now();
    let mut attempted_ids: BTreeSet<PrdId> = BTreeSet::new();
    let mut attempted = 0_u64;
    let mut completed = 0_u64;
    let mut known_cost = 0_u64;
    let mut delivered = 0_u64;

    let session_preflight = crate::preflight::run(agents, config, &repository.worktree);
    let termination = if !session_preflight.is_valid() {
        let detail = session_preflight.failure_summary();
        DriverRepository::new(db.conn())
            .record_session_detail(&session_id, &detail)
            .map_err(|error| DriveError::Storage(error.to_string()))?;
        eprintln!("drive: session preflight failed: {}", detail);
        DriveTermination::PreflightFailed
    } else {
        loop {
            if heartbeat.failed() {
                break DriveTermination::WorkerHeartbeatLost;
            }
            let elapsed = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if let Some(reason) = warrant.exhausted(attempted, known_cost, elapsed) {
                break reason;
            }
            let discovered =
                match discovery.discover_with_layout(&repository, &repository_config.layout()) {
                    Ok(discovered) => discovered,
                    Err(error) => {
                        eprintln!("drive: discovery failed: {error}");
                        break DriveTermination::StorageFailure;
                    }
                };
            if let Err(error) = validate_graph(&discovered) {
                eprintln!("drive: backlog graph invalid: {error}");
                break DriveTermination::StorageFailure;
            }
            let remaining = if warrant.max_prds == 0 {
                config.driver.max_concurrency
            } else {
                usize::try_from(warrant.max_prds.saturating_sub(attempted))
                    .unwrap_or(usize::MAX)
                    .min(config.driver.max_concurrency)
            };
            let targets =
                match select_batch(&repository, &discovered, &mut db, &attempted_ids, remaining)? {
                    Selection::Eligible(prds) => prds,
                    Selection::BacklogEmpty => break DriveTermination::BacklogEmpty,
                    Selection::NothingEligible => break DriveTermination::NothingEligible,
                };
            let mut jobs = Vec::new();
            let mut preparation_failed = false;
            for target in targets {
                attempted_ids.insert(target.id.clone());
                attempted += 1;
                let sequence = match DriverRepository::new(db.conn()).record_attempt_started(
                    &session_id,
                    &target.id.to_string(),
                    target.path.as_str(),
                    None,
                ) {
                    Ok(sequence) => sequence,
                    Err(error) => {
                        eprintln!("drive: cannot record attempt: {error}");
                        preparation_failed = true;
                        break;
                    }
                };
                eprintln!("drive: attempt {sequence} {} {}", target.id, target.path);
                if let Err(error) = DriverRepository::new(db.conn()).record_attempt_diagnostics(
                    &session_id,
                    sequence,
                    None,
                    Some(adapter_id),
                    configured_model,
                    None,
                    None,
                    "preflight",
                ) {
                    eprintln!("drive: cannot record attempt phase: {error}");
                    preparation_failed = true;
                    break;
                }
                let mut execution_config = config.clone();
                let scope_count = conflict_scopes(&repository.worktree, &target)?.len();
                if let Some(model) = routed_model(config, scope_count) {
                    if let Some(entries) = &mut execution_config.agents {
                        entries.implementation.model = Some(model.clone());
                        execution_config.review.implementation_agent.model = Some(model);
                    }
                }
                let worktree = if config.driver.isolated_worktrees {
                    match crate::worktree::WorktreeLease::create(
                        &repository.worktree,
                        &paths.state_dir,
                        &session_id,
                        &target.id.to_string(),
                    ) {
                        Ok(lease) => Some(lease),
                        Err(error) => {
                            let _ = DriverRepository::new(db.conn()).record_attempt_finished(
                                &session_id,
                                sequence,
                                "retained",
                                Some("worktree_failed"),
                                None,
                                None,
                            );
                            eprintln!("drive: isolated worktree creation failed: {error}");
                            preparation_failed = true;
                            break;
                        }
                    }
                } else {
                    None
                };
                let execution_root = worktree
                    .as_ref()
                    .map(|lease| lease.path().to_path_buf())
                    .unwrap_or_else(|| repository.worktree.clone());
                let worktree_heartbeat = worktree.as_ref().map(|lease| {
                    lease.start_heartbeat(Duration::from_secs(
                        config.daemon.heartbeat_interval_secs.max(1),
                    ))
                });
                if worktree.is_some() {
                    execution_config.repositories.insert(
                        execution_root.display().to_string(),
                        repository_config.clone(),
                    );
                }
                let routed_model = execution_config
                    .agents
                    .as_ref()
                    .and_then(|entries| entries.implementation.model.clone());
                jobs.push((
                    target,
                    sequence,
                    execution_root,
                    execution_config,
                    routed_model,
                    worktree,
                    worktree_heartbeat,
                ));
            }
            if preparation_failed {
                break DriveTermination::StorageFailure;
            }

            let mut results = std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for (
                    target,
                    sequence,
                    execution_root,
                    execution_config,
                    routed_model,
                    worktree,
                    worktree_heartbeat,
                ) in jobs
                {
                    handles.push(scope.spawn(move || {
                        let attempt_timer = Instant::now();
                        let prd_path = execution_root.join(target.path.as_str());
                        let execution =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                execute_with_config_tracked_from_preflighted(
                                    &execution_root,
                                    &prd_path,
                                    agents,
                                    &execution_config,
                                    paths,
                                    true,
                                )
                            }));
                        let duration_ms =
                            attempt_timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        (
                            target,
                            sequence,
                            routed_model,
                            worktree,
                            execution,
                            duration_ms,
                            worktree_heartbeat,
                        )
                    }));
                }
                handles
                    .into_iter()
                    .map(|handle| handle.join())
                    .collect::<Vec<_>>()
            });
            let mut batch_stop = None;
            for joined in results.drain(..) {
                let (
                    target,
                    sequence,
                    routed_model,
                    mut worktree,
                    execution,
                    duration_ms,
                    worktree_heartbeat,
                ) = joined.unwrap();
                let heartbeat_failed = worktree_heartbeat
                    .as_ref()
                    .is_some_and(crate::worktree::WorktreeHeartbeatGuard::failed);
                drop(worktree_heartbeat);
                let (result, trace) = match execution {
                    Ok(value) => value,
                    Err(_) => {
                        eprintln!("drive: attempt worker panicked for {}", target.id);
                        if let Some(lease) = &mut worktree {
                            if let Err(error) = lease.mark_state("retained_unclassified") {
                                eprintln!("drive: cannot persist panicked worktree state: {error}");
                                batch_stop = Some(DriveTermination::StorageFailure);
                            }
                        }
                        if let Err(error) = DriverRepository::new(db.conn())
                            .record_attempt_diagnostics(
                                &session_id,
                                sequence,
                                None,
                                Some(adapter_id),
                                routed_model.as_deref().or(configured_model),
                                None,
                                None,
                                "retained",
                            )
                            .and_then(|()| {
                                DriverRepository::new(db.conn()).record_attempt_finished(
                                    &session_id,
                                    sequence,
                                    "retained",
                                    Some("unclassified_result"),
                                    None,
                                    Some(duration_ms),
                                )
                            })
                        {
                            eprintln!("drive: cannot terminalize panicked attempt: {error}");
                            batch_stop = Some(DriveTermination::StorageFailure);
                        } else {
                            batch_stop.get_or_insert(DriveTermination::UnclassifiedResult);
                        }
                        continue;
                    }
                };
                if heartbeat_failed {
                    eprintln!("drive: worktree heartbeat failed for {}", target.id);
                    batch_stop = Some(DriveTermination::WorkerHeartbeatLost);
                }
                let terminal = match &result {
                    Ok(workflow) => Some(&workflow.implementation),
                    Err(crate::run::RunError::Agent(error)) => Some(error.result()),
                    Err(crate::run::RunError::Workflow {
                        result: Some(result),
                        ..
                    }) => Some(result.as_ref()),
                    _ => None,
                };
                if let Err(error) = DriverRepository::new(db.conn()).record_attempt_diagnostics(
                    &session_id,
                    sequence,
                    trace.execution_id.as_deref(),
                    Some(adapter_id),
                    terminal
                        .and_then(|value| value.model.as_deref())
                        .or(routed_model.as_deref())
                        .or(configured_model),
                    terminal.and_then(|value| value.exit_code),
                    terminal.and_then(|value| value.signal),
                    if result.is_ok() {
                        "completed"
                    } else {
                        "retained"
                    },
                ) {
                    eprintln!("drive: cannot record terminal diagnostics: {error}");
                    batch_stop = Some(DriveTermination::StorageFailure);
                    continue;
                }

                let cost = trace
                    .execution_id
                    .as_deref()
                    .and_then(|id| attempt_cost(&db, id));
                if let Some(value) = cost {
                    known_cost = known_cost.saturating_add(value);
                }
                let unclassified = result.is_err() && trace.retained_reason.is_none();
                if let Err(error) = &result {
                    eprintln!("drive: attempt {sequence} {} failed: {error}", target.id);
                }
                let (outcome, retained_reason) = match &result {
                    Ok(_) => {
                        completed += 1;
                        ("completed", None)
                    }
                    Err(_) => (
                        "retained",
                        trace.retained_reason.or(Some("unclassified_result")),
                    ),
                };
                if let Some(lease) = &mut worktree {
                    let state = if result.is_ok() {
                        "ready_for_delivery"
                    } else {
                        "retained"
                    };
                    if let Err(error) = lease.mark_state(state) {
                        eprintln!("drive: cannot persist worktree state: {error}");
                        batch_stop = Some(DriveTermination::StorageFailure);
                        continue;
                    }
                    if result.is_ok() && config.delivery.enabled {
                        if delivered >= config.delivery.max_deliveries_per_session {
                            batch_stop.get_or_insert(DriveTermination::BudgetDeliveriesExhausted);
                        } else {
                            let delivery_heartbeat = lease.start_heartbeat(Duration::from_secs(
                                config.daemon.heartbeat_interval_secs.max(1),
                            ));
                            let delivery_result =
                                crate::delivery::deliver(lease.ownership_path(), &config.delivery);
                            let delivery_heartbeat_failed = delivery_heartbeat.failed();
                            drop(delivery_heartbeat);
                            if delivery_heartbeat_failed {
                                eprintln!(
                                    "drive: delivery worktree heartbeat failed for {}",
                                    target.id
                                );
                                batch_stop = Some(DriveTermination::WorkerHeartbeatLost);
                            }
                            match delivery_result {
                                Ok(delivery) => {
                                    delivered = delivered.saturating_add(1);
                                    if let Err(error) = lease.mark_state(&delivery.phase) {
                                        eprintln!(
                                            "drive: cannot persist delivered worktree state: {error}"
                                        );
                                        batch_stop = Some(DriveTermination::StorageFailure);
                                    }
                                    if let Err(error) = DriverRepository::new(db.conn())
                                        .record_attempt_diagnostics(
                                            &session_id,
                                            sequence,
                                            trace.execution_id.as_deref(),
                                            Some(adapter_id),
                                            terminal
                                                .and_then(|value| value.model.as_deref())
                                                .or(routed_model.as_deref())
                                                .or(configured_model),
                                            terminal.and_then(|value| value.exit_code),
                                            terminal.and_then(|value| value.signal),
                                            &delivery.phase,
                                        )
                                    {
                                        eprintln!("drive: cannot persist delivery phase: {error}");
                                        batch_stop = Some(DriveTermination::StorageFailure);
                                    }
                                }
                                Err(error) => {
                                    eprintln!("drive: delivery blocked for {}: {error}", target.id);
                                    if let Err(storage_error) = DriverRepository::new(db.conn())
                                        .record_attempt_diagnostics(
                                            &session_id,
                                            sequence,
                                            trace.execution_id.as_deref(),
                                            Some(adapter_id),
                                            terminal
                                                .and_then(|value| value.model.as_deref())
                                                .or(routed_model.as_deref())
                                                .or(configured_model),
                                            terminal.and_then(|value| value.exit_code),
                                            terminal.and_then(|value| value.signal),
                                            "delivery_blocked",
                                        )
                                    {
                                        eprintln!(
                                            "drive: cannot persist delivery blocker: {storage_error}"
                                        );
                                        batch_stop = Some(DriveTermination::StorageFailure);
                                    } else {
                                        batch_stop = Some(DriveTermination::DeliveryBlocked);
                                    }
                                }
                            }
                        }
                    }
                }
                if let Err(error) = DriverRepository::new(db.conn()).record_attempt_finished(
                    &session_id,
                    sequence,
                    outcome,
                    retained_reason,
                    cost,
                    Some(duration_ms),
                ) {
                    eprintln!("drive: cannot record attempt outcome: {error}");
                    batch_stop = Some(DriveTermination::StorageFailure);
                    continue;
                }
                eprintln!(
                    "drive: attempt {sequence} {} outcome={outcome}{}",
                    target.id,
                    retained_reason
                        .map(|reason| format!(" reason={reason}"))
                        .unwrap_or_default()
                );
                if unclassified {
                    batch_stop.get_or_insert(DriveTermination::UnclassifiedResult);
                }
                if warrant.max_cost_microusd > 0 && cost.is_none() {
                    batch_stop.get_or_insert(DriveTermination::CostUnknown);
                }
            }
            if let Some(reason) = batch_stop {
                break reason;
            }
            if let Err(error) = DriverRepository::new(db.conn()).heartbeat(&session_id, &session_id)
            {
                eprintln!("drive: heartbeat persistence failed: {error}");
                break DriveTermination::StorageFailure;
            }
        }
    };

    DriverRepository::new(db.conn())
        .finish_session(&session_id, termination.as_str())
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    eprintln!(
        "drive: session {session_id} terminated reason={} attempted={attempted} completed={completed}",
        termination.as_str()
    );
    Ok(DriveSummary {
        session_id,
        termination,
        attempted,
        completed,
        known_cost_microusd: known_cost,
    })
}

struct HeartbeatGuard {
    stop: std::sync::mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl HeartbeatGuard {
    fn start(database_path: std::path::PathBuf, session_id: String, interval: Duration) -> Self {
        let (stop, receiver) = std::sync::mpsc::channel();
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_failed = std::sync::Arc::clone(&failed);
        let worker_id = session_id.clone();
        let handle = std::thread::spawn(move || {
            while receiver.recv_timeout(interval).is_err() {
                let result = Database::open(&database_path).and_then(|database| {
                    database.run_migrations()?;
                    DriverRepository::new(database.conn()).heartbeat(&session_id, &worker_id)
                });
                if result.is_err() {
                    thread_failed.store(true, std::sync::atomic::Ordering::Release);
                    break;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            failed,
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The recorded cost of one execution, when pricing made it knowable.
fn attempt_cost(db: &Database, execution_id: &str) -> Option<u64> {
    ExecutionHistoryRepository::new(db.conn())
        .recent(100)
        .ok()?
        .into_iter()
        .find(|row| row.execution_id == execution_id)
        .and_then(|row| row.estimated_cost_microusd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warrant_requires_at_least_one_finite_ceiling() {
        assert!(DriveWarrant::default().validate().is_err());
        assert!(DriveWarrant {
            max_prds: 1,
            ..DriveWarrant::default()
        }
        .validate()
        .is_ok());
        assert!(DriveWarrant {
            max_duration_ms: 1,
            ..DriveWarrant::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn command_line_ceilings_only_tighten() {
        let configured = DriveWarrant {
            max_prds: 10,
            max_cost_microusd: 500,
            max_duration_ms: 1_000,
        };
        // Lower values win.
        let tightened = configured.tightened_by(Some(3), Some(100), Some(250));
        assert_eq!(
            tightened,
            DriveWarrant {
                max_prds: 3,
                max_cost_microusd: 100,
                max_duration_ms: 250
            }
        );
        // Higher values are ignored — a flag can never loosen configuration.
        assert_eq!(
            configured.tightened_by(Some(99), Some(9_999), Some(9_999)),
            configured
        );
        // Absent flags leave configuration alone.
        assert_eq!(configured.tightened_by(None, None, None), configured);
        // A flag may set a ceiling where configuration had none.
        assert_eq!(
            DriveWarrant::default().tightened_by(Some(5), None, None),
            DriveWarrant {
                max_prds: 5,
                ..DriveWarrant::default()
            }
        );
    }

    #[test]
    fn each_ceiling_reports_its_own_exhaustion() {
        let warrant = DriveWarrant {
            max_prds: 2,
            max_cost_microusd: 100,
            max_duration_ms: 1_000,
        };
        assert_eq!(warrant.exhausted(0, 0, 0), None);
        assert_eq!(
            warrant.exhausted(2, 0, 0),
            Some(DriveTermination::BudgetPrdsExhausted)
        );
        assert_eq!(
            warrant.exhausted(0, 100, 0),
            Some(DriveTermination::BudgetCostExhausted)
        );
        assert_eq!(
            warrant.exhausted(0, 0, 1_000),
            Some(DriveTermination::BudgetDurationExhausted)
        );
        // Ceilings set to zero never trigger.
        let only_prds = DriveWarrant {
            max_prds: 1,
            ..DriveWarrant::default()
        };
        assert_eq!(only_prds.exhausted(0, u64::MAX, u64::MAX), None);
    }

    #[test]
    fn termination_reasons_are_stable_strings() {
        for (termination, text) in [
            (DriveTermination::BacklogEmpty, "backlog_empty"),
            (DriveTermination::NothingEligible, "nothing_eligible"),
            (
                DriveTermination::BudgetPrdsExhausted,
                "budget_prds_exhausted",
            ),
            (
                DriveTermination::BudgetCostExhausted,
                "budget_cost_exhausted",
            ),
            (
                DriveTermination::BudgetDurationExhausted,
                "budget_duration_exhausted",
            ),
            (DriveTermination::CostUnknown, "cost_unknown"),
            (DriveTermination::StorageFailure, "storage_failure"),
            (DriveTermination::Interrupted, "interrupted"),
            (DriveTermination::UnclassifiedResult, "unclassified_result"),
            (
                DriveTermination::WorkerHeartbeatLost,
                "worker_heartbeat_lost",
            ),
            (DriveTermination::PreflightFailed, "preflight_failed"),
            (DriveTermination::DeliveryBlocked, "delivery_blocked"),
            (
                DriveTermination::BudgetDeliveriesExhausted,
                "budget_deliveries_exhausted",
            ),
        ] {
            assert_eq!(termination.as_str(), text);
        }
    }

    #[test]
    fn only_transient_or_crash_like_worker_terminations_request_restart() {
        for termination in [
            DriveTermination::StorageFailure,
            DriveTermination::Interrupted,
            DriveTermination::UnclassifiedResult,
            DriveTermination::WorkerHeartbeatLost,
            DriveTermination::PreflightFailed,
        ] {
            assert!(termination.worker_should_restart(), "{termination:?}");
        }
        for termination in [
            DriveTermination::BacklogEmpty,
            DriveTermination::NothingEligible,
            DriveTermination::BudgetPrdsExhausted,
            DriveTermination::BudgetCostExhausted,
            DriveTermination::BudgetDurationExhausted,
            DriveTermination::CostUnknown,
            DriveTermination::DeliveryBlocked,
            DriveTermination::BudgetDeliveriesExhausted,
        ] {
            assert!(!termination.worker_should_restart(), "{termination:?}");
        }
    }

    #[test]
    fn warrant_json_is_the_recorded_snapshot() {
        let warrant = DriveWarrant {
            max_prds: 4,
            max_cost_microusd: 0,
            max_duration_ms: 60_000,
        };
        assert_eq!(
            warrant.as_json(),
            r#"{"max_prds":4,"max_cost_microusd":0,"max_duration_ms":60000}"#
        );
    }

    #[test]
    fn conflict_scopes_serialize_shared_and_nested_paths() {
        assert!(path_scopes_overlap("src/", "src/lib.rs"));
        assert!(scopes_conflict(
            &["db/migrations/001.sql".into()],
            &["cmd/server.go".into()]
        ));
        assert!(scopes_conflict(
            &["crates/a/Cargo.toml".into()],
            &["crates/b/src/".into()]
        ));
        assert!(!scopes_conflict(
            &["crates/a/src/".into()],
            &["crates/b/src/".into()]
        ));
    }

    #[test]
    fn model_routes_are_deterministic_and_fall_back_to_configured_model() {
        let mut config = Config::default();
        config.driver.model_routes = vec![
            familiar_ai_core::DriverModelRouteConfig {
                max_expected_files: 2,
                model: "local-narrow".into(),
            },
            familiar_ai_core::DriverModelRouteConfig {
                max_expected_files: 8,
                model: "strong-medium".into(),
            },
        ];
        config.agents = Some(familiar_ai_core::AgentsConfig {
            implementation: familiar_ai_core::AgentEntryConfig {
                model: Some("strong-broad".into()),
                ..Default::default()
            },
            reviewer: Default::default(),
        });
        assert_eq!(routed_model(&config, 1).as_deref(), Some("local-narrow"));
        assert_eq!(routed_model(&config, 5).as_deref(), Some("strong-medium"));
        assert_eq!(routed_model(&config, 20).as_deref(), Some("strong-broad"));
    }
}
