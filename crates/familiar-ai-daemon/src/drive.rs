//! Unattended backlog driver: run eligible PRDs one after another through the
//! unchanged single-PRD workflow until the backlog is empty, nothing is
//! eligible, or the budget warrant is exhausted — recording a durable account
//! of what ran, what stopped, why, and what it cost.
//!
//! The loop adds no execution semantics. Selection, admission, claim,
//! verification, review, and fail-closed completion all remain exactly as
//! `familiar-ai run` performs them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, BacklogStatus, BacklogStatusStore, Config,
    DiscoveredPrd, FilesystemBacklogDiscovery, PrdId, RepositoryIdentity,
};
use familiar_ai_storage::{
    Database, DeliveryRepository, DriverRepository, ExecutionHistoryRepository,
};

use crate::run::{execute_with_config_tracked_from_preflighted, AgentSet};

/// Why a driver session stopped. Closed set; persisted verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveTermination {
    BacklogEmpty,
    NothingEligible,
    BudgetPrdsExhausted,
    BudgetCostExhausted,
    BudgetTokensExhausted,
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
            Self::BudgetTokensExhausted => "budget_tokens_exhausted",
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
    pub max_tokens: u64,
    pub max_duration_ms: u64,
}

impl DriveWarrant {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_prds: config.driver.max_prds_per_session,
            max_cost_microusd: config.driver.max_session_cost_microusd,
            max_tokens: config.driver.max_session_tokens,
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
            max_tokens: self.max_tokens,
            max_duration_ms: tighten(self.max_duration_ms, duration),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_prds == 0
            && self.max_cost_microusd == 0
            && self.max_tokens == 0
            && self.max_duration_ms == 0
        {
            return Err("unattended drive requires at least one finite ceiling".into());
        }
        Ok(())
    }

    fn as_json(&self) -> String {
        format!(
            "{{\"max_prds\":{},\"max_cost_microusd\":{},\"max_tokens\":{},\"max_duration_ms\":{}}}",
            self.max_prds, self.max_cost_microusd, self.max_tokens, self.max_duration_ms
        )
    }

    /// The ceiling breached before starting another attempt, if any.
    fn exhausted(
        &self,
        attempted: u64,
        cost: u64,
        tokens: u64,
        elapsed_ms: u64,
    ) -> Option<DriveTermination> {
        if self.max_prds > 0 && attempted >= self.max_prds {
            return Some(DriveTermination::BudgetPrdsExhausted);
        }
        if self.max_cost_microusd > 0 && cost >= self.max_cost_microusd {
            return Some(DriveTermination::BudgetCostExhausted);
        }
        if self.max_tokens > 0 && tokens >= self.max_tokens {
            return Some(DriveTermination::BudgetTokensExhausted);
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
    pub known_tokens: u64,
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
    components: &BTreeMap<PrdId, String>,
    stopped_components: &BTreeSet<String>,
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
    let mut selected_components = BTreeSet::new();
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
            let component = &components[&entry.prd.id];
            if !stopped_components.contains(component)
                && selected_components.insert(component.clone())
            {
                selected.push(entry.prd);
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

/// Deterministically partition the validated, profile-neutral dependency graph
/// into weakly connected components. Component identity is its least PRD id.
pub fn dependency_components(prds: &[DiscoveredPrd]) -> BTreeMap<PrdId, String> {
    let mut neighbors = BTreeMap::<PrdId, BTreeSet<PrdId>>::new();
    for prd in prds {
        neighbors.entry(prd.id.clone()).or_default();
        for dependency in &prd.dependencies {
            neighbors
                .entry(prd.id.clone())
                .or_default()
                .insert(dependency.clone());
            neighbors
                .entry(dependency.clone())
                .or_default()
                .insert(prd.id.clone());
        }
    }
    let mut result = BTreeMap::new();
    for root in neighbors.keys() {
        if result.contains_key(root) {
            continue;
        }
        let mut pending = vec![root.clone()];
        let mut members = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if members.insert(id.clone()) {
                pending.extend(neighbors[&id].iter().cloned());
            }
        }
        let first = members.first().expect("component is nonempty");
        let component = format!(
            "component-PRD-{:03}{}",
            first.number(),
            first
                .suffix()
                .map(|value| value.to_string())
                .unwrap_or_default()
        );
        for id in members {
            result.insert(id, component.clone());
        }
    }
    result
}

fn conflict_scopes(repository: &Path, prd: &DiscoveredPrd) -> Result<Vec<String>, DriveError> {
    let path = repository.join(prd.path.as_str());
    let content = std::fs::read_to_string(&path).map_err(|error| {
        DriveError::Config(format!(
            "cannot read conflict scope from {}: {error}",
            path.display()
        ))
    })?;
    let scope_content = if prd.metadata.contract_version == Some(1) {
        let bullets = prd
            .metadata
            .expected_files
            .iter()
            .map(|path| format!("- `{path}`\n"))
            .collect::<String>();
        format!("## Expected Files\n\n{bullets}")
    } else {
        content
    };
    let entries = familiar_ai_review::parse_expected_files(&scope_content).map_err(|error| {
        DriveError::Config(format!(
            "cannot parse conflict scope from {}: {error}",
            path.display()
        ))
    })?;
    Ok(entries.into_iter().map(|entry| entry.normalized).collect())
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
    // Component concurrency is an explicit opt-in independent of the legacy
    // PRD worker ceiling. A value of one preserves PRD-017's serial path.
    // Finite cost/token session ceilings cannot be safely reserved from
    // unknown future usage, so they deliberately retain serial admission.
    let parallelism = component_parallelism(config, &warrant);
    if parallelism == 0 {
        return Err(DriveError::Config(
            "driver.max_concurrency must be positive".into(),
        ));
    }
    let current = std::env::current_dir().map_err(|error| {
        DriveError::Config(format!("cannot resolve current directory: {error}"))
    })?;
    let discovery = FilesystemBacklogDiscovery;
    let repository = discovery
        .resolve(&current)
        .map_err(|error| DriveError::Config(error.to_string()))?;
    let _worker_lock =
        crate::worker_lock::WorkerLock::acquire_repository(&paths.runtime_dir, &repository.key)
            .map_err(|error| {
                DriveError::Config(format!("cannot acquire driver ownership: {error}"))
            })?;
    let repository_config = config.repository(&repository.worktree);
    let delivery_policy = repository_config.delivery.as_ref();
    if delivery_policy.is_some_and(|policy| policy.mode != familiar_ai_core::DeliveryMode::Disabled)
        && parallelism == 1
        && !config.driver.isolated_worktrees
    {
        return Err(DriveError::Config(
            "delivery requires driver.isolated_worktrees=true".into(),
        ));
    }
    let effective = config.effective_execution(&repository.worktree);
    let review_configuration_source = effective.review_source.as_str();
    let execution_context_configuration_source = effective.execution_context_source.as_str();
    let mut effective_config = config.clone();
    effective_config.review = effective.review;
    effective_config.execution_context = effective.execution_context;
    // A drive session is pinned to one repository. Isolated worker paths are
    // implementation details and must not trigger a second policy lookup.
    effective_config.repositories.clear();
    let config = &effective_config;
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
    let mut known_tokens = 0_u64;
    let mut delivered = 0_u64;
    let mut poc_risks_accepted = 0_u64;
    let mut stopped_components = BTreeSet::new();
    let mut component_worktrees = BTreeMap::<String, crate::worktree::WorktreeLease>::new();

    let session_preflight = crate::preflight::run(agents, config, &repository.worktree);
    let termination = if !session_preflight.is_valid() {
        let detail = session_preflight.failure_summary();
        DriverRepository::new(db.conn())
            .record_session_detail(&session_id, &detail)
            .map_err(|error| DriveError::Storage(error.to_string()))?;
        eprintln!("drive: session preflight failed: {detail}");
        DriveTermination::PreflightFailed
    } else {
        loop {
            if heartbeat.failed() {
                break DriveTermination::WorkerHeartbeatLost;
            }
            let elapsed = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if let Some(reason) = warrant.exhausted(attempted, known_cost, known_tokens, elapsed) {
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
            let components = dependency_components(&discovered);
            let remaining = if warrant.max_prds == 0 {
                parallelism
            } else {
                usize::try_from(warrant.max_prds.saturating_sub(attempted))
                    .unwrap_or(usize::MAX)
                    .min(parallelism)
            };
            let targets = match select_batch(
                &repository,
                &discovered,
                &mut db,
                &attempted_ids,
                &components,
                &stopped_components,
                remaining,
            )? {
                Selection::Eligible(prds) => prds,
                Selection::BacklogEmpty => break DriveTermination::BacklogEmpty,
                Selection::NothingEligible => break DriveTermination::NothingEligible,
            };
            let mut jobs = Vec::new();
            let mut preparation_failed = false;
            for target in targets {
                let component_id = components[&target.id].clone();
                attempted_ids.insert(target.id.clone());
                attempted += 1;
                let sequence = match DriverRepository::new(db.conn())
                    .record_component_attempt_started_with_sources(
                        &session_id,
                        &target.id.to_string(),
                        target.path.as_str(),
                        None,
                        review_configuration_source,
                        execution_context_configuration_source,
                        (parallelism > 1).then_some(component_id.as_str()),
                        (parallelism > 1)
                            .then(|| {
                                component_worktrees
                                    .get(&component_id)
                                    .map(|lease| lease.path().to_string_lossy())
                            })
                            .flatten()
                            .as_deref(),
                        (parallelism > 1)
                            .then(|| {
                                component_worktrees
                                    .get(&component_id)
                                    .map(crate::worktree::WorktreeLease::branch)
                            })
                            .flatten(),
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
                let use_worktree = parallelism > 1;
                let worktree = if use_worktree {
                    if let Some(lease) = component_worktrees.remove(&component_id) {
                        Some(lease)
                    } else {
                        match crate::worktree::WorktreeLease::create_component(
                            &repository.worktree,
                            &paths.state_dir,
                            &session_id,
                            &component_id,
                            (!config.driver.worktree_root.is_empty())
                                .then(|| Path::new(&config.driver.worktree_root)),
                        ) {
                            Ok(lease) => {
                                let path = lease.path().to_string_lossy().into_owned();
                                let branch = lease.branch().to_owned();
                                match DriverRepository::new(db.conn()).record_attempt_workspace(
                                    &session_id,
                                    sequence,
                                    &component_id,
                                    &path,
                                    &branch,
                                ) {
                                    Ok(()) => Some(lease),
                                    Err(error) => {
                                        let _ = DriverRepository::new(db.conn())
                                            .record_attempt_finished(
                                                &session_id,
                                                sequence,
                                                "retained",
                                                Some("workspace_evidence_failed"),
                                                None,
                                                None,
                                            );
                                        eprintln!(
                                            "drive: cannot persist component workspace evidence: {error}"
                                        );
                                        stopped_components.insert(component_id.clone());
                                        continue;
                                    }
                                }
                            }
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
                                stopped_components.insert(component_id.clone());
                                continue;
                            }
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
                    component_id,
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
                    component_id,
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
                            component_id,
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
                    component_id,
                ) = joined.unwrap();
                let heartbeat_failed = worktree_heartbeat
                    .as_ref()
                    .is_some_and(crate::worktree::WorktreeHeartbeatGuard::failed);
                drop(worktree_heartbeat);
                let (mut result, trace) = match execution {
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
                if let Err(crate::run::RunError::HumanReviewRequired {
                    result: implementation,
                    cycle,
                    prd_id,
                }) = &result
                {
                    if let Some(policy) = delivery_policy.filter(|policy| {
                        policy.mode == familiar_ai_core::DeliveryMode::PocSelfApproval
                    }) {
                        let warrant = policy.poc_warrant.as_ref().expect("validated PoC warrant");
                        let unexpired = chrono::DateTime::parse_from_rfc3339(&warrant.expires_at)
                            .is_ok_and(|expiry| expiry > chrono::Utc::now());
                        if unexpired && poc_risks_accepted < warrant.max_prds {
                            let accepted_implementation = implementation.as_ref().clone();
                            let findings=serde_json::json!({"review":cycle.review_result.as_ref().map(|review| &review.findings),"scope":cycle.scope_evaluations.iter().flat_map(|evaluation| &evaluation.findings).collect::<Vec<_>>()}).to_string();
                            let stops = serde_json::to_string(&cycle.stop_reasons)
                                .unwrap_or_else(|_| "[]".into());
                            let acceptance_root = worktree
                                .as_ref()
                                .map_or(repository.worktree.as_path(), |lease| lease.path());
                            match crate::run::accept_review_risk(
                                acceptance_root,
                                prd_id,
                                &warrant.actor,
                                cycle,
                                config,
                                paths,
                            ) {
                                Ok(()) => {
                                    poc_risks_accepted = poc_risks_accepted.saturating_add(1);
                                    let warrant_json = serde_json::to_string(warrant)
                                        .unwrap_or_else(|_| "{}".into());
                                    let _ = DeliveryRepository::new(db.conn())
                                        .record_authority_decision(
                                            &format!("poc-risk:{session_id}:{prd_id}"),
                                            &repository.key,
                                            &session_id,
                                            prd_id,
                                            "poc_self_approval",
                                            &warrant.actor,
                                            "accepted_reviewed_risk",
                                            Some(&warrant.assurance_label),
                                            &findings,
                                            &stops,
                                            Some(&warrant_json),
                                            poc_risks_accepted,
                                        );
                                    result = Ok(crate::run::RunWorkflowResult {
                                        implementation: accepted_implementation,
                                    });
                                }
                                Err(error) => eprintln!(
                                    "drive: PoC risk acceptance failed for {prd_id}: {error}"
                                ),
                            }
                        }
                    }
                }
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
                let tokens = trace
                    .execution_id
                    .as_deref()
                    .and_then(|id| attempt_tokens(&db, id));
                if let Some(value) = tokens {
                    known_tokens = known_tokens.saturating_add(value);
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
                if result.is_err() {
                    stopped_components.insert(component_id.clone());
                }
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
                    if result.is_ok()
                        && delivery_policy.is_some_and(|policy| {
                            policy.mode != familiar_ai_core::DeliveryMode::Disabled
                        })
                    {
                        let policy = delivery_policy.expect("checked delivery policy");
                        if delivered >= policy.max_deliveries_per_session {
                            batch_stop.get_or_insert(DriveTermination::BudgetDeliveriesExhausted);
                        } else {
                            let delivery_heartbeat = lease.start_heartbeat(Duration::from_secs(
                                config.daemon.heartbeat_interval_secs.max(1),
                            ));
                            let delivery_result =
                                crate::delivery::deliver(lease.ownership_path(), policy);
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
                if let Some(lease) = worktree {
                    component_worktrees.insert(component_id, lease);
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
                if warrant.max_tokens > 0 && tokens.is_none() {
                    batch_stop.get_or_insert(DriveTermination::UnclassifiedResult);
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
        known_tokens,
    })
}

fn component_parallelism(config: &Config, warrant: &DriveWarrant) -> usize {
    if warrant.max_cost_microusd > 0 || warrant.max_tokens > 0 {
        1
    } else {
        config.driver.max_parallel_components
    }
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

fn attempt_tokens(db: &Database, execution_id: &str) -> Option<u64> {
    ExecutionHistoryRepository::new(db.conn())
        .get(execution_id)
        .ok()
        .flatten()?
        .total_tokens
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
            max_tokens: 700,
            max_duration_ms: 1_000,
        };
        // Lower values win.
        let tightened = configured.tightened_by(Some(3), Some(100), Some(250));
        assert_eq!(
            tightened,
            DriveWarrant {
                max_prds: 3,
                max_cost_microusd: 100,
                max_tokens: 700,
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
            max_tokens: 200,
            max_duration_ms: 1_000,
        };
        assert_eq!(warrant.exhausted(0, 0, 0, 0), None);
        assert_eq!(
            warrant.exhausted(2, 0, 0, 0),
            Some(DriveTermination::BudgetPrdsExhausted)
        );
        assert_eq!(
            warrant.exhausted(0, 100, 0, 0),
            Some(DriveTermination::BudgetCostExhausted)
        );
        assert_eq!(
            warrant.exhausted(0, 0, 0, 1_000),
            Some(DriveTermination::BudgetDurationExhausted)
        );
        // Ceilings set to zero never trigger.
        let only_prds = DriveWarrant {
            max_prds: 1,
            ..DriveWarrant::default()
        };
        assert_eq!(only_prds.exhausted(0, u64::MAX, u64::MAX, u64::MAX), None);
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
            max_tokens: 0,
            max_duration_ms: 60_000,
        };
        assert_eq!(
            warrant.as_json(),
            r#"{"max_prds":4,"max_cost_microusd":0,"max_tokens":0,"max_duration_ms":60000}"#
        );
    }

    #[test]
    fn dependency_partition_is_weakly_connected_and_stable() {
        fn prd(number: u64, dependencies: &[u64]) -> DiscoveredPrd {
            DiscoveredPrd {
                id: PrdId::numbered_slug(number, None, 3),
                number,
                path: familiar_ai_core::RepositoryPath::new(format!(
                    "docs/prds/PRD-{number:03}.md"
                ))
                .unwrap(),
                location: familiar_ai_core::PrdLocation::Active,
                title: format!("PRD {number}"),
                dependencies: dependencies
                    .iter()
                    .copied()
                    .map(|number| PrdId::numbered_slug(number, None, 3))
                    .collect(),
                metadata: Default::default(),
                content_hash: format!("hash-{number}"),
            }
        }
        let components = dependency_components(&[
            prd(1, &[]),
            prd(2, &[1]),
            prd(3, &[2]),
            prd(10, &[]),
            prd(11, &[10]),
        ]);
        assert_eq!(components[&PrdId::new(1)], "component-PRD-001");
        assert_eq!(components[&PrdId::new(3)], "component-PRD-001");
        assert_eq!(components[&PrdId::new(10)], "component-PRD-010");
        assert_eq!(components[&PrdId::new(11)], "component-PRD-010");
    }

    #[test]
    fn component_parallelism_preserves_serial_and_bounded_budget_modes() {
        let mut config = Config::default();
        config.driver.max_concurrency = 15;
        config.driver.max_parallel_components = 1;
        let mut warrant = DriveWarrant {
            max_prds: 15,
            max_cost_microusd: 0,
            max_tokens: 0,
            max_duration_ms: 60_000,
        };
        assert_eq!(component_parallelism(&config, &warrant), 1);
        config.driver.max_parallel_components = 7;
        assert_eq!(component_parallelism(&config, &warrant), 7);
        warrant.max_tokens = 1;
        assert_eq!(component_parallelism(&config, &warrant), 1);
        warrant.max_tokens = 0;
        warrant.max_cost_microusd = 1;
        assert_eq!(component_parallelism(&config, &warrant), 1);
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
