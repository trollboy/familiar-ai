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

use crate::run::{
    execute_with_config_tracked_from_preflighted_with_route_context,
    execute_with_config_tracked_from_preflighted_with_route_context_and_timeout,
    next_implementation_worker, resolved_worker_plan, AgentSet, RouteContext,
};
use familiar_ai_agent::WorkerStage;

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
/// least one must be finite for a session to start. An allowlist, when
/// present, is the immutable approved PRD set: selection may never leave it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveWarrant {
    pub max_prds: u64,
    pub max_cost_microusd: u64,
    pub max_tokens: u64,
    pub max_duration_ms: u64,
    pub prd_allowlist: Option<BTreeSet<PrdId>>,
}

impl DriveWarrant {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_prds: config.driver.max_prds_per_session,
            max_cost_microusd: config.driver.max_session_cost_microusd,
            max_tokens: config.driver.max_session_tokens,
            max_duration_ms: config.driver.max_session_duration_ms,
            prd_allowlist: None,
        }
    }

    /// Bind the warrant to an explicit approved PRD set. An allowlist may only
    /// ever shrink an inherited one — widening is refused, like every other
    /// warrant loosening.
    pub fn with_prd_allowlist(mut self, prds: BTreeSet<PrdId>) -> Result<Self, String> {
        if prds.is_empty() {
            return Err("an explicit PRD allowlist must not be empty".into());
        }
        if let Some(existing) = &self.prd_allowlist {
            if !prds.is_subset(existing) {
                return Err("a PRD allowlist may only tighten the inherited set".into());
            }
        }
        self.prd_allowlist = Some(prds);
        Ok(self)
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
            prd_allowlist: self.prd_allowlist,
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
        let allowlist = self
            .prd_allowlist
            .as_ref()
            .map(|prds| {
                let rendered = prds
                    .iter()
                    .map(|id| format!("\"{id}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(",\"prd_allowlist\":[{rendered}]")
            })
            .unwrap_or_default();
        format!(
            "{{\"max_prds\":{},\"max_cost_microusd\":{},\"max_tokens\":{},\"max_duration_ms\":{}{allowlist}}}",
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

/// One selection or deferral decision for a ready PRD, persisted durably so an
/// operator can always answer "why did/didn't this run?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionDecision {
    pub prd_id: PrdId,
    pub decision: &'static str,
    pub detail: String,
}

/// The mutable expected-file scope of one PRD under the closed Expected Files
/// grammar (exact file, or directory prefix from `dir/` / `dir/**`).
fn prd_scope(
    repository_worktree: &Path,
    prd: &DiscoveredPrd,
) -> Result<Vec<(String, familiar_ai_review::ExpectedMatchKind)>, String> {
    use familiar_ai_review::ExpectedMatchKind;
    if prd.metadata.contract_version == Some(1) {
        prd.metadata
            .expected_files
            .iter()
            .map(|expression| {
                familiar_ai_review::normalize_scope_path(expression)
                    .map_err(|rule| format!("{expression}: {rule}"))
            })
            .collect()
    } else {
        let path = repository_worktree.join(prd.path.as_str());
        let content = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        Ok(familiar_ai_review::parse_expected_files(&content)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|entry| (entry.normalized, entry.match_kind))
            .collect::<Vec<(String, ExpectedMatchKind)>>())
    }
}

/// Whether two normalized scope entries can authorize writes to one path.
fn scope_entries_overlap(
    (left, left_kind): &(String, familiar_ai_review::ExpectedMatchKind),
    (right, right_kind): &(String, familiar_ai_review::ExpectedMatchKind),
) -> bool {
    use familiar_ai_review::ExpectedMatchKind::*;
    match (left_kind, right_kind) {
        (ExactFile, ExactFile) => left == right,
        (ExactFile, Directory) => left.starts_with(right.as_str()),
        (Directory, ExactFile) => right.starts_with(left.as_str()),
        (Directory, Directory) => {
            left.starts_with(right.as_str()) || right.starts_with(left.as_str())
        }
    }
}

/// The first overlapping pattern pair between two PRD scopes, if any.
fn scope_overlap(
    left: &[(String, familiar_ai_review::ExpectedMatchKind)],
    right: &[(String, familiar_ai_review::ExpectedMatchKind)],
) -> Option<(String, String)> {
    left.iter().find_map(|a| {
        right
            .iter()
            .find(|b| scope_entries_overlap(a, b))
            .map(|b| (a.0.clone(), b.0.clone()))
    })
}

/// Schedule from the current ready set. Dependencies are admission gates, not
/// mutual-exclusion edges: every pending PRD whose dependencies are completed
/// is admitted up to `limit`, and two ready PRDs serialize only for an
/// overlapping mutable expected-file scope. A PRD completed earlier in this
/// session whose work still sits undelivered in an isolated worktree defers
/// its dependents — a fresh worktree branches from the main HEAD and would
/// not contain that work.
#[allow(clippy::too_many_arguments)]
fn select_batch(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    db: &mut Database,
    attempted: &BTreeSet<PrdId>,
    allowlist: Option<&BTreeSet<PrdId>>,
    session_undelivered: &BTreeSet<PrdId>,
    limit: usize,
    decisions: &mut Vec<SelectionDecision>,
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
    let mut selected: Vec<DiscoveredPrd> = Vec::new();
    let mut selected_scopes: Vec<(PrdId, Vec<(String, familiar_ai_review::ExpectedMatchKind)>)> =
        Vec::new();
    let mut selected_resources: Vec<(PrdId, Vec<String>)> = Vec::new();
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
        if entry.status != BacklogStatus::Pending || !dependencies_met {
            continue;
        }
        if let Some(allowlist) = allowlist {
            if !allowlist.contains(&entry.prd.id) {
                decisions.push(SelectionDecision {
                    prd_id: entry.prd.id.clone(),
                    decision: "excluded_allowlist",
                    detail: "ready but outside the session's approved PRD set".into(),
                });
                continue;
            }
        }
        if let Some(undelivered) = entry
            .prd
            .dependencies
            .iter()
            .find(|id| session_undelivered.contains(id))
        {
            decisions.push(SelectionDecision {
                prd_id: entry.prd.id.clone(),
                decision: "deferred_dependency_undelivered",
                detail: format!(
                    "dependency {undelivered} completed this session in an isolated worktree not yet delivered to the base branch"
                ),
            });
            continue;
        }
        if selected.len() >= limit.max(1) {
            decisions.push(SelectionDecision {
                prd_id: entry.prd.id.clone(),
                decision: "deferred_width",
                detail: format!(
                    "ready, but the warrant width of {} is exhausted",
                    limit.max(1)
                ),
            });
            continue;
        }
        if let Some((holder, resource)) = selected_resources.iter().find_map(|(id, held)| {
            entry
                .prd
                .metadata
                .resources
                .iter()
                .find(|resource| held.contains(resource))
                .map(|resource| (id.clone(), resource.clone()))
        }) {
            decisions.push(SelectionDecision {
                prd_id: entry.prd.id.clone(),
                decision: "deferred_resource",
                detail: format!("declared resource '{resource}' is held by {holder}"),
            });
            continue;
        }
        let scope = match prd_scope(&repository.worktree, &entry.prd) {
            Ok(scope) => scope,
            Err(error) => {
                decisions.push(SelectionDecision {
                    prd_id: entry.prd.id.clone(),
                    decision: "deferred_scope_unavailable",
                    detail: format!("expected-file scope cannot be resolved: {error}"),
                });
                continue;
            }
        };
        if let Some((holder, (left, right))) = selected_scopes
            .iter()
            .find_map(|(id, held)| scope_overlap(&scope, held).map(|pair| (id.clone(), pair)))
        {
            decisions.push(SelectionDecision {
                prd_id: entry.prd.id.clone(),
                decision: "deferred_scope_overlap",
                detail: format!("scope '{left}' overlaps '{right}' held by {holder}"),
            });
            continue;
        }
        decisions.push(SelectionDecision {
            prd_id: entry.prd.id.clone(),
            decision: "ready_selected",
            detail: String::new(),
        });
        selected_scopes.push((entry.prd.id.clone(), scope));
        selected_resources.push((entry.prd.id.clone(), entry.prd.metadata.resources.clone()));
        selected.push(entry.prd);
    }
    if selected.is_empty() {
        Ok(Selection::NothingEligible)
    } else {
        Ok(Selection::Eligible(selected))
    }
}

/// Deterministically partition the validated, profile-neutral dependency graph
/// into weakly connected components. Component identity is its least PRD id.
/// Since PRD-065 this is a reporting label only — admission is decided by the
/// ready set in `select_batch`, never by component membership.
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

fn route_context(repository: &Path, prd: &DiscoveredPrd) -> Result<RouteContext, DriveError> {
    let expected_file_count = if prd.metadata.contract_version == Some(1) {
        prd.metadata.expected_files.len() as u64
    } else {
        let path = repository.join(prd.path.as_str());
        let content = std::fs::read_to_string(&path).map_err(|error| {
            DriveError::Config(format!(
                "cannot read routing scope from {}: {error}",
                path.display()
            ))
        })?;
        familiar_ai_review::parse_expected_files(&content)
            .map_err(|error| {
                DriveError::Config(format!(
                    "cannot parse routing scope from {}: {error}",
                    path.display()
                ))
            })?
            .len() as u64
    };
    Ok(RouteContext {
        risk_classes: prd.metadata.risk_classes.clone(),
        expected_file_count,
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
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| DriveError::Config(error.to_string()))?;
    let delivery_policy = repository_config.delivery.as_ref();
    if delivery_policy.is_some_and(|policy| policy.mode != familiar_ai_core::DeliveryMode::Disabled)
        && parallelism == 1
        && !config.driver.isolated_worktrees
    {
        return Err(DriveError::Config(
            "delivery requires driver.isolated_worktrees=true".into(),
        ));
    }
    let effective = config
        .effective_execution(&repository.worktree)
        .map_err(|error| DriveError::Config(error.to_string()))?;
    let review_configuration_source = effective.review_source.as_str();
    let execution_context_configuration_source = effective.execution_context_source.as_str();
    let mut effective_config = config.clone();
    effective_config.review = effective.review;
    effective_config.execution_context = effective.execution_context;
    // A drive session is pinned to one repository: other repositories'
    // entries are dropped so isolated worker paths can never trigger a
    // second policy lookup. The pinned repository's own entry must survive —
    // repository-scoped config that effective_execution does not fold (the
    // risk vocabulary) is resolved again during per-attempt discovery.
    effective_config.repositories.retain(|path, _| {
        Path::new(path)
            .canonicalize()
            .map(|canonical| canonical == repository.worktree)
            .unwrap_or(false)
    });
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
    // PRDs completed this session whose work sits in an isolated worktree not
    // yet delivered to the base branch: their dependents must not start from a
    // fresh worktree that lacks that work.
    let mut session_undelivered = BTreeSet::<PrdId>::new();
    // Each (prd, decision) pair is persisted once per session, not once per
    // scheduling pass.
    let mut recorded_decisions = BTreeSet::<(PrdId, &'static str)>::new();
    let mut width_reported = false;

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
            let mut decisions = Vec::new();
            let selection = select_batch(
                &repository,
                &discovered,
                &mut db,
                &attempted_ids,
                warrant.prd_allowlist.as_ref(),
                &session_undelivered,
                remaining,
                &mut decisions,
            )?;
            let mut decision_storage_failed = false;
            for decision in &decisions {
                if !recorded_decisions.insert((decision.prd_id.clone(), decision.decision)) {
                    continue;
                }
                if decision.decision != "ready_selected" {
                    eprintln!(
                        "drive: {} {} {}",
                        decision.decision, decision.prd_id, decision.detail
                    );
                }
                if let Err(error) = DriverRepository::new(db.conn()).record_selection_decision(
                    &session_id,
                    &decision.prd_id.to_string(),
                    decision.decision,
                    &decision.detail,
                ) {
                    eprintln!("drive: cannot persist selection decision: {error}");
                    decision_storage_failed = true;
                    break;
                }
            }
            if decision_storage_failed {
                break DriveTermination::StorageFailure;
            }
            let targets = match selection {
                Selection::Eligible(prds) => prds,
                Selection::BacklogEmpty => break DriveTermination::BacklogEmpty,
                Selection::NothingEligible => break DriveTermination::NothingEligible,
            };
            if !width_reported {
                width_reported = true;
                if targets.len() < remaining {
                    let detail = format!(
                        "achievable_width={} requested_width={remaining}",
                        targets.len()
                    );
                    eprintln!("drive: {detail}");
                    if let Err(error) =
                        DriverRepository::new(db.conn()).record_session_detail(&session_id, &detail)
                    {
                        eprintln!("drive: cannot persist width report: {error}");
                        break DriveTermination::StorageFailure;
                    }
                }
            }
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
                        None,
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
                let route_context = route_context(&repository.worktree, &target)?;
                let escalation_worker =
                    next_implementation_worker(&execution_config, &route_context)
                        .map_err(DriveError::Config)?;
                let selections = if execution_config.worker_registry.is_some() {
                    let (_, _, selections) =
                        resolved_worker_plan(&execution_config, &route_context)
                            .map_err(DriveError::Config)?;
                    Some(selections)
                } else {
                    None
                };
                if let (Some(registry), Some(selections)) =
                    (&mut execution_config.worker_registry, selections)
                {
                    let routing = &mut registry.routing;
                    routing.implementation_pin = selections
                        .iter()
                        .find(|record| record.stage == WorkerStage::Implementation)
                        .map(|record| record.selected_worker.clone());
                    routing.remediation_pin = selections
                        .iter()
                        .find(|record| record.stage == WorkerStage::Remediation)
                        .map(|record| record.selected_worker.clone());
                }
                // A potentially escalatable attempt must not dirty the base
                // tree: its retry is required to start from this same clean
                // original state, not from the cheap worker's failed tree.
                let use_worktree = parallelism > 1 || escalation_worker.is_some();
                let worktree = if use_worktree {
                    match crate::worktree::WorktreeLease::create_component(
                        &repository.worktree,
                        &paths.state_dir,
                        &session_id,
                        &target.id.to_string(),
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
                                    eprintln!("drive: cannot persist workspace evidence: {error}");
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
                            continue;
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
                    route_context,
                    worktree,
                    worktree_heartbeat,
                    component_id,
                    escalation_worker,
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
                    route_context,
                    worktree,
                    worktree_heartbeat,
                    component_id,
                    escalation_worker,
                ) in jobs
                {
                    handles.push(scope.spawn(move || {
                        let attempt_timer = Instant::now();
                        let prd_path = execution_root.join(target.path.as_str());
                        let execution =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                execute_with_config_tracked_from_preflighted_with_route_context(
                                    &execution_root,
                                    &prd_path,
                                    agents,
                                    &execution_config,
                                    paths,
                                    true,
                                    Some(route_context.clone()),
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
                            escalation_worker,
                            route_context,
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
                    escalation_worker,
                    route_context,
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
                if result.is_ok() && worktree.is_some() {
                    // Completed in an isolated worktree: dependents defer until
                    // this work is delivered to the base branch.
                    session_undelivered.insert(target.id.clone());
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
                                    session_undelivered.remove(&target.id);
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
                // The lease is dropped, not retired: the worktree and its
                // ownership record survive on disk as durable evidence.
                drop(worktree);
                let _ = component_id;
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
                if retained_reason == Some("verification_failed") {
                    if let Some((stronger_id, stronger_entry)) = escalation_worker {
                        let estimated_cost = config
                            .worker_registry
                            .as_ref()
                            .and_then(|registry| registry.workers.get(&stronger_id))
                            .map_or(0, |worker| worker.estimated_cost_microusd);
                        let elapsed = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        let duration_reservation = remaining_duration_ms(&warrant, elapsed)
                            .unwrap_or(stronger_entry.max_execution_duration_ms)
                            .max(stronger_entry.max_execution_duration_ms);
                        if escalation_admitted(
                            &warrant,
                            known_cost,
                            known_tokens,
                            elapsed,
                            estimated_cost,
                            config.driver.max_implementation_tokens,
                            duration_reservation,
                        ) && (warrant.max_cost_microusd == 0 || cost.is_some())
                            && (warrant.max_tokens == 0 || tokens.is_some())
                        {
                            let escalation_component =
                                format!("{component_id}-escalation-{sequence}");
                            match crate::worktree::WorktreeLease::create_component(
                                &repository.worktree,
                                &paths.state_dir,
                                &session_id,
                                &escalation_component,
                                (!config.driver.worktree_root.is_empty())
                                    .then(|| Path::new(&config.driver.worktree_root)),
                            ) {
                                Ok(mut escalation_tree) => {
                                    let escalation_root = escalation_tree.path().to_path_buf();
                                    let escalation_sequence = DriverRepository::new(db.conn())
                                        .record_escalated_attempt_started_with_sources(
                                            &session_id,
                                            &target.id.to_string(),
                                            target.path.as_str(),
                                            None,
                                            review_configuration_source,
                                            execution_context_configuration_source,
                                            Some(&escalation_component),
                                            Some(&escalation_root.to_string_lossy()),
                                            Some(escalation_tree.branch()),
                                            Some(sequence),
                                            Some("required_verification_failed"),
                                        );
                                    match escalation_sequence {
                                        Ok(escalation_sequence) => {
                                            let mut escalation_config = config.clone();
                                            if let Some(registry) =
                                                &mut escalation_config.worker_registry
                                            {
                                                registry.routing.implementation_pin =
                                                    Some(stronger_id.clone());
                                            }
                                            escalation_config.repositories.insert(
                                                escalation_root.display().to_string(),
                                                repository_config.clone(),
                                            );
                                            let escalation_timer = Instant::now();
                                            let escalation_timeout_ms = remaining_duration_ms(
                                                &warrant,
                                                timer.elapsed().as_millis().min(u64::MAX as u128)
                                                    as u64,
                                            );
                                            let (escalated, escalation_trace) =
                                                execute_with_config_tracked_from_preflighted_with_route_context_and_timeout(
                                                    &escalation_root,
                                                    &escalation_root.join(target.path.as_str()),
                                                    agents,
                                                    &escalation_config,
                                                    paths,
                                                    true,
                                                    Some(route_context.clone()),
                                                    escalation_timeout_ms,
                                                );
                                            let escalation_duration = escalation_timer
                                                .elapsed()
                                                .as_millis()
                                                .min(u64::MAX as u128)
                                                as u64;
                                            let escalation_cost = escalation_trace
                                                .execution_id
                                                .as_deref()
                                                .and_then(|id| attempt_cost(&db, id));
                                            let escalation_tokens = escalation_trace
                                                .execution_id
                                                .as_deref()
                                                .and_then(|id| attempt_tokens(&db, id));
                                            if let Some(value) = escalation_cost {
                                                known_cost = known_cost.saturating_add(value);
                                            }
                                            if let Some(value) = escalation_tokens {
                                                known_tokens = known_tokens.saturating_add(value);
                                            }
                                            let escalation_terminal = match &escalated {
                                                Ok(workflow) => Some(&workflow.implementation),
                                                Err(crate::run::RunError::Agent(error)) => {
                                                    Some(error.result())
                                                }
                                                Err(crate::run::RunError::Workflow {
                                                    result: Some(result),
                                                    ..
                                                }) => Some(result.as_ref()),
                                                _ => None,
                                            };
                                            let escalation_outcome = if escalated.is_ok() {
                                                "completed"
                                            } else {
                                                "retained"
                                            };
                                            let escalation_reason = if escalated.is_ok() {
                                                None
                                            } else {
                                                escalation_trace
                                                    .retained_reason
                                                    .or(Some("unclassified_result"))
                                            };
                                            let diagnostic = DriverRepository::new(db.conn())
                                                .record_attempt_diagnostics(
                                                    &session_id,
                                                    escalation_sequence,
                                                    escalation_trace.execution_id.as_deref(),
                                                    Some(stronger_entry.adapter.as_str()),
                                                    escalation_terminal
                                                        .and_then(|value| value.model.as_deref())
                                                        .or(stronger_entry.model.as_deref()),
                                                    escalation_terminal
                                                        .and_then(|value| value.exit_code),
                                                    escalation_terminal
                                                        .and_then(|value| value.signal),
                                                    escalation_outcome,
                                                )
                                                .and_then(|()| {
                                                    DriverRepository::new(db.conn())
                                                        .record_attempt_finished(
                                                            &session_id,
                                                            escalation_sequence,
                                                            escalation_outcome,
                                                            escalation_reason,
                                                            escalation_cost,
                                                            Some(escalation_duration),
                                                        )
                                                });
                                            if let Err(error) = diagnostic {
                                                eprintln!("drive: cannot persist escalated attempt: {error}");
                                                batch_stop = Some(DriveTermination::StorageFailure);
                                            } else if escalated.is_ok() {
                                                completed = completed.saturating_add(1);
                                                session_undelivered.insert(target.id.clone());
                                                let _ = escalation_tree
                                                    .mark_state("ready_for_delivery");
                                                if let Some(policy) =
                                                    delivery_policy.filter(|policy| {
                                                        policy.mode
                                                        != familiar_ai_core::DeliveryMode::Disabled
                                                    })
                                                {
                                                    if delivered
                                                        >= policy.max_deliveries_per_session
                                                    {
                                                        batch_stop = Some(
                                                            DriveTermination::BudgetDeliveriesExhausted,
                                                        );
                                                    } else {
                                                        match crate::delivery::deliver(
                                                            escalation_tree.ownership_path(),
                                                            policy,
                                                        ) {
                                                            Ok(delivery) => {
                                                                delivered =
                                                                    delivered.saturating_add(1);
                                                                session_undelivered
                                                                    .remove(&target.id);
                                                                let _ = escalation_tree
                                                                    .mark_state(&delivery.phase);
                                                            }
                                                            Err(error) => {
                                                                eprintln!("drive: escalated delivery blocked for {}: {error}", target.id);
                                                                batch_stop = Some(
                                                                    DriveTermination::DeliveryBlocked,
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            } else {
                                                let _ = escalation_tree.mark_state("retained");
                                            }
                                            eprintln!("drive: escalation {escalation_sequence} {} worker={stronger_id} outcome={escalation_outcome}", target.id);
                                        }
                                        Err(error) => {
                                            eprintln!("drive: cannot record escalation: {error}");
                                            batch_stop = Some(DriveTermination::StorageFailure);
                                        }
                                    }
                                }
                                Err(error) => eprintln!(
                                    "drive: escalation clean worktree unavailable: {error}"
                                ),
                            }
                        } else {
                            eprintln!("drive: escalation retained for {} because the remaining warrant cannot admit it", target.id);
                        }
                    }
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

fn escalation_admitted(
    warrant: &DriveWarrant,
    known_cost: u64,
    known_tokens: u64,
    elapsed_ms: u64,
    estimated_cost: u64,
    token_reservation: u64,
    duration_reservation_ms: u64,
) -> bool {
    (warrant.max_cost_microusd == 0
        || known_cost.saturating_add(estimated_cost) <= warrant.max_cost_microusd)
        && (warrant.max_tokens == 0
            || (token_reservation > 0
                && known_tokens.saturating_add(token_reservation) <= warrant.max_tokens))
        && (warrant.max_duration_ms == 0
            || (duration_reservation_ms > 0
                && elapsed_ms.saturating_add(duration_reservation_ms) <= warrant.max_duration_ms))
}

fn remaining_duration_ms(warrant: &DriveWarrant, elapsed_ms: u64) -> Option<u64> {
    (warrant.max_duration_ms > 0).then(|| warrant.max_duration_ms.saturating_sub(elapsed_ms))
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
    fn escalation_admission_reserves_every_finite_warrant() {
        let warrant = DriveWarrant {
            max_prds: 1,
            max_cost_microusd: 1_000,
            max_tokens: 500,
            max_duration_ms: 0,
            prd_allowlist: None,
        };
        assert!(escalation_admitted(&warrant, 200, 100, 10, 300, 200, 0));
        assert!(!escalation_admitted(&warrant, 800, 100, 10, 300, 200, 0));
        assert!(!escalation_admitted(&warrant, 200, 400, 10, 300, 200, 0));
        let duration_bounded = DriveWarrant {
            max_duration_ms: 1_000,
            ..warrant
        };
        assert!(escalation_admitted(&duration_bounded, 0, 0, 100, 1, 1, 900));
        assert!(!escalation_admitted(
            &duration_bounded,
            0,
            0,
            900,
            1,
            1,
            101
        ));
    }

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
            prd_allowlist: None,
        };
        // Lower values win.
        let tightened = configured
            .clone()
            .tightened_by(Some(3), Some(100), Some(250));
        assert_eq!(
            tightened,
            DriveWarrant {
                max_prds: 3,
                max_cost_microusd: 100,
                max_tokens: 700,
                max_duration_ms: 250,
                prd_allowlist: None,
            }
        );
        // Higher values are ignored — a flag can never loosen configuration.
        assert_eq!(
            configured
                .clone()
                .tightened_by(Some(99), Some(9_999), Some(9_999)),
            configured
        );
        // Absent flags leave configuration alone.
        assert_eq!(
            configured.clone().tightened_by(None, None, None),
            configured
        );
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
            prd_allowlist: None,
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
            prd_allowlist: None,
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
            prd_allowlist: None,
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

    fn contract_prd(
        number: u64,
        dependencies: &[u64],
        location: familiar_ai_core::PrdLocation,
        expected_files: &[&str],
    ) -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(number),
            number,
            path: familiar_ai_core::RepositoryPath::new(
                if location == familiar_ai_core::PrdLocation::Archived {
                    format!("docs/prds/done/PRD-{number:03}.md")
                } else {
                    format!("docs/prds/PRD-{number:03}.md")
                },
            )
            .unwrap(),
            location,
            title: format!("PRD {number}"),
            dependencies: dependencies.iter().copied().map(PrdId::new).collect(),
            metadata: familiar_ai_core::PrdMetadata {
                contract_version: Some(1),
                status: Some("ready".into()),
                expected_files: expected_files.iter().map(|s| s.to_string()).collect(),
                acceptance_criteria: vec!["x".into()],
                risk_classes: vec!["scheduling".into()],
                external_gates: Vec::new(),
                resources: Vec::new(),
            },
            content_hash: format!("hash-{number}"),
        }
    }

    fn test_repository() -> (tempfile::TempDir, RepositoryIdentity) {
        let temp = tempfile::tempdir().unwrap();
        let identity = RepositoryIdentity {
            worktree: temp.path().to_path_buf(),
            key: format!("{}/.git", temp.path().display()),
        };
        (temp, identity)
    }

    fn select(
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        db: &mut Database,
        allowlist: Option<&BTreeSet<PrdId>>,
        session_undelivered: &BTreeSet<PrdId>,
        limit: usize,
    ) -> (Vec<PrdId>, Vec<SelectionDecision>) {
        let mut decisions = Vec::new();
        let selected = match select_batch(
            repository,
            discovered,
            db,
            &BTreeSet::new(),
            allowlist,
            session_undelivered,
            limit,
            &mut decisions,
        )
        .unwrap()
        {
            Selection::Eligible(prds) => prds.into_iter().map(|prd| prd.id).collect(),
            _ => Vec::new(),
        };
        (selected, decisions)
    }

    /// PRD-065 defect-1 regression, component half: with the recorded Wave 1
    /// dependency edges but deliberately DISJOINT scopes, all six ready PRDs
    /// are admitted in one batch. This isolates removal of
    /// dependency-component serialization from scope conflicts; the honest
    /// recorded-scope behavior is pinned separately below (review F3).
    #[test]
    fn wave_one_edges_with_disjoint_scopes_select_all_six() {
        use familiar_ai_core::PrdLocation::{Active, Archived};
        let (_temp, repository) = test_repository();
        let discovered = vec![
            // Completed ancestors, exactly as recorded in docs/prds/done/.
            contract_prd(19, &[], Archived, &["a/19.rs"]),
            contract_prd(20, &[19], Archived, &["a/20.rs"]),
            contract_prd(21, &[], Archived, &["a/21.rs"]),
            contract_prd(24, &[], Archived, &["a/24.rs"]),
            contract_prd(26, &[], Archived, &["a/26.rs"]),
            contract_prd(28, &[24], Archived, &["a/28.rs"]),
            contract_prd(30, &[], Archived, &["a/30.rs"]),
            contract_prd(31, &[], Archived, &["a/31.rs"]),
            contract_prd(33, &[], Archived, &["a/33.rs"]),
            contract_prd(34, &[], Archived, &["a/34.rs"]),
            contract_prd(39, &[], Archived, &["a/39.rs"]),
            contract_prd(42, &[30], Archived, &["a/42.rs"]),
            contract_prd(43, &[], Archived, &["a/43.rs"]),
            // Wave 1 as it was pending on 2026-08-30, with its real
            // dependency edges and disjoint scopes.
            contract_prd(36, &[19, 26, 30, 31, 33, 39], Active, &["w/36/"]),
            contract_prd(37, &[21, 24, 33, 34, 39], Active, &["w/37/"]),
            contract_prd(44, &[39, 43], Active, &["w/44/"]),
            contract_prd(45, &[28, 42], Active, &["w/45/"]),
            contract_prd(46, &[43], Active, &["w/46/"]),
            contract_prd(47, &[31, 43], Active, &["w/47/"]),
        ];
        // The whole graph is one weakly connected component — the exact
        // shape that used to serialize.
        let components = dependency_components(&discovered);
        let distinct: BTreeSet<_> = components.values().collect();
        assert_eq!(distinct.len(), 1, "wave 1 shares one component");

        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &BTreeSet::new(), 6);
        assert_eq!(
            selected,
            vec![
                PrdId::new(36),
                PrdId::new(37),
                PrdId::new(44),
                PrdId::new(45),
                PrdId::new(46),
                PrdId::new(47)
            ],
            "every ready wave-1 PRD is admitted in one batch"
        );
        assert_eq!(
            decisions
                .iter()
                .filter(|decision| decision.decision == "ready_selected")
                .count(),
            6
        );
    }

    /// PRD-065 review F3: the recorded Wave 1 input with its REAL structured
    /// scopes (docs/prds/done/PRD-036..047). PRD-036 declares whole-crate
    /// directories (`crates/familiar-ai-core/src/`,
    /// `crates/familiar-ai-daemon/src/`, `crates/familiar-ai-daemon/tests/`,
    /// `docs/contracts/`) that contain every other wave-1 PRD's scope, so the
    /// honest achievable width of the recorded wave was ONE — the after-action
    /// report's "intended width of six" was never scope-safe. This pins that
    /// truth: 036 selects, the other five defer with recorded scope reasons
    /// naming the holder, and none of it is component serialization.
    #[test]
    fn recorded_wave_one_scopes_admit_only_one_prd_honestly() {
        use familiar_ai_core::PrdLocation::{Active, Archived};
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(19, &[], Archived, &["a/19.rs"]),
            contract_prd(20, &[19], Archived, &["a/20.rs"]),
            contract_prd(21, &[], Archived, &["a/21.rs"]),
            contract_prd(24, &[], Archived, &["a/24.rs"]),
            contract_prd(26, &[], Archived, &["a/26.rs"]),
            contract_prd(28, &[24], Archived, &["a/28.rs"]),
            contract_prd(30, &[], Archived, &["a/30.rs"]),
            contract_prd(31, &[], Archived, &["a/31.rs"]),
            contract_prd(33, &[], Archived, &["a/33.rs"]),
            contract_prd(34, &[], Archived, &["a/34.rs"]),
            contract_prd(39, &[], Archived, &["a/39.rs"]),
            contract_prd(42, &[30], Archived, &["a/42.rs"]),
            contract_prd(43, &[], Archived, &["a/43.rs"]),
            // The six wave-1 PRDs with their actual declared scopes, copied
            // from the archived documents' authoritative metadata.
            contract_prd(
                36,
                &[19, 26, 30, 31, 33, 39],
                Active,
                &[
                    "crates/familiar-ai-core/src/",
                    "crates/familiar-ai-daemon/src/",
                    "crates/familiar-ai-daemon/tests/",
                    "config/default.toml",
                    "docs/contracts/",
                ],
            ),
            contract_prd(
                37,
                &[21, 24, 33, 34, 39],
                Active,
                &[
                    "docs/security/",
                    "crates/familiar-ai-agent/tests/",
                    "crates/familiar-ai-review/tests/",
                    "crates/familiar-ai-daemon/tests/",
                    "crates/familiar-ai-storage/tests/",
                ],
            ),
            contract_prd(
                44,
                &[39, 43],
                Active,
                &[
                    "crates/familiar-ai-core/src/config.rs",
                    "crates/familiar-ai-daemon/src/drive.rs",
                    "crates/familiar-ai-daemon/src/run.rs",
                    "crates/familiar-ai-storage/migrations/",
                    "crates/familiar-ai-storage/src/repos/",
                    "crates/familiar-ai-daemon/tests/",
                ],
            ),
            contract_prd(
                45,
                &[28, 42],
                Active,
                &[
                    "crates/familiar-ai-core/src/config.rs",
                    "crates/familiar-ai-review/src/tier.rs",
                    "crates/familiar-ai-review/src/policy.rs",
                ],
            ),
            contract_prd(
                46,
                &[43],
                Active,
                &[
                    "crates/familiar-ai-daemon/src/drive.rs",
                    "crates/familiar-ai-daemon/src/run.rs",
                    "crates/familiar-ai-daemon/src/plan.rs",
                    "crates/familiar-ai-daemon/src/worktree.rs",
                    "crates/familiar-ai-review/src/coordinator.rs",
                    "crates/familiar-ai-core/src/backlog.rs",
                ],
            ),
            contract_prd(
                47,
                &[31, 43],
                Active,
                &[
                    "docs/contracts/providers.md",
                    "crates/familiar-ai-core/src/config.rs",
                    "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
                    "crates/familiar-ai-daemon/src/config_cli.rs",
                    "crates/familiar-ai-storage/migrations/",
                    "crates/familiar-ai-storage/src/repos/",
                    "crates/familiar-ai-daemon/tests/",
                ],
            ),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &BTreeSet::new(), 6);
        assert_eq!(
            selected,
            vec![PrdId::new(36)],
            "PRD-036's whole-crate scopes exclude every sibling"
        );
        for number in [37, 44, 45, 46, 47] {
            let deferred = decisions
                .iter()
                .find(|decision| decision.prd_id == PrdId::new(number))
                .unwrap_or_else(|| panic!("no decision for PRD-{number}"));
            assert_eq!(deferred.decision, "deferred_scope_overlap", "PRD-{number}");
            assert!(
                deferred.detail.contains("PRD-36"),
                "PRD-{number}: {}",
                deferred.detail
            );
        }
        // Spot-check one recorded reason names the actual overlapping pair.
        let deferred_44 = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(44))
            .unwrap();
        assert!(
            deferred_44.detail.contains("crates/familiar-ai-core/src/"),
            "{}",
            deferred_44.detail
        );
    }

    /// PRD-065 review F2: two ready PRDs with disjoint file scopes but one
    /// shared declared resource never run concurrently; a third PRD holding a
    /// different resource still admits.
    #[test]
    fn declared_resource_conflicts_serialize_with_recorded_reason() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let mut first = contract_prd(1, &[], Active, &["a.rs"]);
        first.metadata.resources = vec!["sqlite-db".into(), "gpu-0".into()];
        let mut second = contract_prd(2, &[], Active, &["b.rs"]);
        second.metadata.resources = vec!["sqlite-db".into()];
        let mut third = contract_prd(3, &[], Active, &["c.rs"]);
        third.metadata.resources = vec!["staging-deploy".into()];
        let discovered = vec![first, second, third];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &BTreeSet::new(), 6);
        assert_eq!(selected, vec![PrdId::new(1), PrdId::new(3)]);
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .unwrap();
        assert_eq!(deferred.decision, "deferred_resource");
        assert!(deferred.detail.contains("sqlite-db"), "{}", deferred.detail);
        assert!(deferred.detail.contains("PRD-1"), "{}", deferred.detail);
    }

    #[test]
    fn overlapping_scopes_serialize_with_recorded_reason() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Active, &["src/lib.rs", "src/a/"]),
            contract_prd(2, &[], Active, &["src/a/deep.rs"]),
            contract_prd(3, &[], Active, &["docs/other.md"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &BTreeSet::new(), 6);
        assert_eq!(selected, vec![PrdId::new(1), PrdId::new(3)]);
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .unwrap();
        assert_eq!(deferred.decision, "deferred_scope_overlap");
        assert!(deferred.detail.contains("src/a/"), "{}", deferred.detail);
        assert!(deferred.detail.contains("PRD-1"), "{}", deferred.detail);
    }

    #[test]
    fn allowlist_confines_selection_to_the_approved_set() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Active, &["a.rs"]),
            contract_prd(2, &[], Active, &["b.rs"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let allowlist: BTreeSet<PrdId> = [PrdId::new(2)].into_iter().collect();
        let (selected, decisions) = select(
            &repository,
            &discovered,
            &mut db,
            Some(&allowlist),
            &BTreeSet::new(),
            6,
        );
        assert_eq!(selected, vec![PrdId::new(2)]);
        let excluded = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(1))
            .unwrap();
        assert_eq!(excluded.decision, "excluded_allowlist");
    }

    #[test]
    fn undelivered_session_dependency_defers_dependent() {
        use familiar_ai_core::PrdLocation::{Active, Archived};
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Archived, &["a.rs"]),
            contract_prd(2, &[1], Active, &["b.rs"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let undelivered: BTreeSet<PrdId> = [PrdId::new(1)].into_iter().collect();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &undelivered, 6);
        assert!(selected.is_empty());
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .unwrap();
        assert_eq!(deferred.decision, "deferred_dependency_undelivered");
    }

    #[test]
    fn width_exhaustion_defers_with_recorded_reason() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Active, &["a.rs"]),
            contract_prd(2, &[], Active, &["b.rs"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) =
            select(&repository, &discovered, &mut db, None, &BTreeSet::new(), 1);
        assert_eq!(selected, vec![PrdId::new(1)]);
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .unwrap();
        assert_eq!(deferred.decision, "deferred_width");
    }

    #[test]
    fn allowlist_only_tightens_and_is_recorded_in_the_warrant_snapshot() {
        let warrant = DriveWarrant {
            max_prds: 4,
            ..DriveWarrant::default()
        };
        let narrow: BTreeSet<PrdId> = [PrdId::new(65)].into_iter().collect();
        let wide: BTreeSet<PrdId> = [PrdId::new(65), PrdId::new(66)].into_iter().collect();
        // Setting where none exists is allowed; widening is refused.
        let bound = warrant.clone().with_prd_allowlist(narrow.clone()).unwrap();
        assert!(bound.clone().with_prd_allowlist(wide.clone()).is_err());
        // Shrinking is allowed.
        let rebound = warrant
            .clone()
            .with_prd_allowlist(wide)
            .unwrap()
            .with_prd_allowlist(narrow)
            .unwrap();
        assert_eq!(rebound.prd_allowlist, bound.prd_allowlist);
        // An empty allowlist is not "no allowlist".
        assert!(warrant.with_prd_allowlist(BTreeSet::new()).is_err());
        assert_eq!(
            bound.as_json(),
            r#"{"max_prds":4,"max_cost_microusd":0,"max_tokens":0,"max_duration_ms":0,"prd_allowlist":["PRD-65"]}"#
        );
    }

    #[test]
    fn model_routes_name_registry_replacement() {
        let mut config = Config::default();
        config.driver.max_prds_per_session = 1;
        config.driver.model_routes = vec![familiar_ai_core::DriverModelRouteConfig {
            max_expected_files: 2,
            model: "local-narrow".into(),
        }];
        let error = config.driver.validate().unwrap_err();
        assert!(error.contains("driver.model_routes"));
        assert!(error.contains("worker_registry.routing.rules"));
    }
}
