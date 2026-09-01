//! Unattended backlog driver: run eligible PRDs one after another through the
//! unchanged single-PRD workflow until the backlog is empty, nothing is
//! eligible, or the budget warrant is exhausted — recording a durable account
//! of what ran, what stopped, why, and what it cost.
//!
//! The loop adds no execution semantics. Selection, admission, claim,
//! verification, review, and fail-closed completion all remain exactly as
//! `familiar-ai run` performs them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use std::time::Instant;

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, BacklogStatus, BacklogStatusStore,
    BacklogStoreError, Config, DiscoveredPrd, FilesystemBacklogDiscovery, GrantMode, PrdId,
    RepositoryIdentity, ReservationOwnerIdentity, ResourceRequest, ResourceType,
    UnknownConsumptionPolicy,
};
use familiar_ai_storage::{
    AcquireOutcome, Database, DeliveryRepository, DriverRepository, ExecutionHistoryRepository,
    OrchestrationRepository, ReservationRepository, SettlementObservation,
};

/// Shared application-service entry point used by CLI fallback and daemon
/// hosting. Argument parsing and rendering stay in adapters; routing and
/// warrant construction do not.
pub fn execute_configured(
    paths: &AppPaths,
    repository: &Path,
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
    max_parallel_components: Option<usize>,
    worktree_root: Option<&Path>,
    prd_flags: &[String],
) -> Result<DriveSummary, String> {
    let mut config = crate::config_cli::effective_config_for_repository(
        &crate::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        repository,
    )?;
    if let Some(value) = max_parallel_components {
        if value == 0 {
            return Err("--max-parallel-components must be positive".into());
        }
        config.driver.max_parallel_components = value;
    }
    if let Some(value) = worktree_root {
        config.driver.worktree_root = value.to_string_lossy().into_owned();
    }
    let (implementation_entry, reviewer_entry) = crate::run::resolved_agent_entries(&config)?;
    let implementation = crate::run::build_agent(&implementation_entry);
    let reviewer = crate::run::build_agent(&reviewer_entry);
    let remediation = crate::run::build_agent(&crate::run::resolved_remediation_entry(&config)?);
    let mut warrant = DriveWarrant::from_config(&config).tightened_by(
        max_prds,
        max_cost_microusd,
        max_duration_ms,
    );
    if !prd_flags.is_empty() {
        let allowlist = prd_flags
            .iter()
            .map(|value| parse_prd_flag(value))
            .collect::<Result<BTreeSet<_>, _>>()?;
        warrant = warrant.with_prd_allowlist(allowlist)?;
    }
    drive(
        &crate::run::AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        paths,
        warrant,
    )
    .map_err(|error| error.to_string())
}

pub fn parse_prd_flag(value: &str) -> Result<PrdId, String> {
    let trimmed = value.trim();
    let body = trimmed
        .strip_prefix("PRD-")
        .or_else(|| trimmed.strip_prefix("prd-"))
        .unwrap_or(trimmed);
    let (digits, suffix) = match body.strip_suffix(|c: char| c.is_ascii_lowercase()) {
        Some(prefix) => (prefix, body.chars().last()),
        None => (body, None),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid --prd value '{value}': expected PRD-<number> with an optional lowercase suffix"
        ));
    }
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("invalid --prd value '{value}': number out of range"))?;
    Ok(PrdId::with_suffix(number, suffix))
}

/// Materialize a reviewed candidate as a real two-parent merge commit on the
/// persisted integration revision. `git merge-tree --write-tree` performs the
/// merge without moving the user's checkout; conflicts are returned for the
/// bounded remediation path.
pub fn merge_candidate(
    repository: &Path,
    integrated: &str,
    candidate: &str,
) -> Result<String, String> {
    let tree = Command::new("git")
        .args(["merge-tree", "--write-tree", integrated, candidate])
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !tree.status.success() {
        return Err(format!(
            "integration conflict: {}",
            String::from_utf8_lossy(&tree.stderr).trim()
        ));
    }
    let tree = String::from_utf8_lossy(&tree.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if tree.is_empty() {
        return Err("integration produced no tree".into());
    }
    let commit = Command::new("git")
        .args([
            "commit-tree",
            &tree,
            "-p",
            integrated,
            "-p",
            candidate,
            "-m",
            "familiar: integrate reviewed candidate",
        ])
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !commit.status.success() {
        return Err(format!(
            "cannot commit integration: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&commit.stdout).trim().to_owned())
}

/// Continue a final human scope approval through the same durable landing
/// boundary as an unattended worker. Integration revision advancement,
/// candidate rebinding, checkpoint completion, and backlog completion are one
/// transaction; none becomes visible without all the others.
pub fn continue_scope_approved_candidate(
    db: &mut Database,
    repository: &RepositoryIdentity,
    target: &DiscoveredPrd,
    config: &Config,
) -> Result<(), String> {
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, &target.id.to_string())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no checkpoint for {}", target.id))?;
    if checkpoint.phase != "reviewed" {
        return Ok(());
    }
    let (session_id, sequence, prior): (String, i64, String) = db
        .conn()
        .query_row(
            "SELECT a.session_id,a.sequence,s.integration_revision FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id WHERE s.repository_key=?1 AND a.prd_id=?2 AND a.integrated_at IS NULL ORDER BY a.started_at DESC,a.sequence DESC LIMIT 1",
            rusqlite::params![repository.key, target.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| error.to_string())?;
    let checkpoint_worktree = Path::new(&checkpoint.worktree_path);
    if !git_output(checkpoint_worktree, &["status", "--porcelain"])?.is_empty() {
        git_output(checkpoint_worktree, &["add", "-A"])?;
        git_output(
            checkpoint_worktree,
            &[
                "commit",
                "-m",
                &format!("familiar: implement {}", target.id),
            ],
        )?;
    }
    let candidate = git_output(checkpoint_worktree, &["rev-parse", "HEAD"])?;
    let merged = merge_candidate(&repository.worktree, &prior, &candidate)?;
    let rebound_diff = Command::new("git")
        .args(["diff", "--binary", &prior, &merged])
        .current_dir(&repository.worktree)
        .output()
        .map_err(|error| error.to_string())?;
    if !rebound_diff.status.success() {
        return Err("cannot compute integrated candidate binding".into());
    }
    let rebound_hash = familiar_ai_review::content_hash(&rebound_diff.stdout);
    let execution_id = checkpoint
        .execution_id
        .as_deref()
        .ok_or_else(|| "scope-approved checkpoint has no execution id".to_string())?;
    let actor = format!("system:familiar-ai-run:{execution_id}");
    let required = config
        .review
        .verification
        .iter()
        .filter(|check| check.required)
        .map(|check| check.check_id.clone())
        .collect::<Vec<_>>();
    let now = chrono::Utc::now().to_rfc3339();
    familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
        .complete_run_with(
            repository,
            target,
            execution_id,
            &actor,
            &required,
            |tx| {
                let changed = tx.execute(
                    "UPDATE driver_sessions SET integration_revision=?1 WHERE session_id=?2 AND integration_revision=?3",
                    rusqlite::params![merged, session_id, prior],
                ).map_err(|error| BacklogStoreError::Storage(error.to_string()))?;
                if changed != 1 {
                    return Err(BacklogStoreError::Storage("integration revision changed during scope-decision landing".into()));
                }
                let changed = tx.execute(
                    "UPDATE driver_attempts SET candidate_revision=?1,integrated_at=?2,last_durable_phase='integrated' WHERE session_id=?3 AND sequence=?4 AND integrated_at IS NULL",
                    rusqlite::params![merged, now, session_id, sequence],
                ).map_err(|error| BacklogStoreError::Storage(error.to_string()))?;
                if changed != 1 {
                    return Err(BacklogStoreError::Storage("scope-decision attempt is missing or already integrated".into()));
                }
                let changed = tx.execute(
                    "UPDATE execution_checkpoints SET phase='completed',diff_hash=?1,approved_diff_hash=CASE WHEN approved_diff_hash=?2 THEN ?1 ELSE approved_diff_hash END,approved_commit=?3,base_revision=?3,invalid_reason=NULL,updated_at=?4 WHERE checkpoint_id=?5 AND diff_hash=?2",
                    rusqlite::params![rebound_hash, checkpoint.diff_hash, merged, now, checkpoint.checkpoint_id],
                ).map_err(|error| BacklogStoreError::Storage(error.to_string()))?;
                if changed != 1 {
                    return Err(BacklogStoreError::Storage("scope-decision candidate binding changed during landing".into()));
                }
                tx.execute(
                    "UPDATE scope_decisions SET candidate_hash=?1 WHERE checkpoint_id=?2",
                    rusqlite::params![rebound_hash, checkpoint.checkpoint_id],
                ).map_err(|error| BacklogStoreError::Storage(error.to_string()))?;
                Ok(())
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

struct ProgressGuard {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn durable_attempt_phase(database_path: &Path, session_id: &str, sequence: i64) -> String {
    Database::open(database_path)
        .ok()
        .and_then(|database| {
            database
                .conn()
                .query_row(
                    "SELECT last_durable_phase FROM driver_attempts WHERE session_id=?1 AND sequence=?2",
                    rusqlite::params![session_id, sequence],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| "unknown".into())
}

impl ProgressGuard {
    fn start(
        prd: String,
        stage: &'static str,
        database_path: std::path::PathBuf,
        session_id: String,
        sequence: i64,
        interval: Duration,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            while !flag.load(Ordering::Acquire) {
                std::thread::park_timeout(interval);
                if !flag.load(Ordering::Acquire) {
                    let last_transition =
                        durable_attempt_phase(&database_path, &session_id, sequence);
                    eprintln!("drive: progress prd={prd} stage={stage} elapsed_ms={} last_durable_phase={last_transition}",started.elapsed().as_millis());
                    let _ = std::io::stderr().flush();
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

use crate::run::{
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
    /// PRD-077: three identical deterministic terminal failures tripped the
    /// session circuit breaker; the session detail carries the recovery plan.
    DeterministicFailureCascade,
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
            Self::DeterministicFailureCascade => "deterministic_failure_cascade",
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
            "{{\"max_prds\":{},\"max_cost_nanousd\":{},\"max_uncached_tokens\":{},\"max_duration_ms\":{}{allowlist}}}",
            self.max_prds,
            self.max_cost_microusd.saturating_mul(1_000),
            self.max_tokens,
            self.max_duration_ms
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidthReport {
    pub graph_width: usize,
    pub achievable_width: usize,
    pub conflicts: Vec<(PrdId, PrdId, String)>,
}

/// Compute the maximum simultaneously admissible subset using the exact
/// scope/resource authority used by runtime selection.
pub fn achievable_width(repository: &Path, prds: &[DiscoveredPrd]) -> Result<WidthReport, String> {
    let scopes = prds
        .iter()
        .map(|p| prd_scope(repository, p))
        .collect::<Result<Vec<_>, _>>()?;
    let mut conflicts = Vec::new();
    let mut edges = vec![vec![false; prds.len()]; prds.len()];
    for i in 0..prds.len() {
        for j in i + 1..prds.len() {
            let scope = scope_overlap(&scopes[i], &scopes[j])
                .map(|(a, b)| format!("scope '{a}' overlaps '{b}'"));
            let resource = prds[i]
                .metadata
                .resources
                .iter()
                .find(|r| prds[j].metadata.resources.contains(r))
                .map(|r| format!("resource '{r}'"));
            if let Some(reason) = scope.or(resource) {
                edges[i][j] = true;
                edges[j][i] = true;
                conflicts.push((prds[i].id.clone(), prds[j].id.clone(), reason));
            }
        }
    }
    fn search(index: usize, picked: &mut Vec<usize>, edges: &[Vec<bool>], best: &mut usize) {
        if index == edges.len() {
            *best = (*best).max(picked.len());
            return;
        }
        if picked.len() + edges.len() - index <= *best {
            return;
        }
        if picked.iter().all(|&p| !edges[p][index]) {
            picked.push(index);
            search(index + 1, picked, edges, best);
            picked.pop();
        }
        search(index + 1, picked, edges, best);
    }
    let mut best = 0;
    search(0, &mut Vec::new(), &edges, &mut best);
    Ok(WidthReport {
        graph_width: prds.len(),
        achievable_width: best,
        conflicts,
    })
}

pub fn validate_claimed_width(
    repository: &Path,
    prds: &[DiscoveredPrd],
    claimed: usize,
) -> Result<WidthReport, String> {
    let report = achievable_width(repository, prds)?;
    if claimed > report.achievable_width {
        let pairs = report
            .conflicts
            .iter()
            .map(|(a, b, r)| format!("{a}<->{b}: {r}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "claimed width {claimed} exceeds achievable width {}: {pairs}",
            report.achievable_width
        ));
    }
    Ok(report)
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
        if a.0 == "crates/familiar-ai-storage/migrations/" {
            return None;
        }
        right
            .iter()
            .find(|b| {
                b.0 != "crates/familiar-ai-storage/migrations/" && scope_entries_overlap(a, b)
            })
            .map(|b| (a.0.clone(), b.0.clone()))
    })
}

/// Schedule from the current ready set. Dependencies are admission gates, not
/// mutual-exclusion edges: every pending PRD whose dependencies are completed
/// is admitted up to `limit`, and two ready PRDs serialize only for an
/// overlapping mutable expected-file scope. Completed dependencies are safe
/// immediately after integration because subsequently admitted workers branch
/// from the persisted integration revision; delivery is independent.
/// Scopes and resources held by workers still in flight. PRD-077
/// (FAM-BUG-012 family): these holds persist across scheduling passes until
/// the holder's result is drained — without them a later pass could admit a
/// PRD overlapping an active worker's files.
pub type ActiveHold = (
    PrdId,
    Vec<(String, familiar_ai_review::ExpectedMatchKind)>,
    Vec<String>,
);

#[allow(clippy::too_many_arguments)]
fn select_batch(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    db: &mut Database,
    attempted: &BTreeSet<PrdId>,
    allowlist: Option<&BTreeSet<PrdId>>,
    active_holds: &[ActiveHold],
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
    // Seed the conflict sets with in-flight holders so admission never
    // overlaps an active worker; in-flight ids label their deferrals
    // distinctly from same-batch conflicts.
    let in_flight: BTreeSet<PrdId> = active_holds.iter().map(|(id, _, _)| id.clone()).collect();
    let mut selected_scopes: Vec<(PrdId, Vec<(String, familiar_ai_review::ExpectedMatchKind)>)> =
        active_holds
            .iter()
            .map(|(id, scope, _)| (id.clone(), scope.clone()))
            .collect();
    let mut selected_resources: Vec<(PrdId, Vec<String>)> = active_holds
        .iter()
        .map(|(id, _, resources)| (id.clone(), resources.clone()))
        .collect();
    for entry in entries {
        // An attempt that failed before claim leaves the PRD pending; without
        // this exclusion the session would select it forever.
        if attempted.contains(&entry.prd.id) {
            continue;
        }
        let unmet: Vec<&PrdId> = entry
            .prd
            .dependencies
            .iter()
            .filter(|id| statuses.get(*id) != Some(&BacklogStatus::Completed))
            .collect();
        if entry.status != BacklogStatus::Pending || !unmet.is_empty() {
            // PRD-077 (FAM-BUG-012): a predecessor that was attempted this
            // session and did not integrate defers its dependents with a
            // durable named decision instead of silence.
            let blocked_by: Vec<String> = unmet
                .iter()
                .filter(|id| attempted.contains(**id) || in_flight.contains(**id))
                .map(ToString::to_string)
                .collect();
            if entry.status == BacklogStatus::Pending && !blocked_by.is_empty() {
                decisions.push(SelectionDecision {
                    prd_id: entry.prd.id.clone(),
                    decision: "dependency_not_integrated",
                    detail: format!(
                        "dependency {} was attempted this session and is not integrated into the session revision",
                        blocked_by.join(", ")
                    ),
                });
            }
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
            let decision = if in_flight.contains(&holder) {
                "deferred_scope_held"
            } else {
                "deferred_scope_overlap"
            };
            decisions.push(SelectionDecision {
                prd_id: entry.prd.id.clone(),
                decision,
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
    let codex_session = familiar_ai_agent::CodexExecutionSession::default();
    DriverRepository::new(db.conn())
        .open_session(&session_id, &repository.key, &warrant.as_json())
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    {
        let mut reservations = ReservationRepository::new(db.conn_mut());
        if warrant.max_cost_microusd > 0 {
            reservations
                .define_pool(
                    &session_id,
                    &ResourceType::NanousdBudget,
                    warrant
                        .max_cost_microusd
                        .checked_mul(1_000)
                        .ok_or_else(|| {
                            DriveError::Config("cost warrant exceeds nanoUSD range".into())
                        })?,
                    false,
                )
                .map_err(|error| DriveError::Storage(error.to_string()))?;
        }
        if warrant.max_tokens > 0 {
            reservations
                .define_pool(
                    &session_id,
                    &ResourceType::UncachedTokens,
                    warrant.max_tokens,
                    false,
                )
                .map_err(|error| DriveError::Storage(error.to_string()))?;
        }
    }
    let session_base =
        git_output(&repository.worktree, &["rev-parse", "HEAD"]).map_err(DriveError::Config)?;
    OrchestrationRepository::new(db.conn())
        .initialize_integration(&session_id, &session_base)
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
    // Each (prd, decision) pair is persisted once per session, not once per
    // scheduling pass.
    let mut recorded_decisions = BTreeSet::<(PrdId, &'static str)>::new();
    let mut width_reported = false;

    let session_preflight = crate::preflight::run(agents, config, &repository.worktree);
    // Persist the complete pass/fail/deduplication ledger, not only a terminal
    // failure. This makes the once-per-session probe contract auditable after
    // the process and its transient logs are gone.
    DriverRepository::new(db.conn())
        .record_session_detail(&session_id, &session_preflight.session_summary())
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    let termination = if !session_preflight.is_valid() {
        let detail = session_preflight.failure_summary();
        eprintln!("drive: session preflight failed: {detail}");
        DriveTermination::PreflightFailed
    } else {
        std::thread::scope(|scope| -> Result<DriveTermination, DriveError> {
            let (result_sender, result_receiver) = std::sync::mpsc::channel();
            let mut active_workers = 0_usize;
            let mut stop_after_drain = None;
            // PRD-077: scopes/resources of in-flight workers, held until their
            // results drain, and per-reason deterministic-failure streaks for
            // the session circuit breaker.
            let mut active_holds: Vec<ActiveHold> = Vec::new();
            let mut failure_streaks: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let termination = loop {
                if active_workers == 0 {
                    if let Some(reason) = stop_after_drain {
                        break reason;
                    }
                }
                if heartbeat.failed() {
                    stop_after_drain = Some(DriveTermination::WorkerHeartbeatLost);
                }
                let elapsed = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
                if let Some(reason) =
                    warrant.exhausted(attempted, known_cost, known_tokens, elapsed)
                {
                    stop_after_drain.get_or_insert(reason);
                    if active_workers == 0 {
                        break reason;
                    }
                }
                let discovered = match discovery
                    .discover_with_layout(&repository, &repository_config.layout())
                {
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
                let remaining = if stop_after_drain.is_some() {
                    0
                } else if warrant.max_prds == 0 {
                    parallelism
                } else {
                    usize::try_from(warrant.max_prds.saturating_sub(attempted))
                        .unwrap_or(usize::MAX)
                        .min(parallelism)
                }
                .min(parallelism.saturating_sub(active_workers));
                let mut decisions = Vec::new();
                let selection = select_batch(
                    &repository,
                    &discovered,
                    &mut db,
                    &attempted_ids,
                    warrant.prd_allowlist.as_ref(),
                    &active_holds,
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
                    Selection::BacklogEmpty if active_workers == 0 => {
                        break DriveTermination::BacklogEmpty
                    }
                    Selection::NothingEligible if active_workers == 0 => {
                        break DriveTermination::NothingEligible
                    }
                    Selection::BacklogEmpty | Selection::NothingEligible => Vec::new(),
                };
                if !width_reported {
                    width_reported = true;
                    if targets.len() < remaining {
                        let detail = format!(
                            "achievable_width={} requested_width={remaining}",
                            targets.len()
                        );
                        eprintln!("drive: {detail}");
                        if let Err(error) = DriverRepository::new(db.conn())
                            .record_session_detail(&session_id, &detail)
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
                    let reservation_requests = component_warrant_requests(
                        &session_id,
                        &warrant,
                        config.driver.max_implementation_tokens,
                        parallelism,
                    );
                    let reservation_id = if reservation_requests.is_empty() {
                        None
                    } else {
                        let identity = ReservationOwnerIdentity {
                            owner_instance_id: format!("{session_id}:{sequence}"),
                            installation_id: None,
                            nonce_or_generation: session_id.clone(),
                            owner_kind: "drive-component".into(),
                            project_id: repository.key.clone(),
                            execution_id: format!("{session_id}:{sequence}"),
                            component_id: component_id.clone(),
                        };
                        match ReservationRepository::new(db.conn_mut()).acquire(
                            &identity,
                            &reservation_requests,
                            GrantMode::AllOrNothing,
                            None,
                        ) {
                            Ok(AcquireOutcome::Granted(grant)) => Some(grant.reservation_id),
                            Ok(AcquireOutcome::Refused { unavailable }) => {
                                eprintln!(
                                    "drive: component {} refused: finite warrant reservation unavailable: {unavailable:?}",
                                    target.id
                                );
                                DriverRepository::new(db.conn())
                                    .record_attempt_finished(
                                        &session_id,
                                        sequence,
                                        "retained",
                                        Some("warrant_reservation_refused"),
                                        None,
                                        None,
                                    )
                                    .map_err(|error| DriveError::Storage(error.to_string()))?;
                                continue;
                            }
                            Err(error) => {
                                return Err(DriveError::Storage(error.to_string()));
                            }
                        }
                    };
                    let migration_version =
                        if prd_scope(&repository.worktree, &target).is_ok_and(|scope| {
                            scope.iter().any(|(path, kind)| {
                                path == "crates/familiar-ai-storage/migrations/"
                                    && *kind == familiar_ai_review::ExpectedMatchKind::Directory
                            })
                        }) {
                            let first = next_migration_version(&repository.worktree);
                            match OrchestrationRepository::new(db.conn()).reserve_migration(
                                &repository.key,
                                &session_id,
                                &target.id.to_string(),
                                first,
                            ) {
                                Ok(reservation) => Some(reservation.version),
                                Err(error) => {
                                    eprintln!(
                                        "drive: cannot reserve migration version for {}: {error}",
                                        target.id
                                    );
                                    preparation_failed = true;
                                    break;
                                }
                            }
                        } else {
                            None
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
                    // Integration-gated completion requires every candidate to
                    // remain isolated, including configured width one.
                    let use_worktree = true;
                    let worktree = if use_worktree {
                        let integration_revision = OrchestrationRepository::new(db.conn())
                            .integration_revision(&session_id)
                            .map_err(|error| DriveError::Storage(error.to_string()))?;
                        match crate::worktree::WorktreeLease::create_component_at(
                            &repository.worktree,
                            &paths.state_dir,
                            &session_id,
                            &target.id.to_string(),
                            (!config.driver.worktree_root.is_empty())
                                .then(|| Path::new(&config.driver.worktree_root)),
                            &integration_revision,
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
                                            "drive: cannot persist workspace evidence: {error}"
                                        );
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
                        migration_version,
                        reservation_id,
                    ));
                }
                if preparation_failed {
                    break DriveTermination::StorageFailure;
                }

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
                    migration_version,
                    reservation_id,
                ) in jobs
                {
                    let sender = result_sender.clone();
                    let progress_database_path = database_path.clone();
                    let progress_session_id = session_id.clone();
                    let codex_session = &codex_session;
                    active_workers = active_workers.saturating_add(1);
                    // PRD-077: hold this worker's scope and resources until its
                    // result drains, so no later scheduling pass admits
                    // overlapping work while it runs.
                    active_holds.push((
                        target.id.clone(),
                        prd_scope(&repository.worktree, &target).unwrap_or_default(),
                        target.metadata.resources.clone(),
                    ));
                    scope.spawn(move || {
                        let attempt_timer = Instant::now();
                        let _progress = ProgressGuard::start(
                            target.id.to_string(),
                            "execution",
                            progress_database_path,
                            progress_session_id,
                            sequence,
                            Duration::from_secs(
                                execution_config.daemon.heartbeat_interval_secs.max(1),
                            ),
                        );
                        let prd_path = execution_root.join(target.path.as_str());
                        let execution =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                crate::run::execute_reviewed_candidate(
                                    &execution_root,
                                    &prd_path,
                                    agents,
                                    &execution_config,
                                    paths,
                                    Some(route_context.clone()),
                                    None,
                                    migration_version,
                                    codex_session,
                                )
                            }));
                        let duration_ms =
                            attempt_timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        let _ = sender.send((
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
                            migration_version,
                            reservation_id,
                        ));
                    });
                }
                let mut batch_stop = None;
                if active_workers == 0 {
                    break DriveTermination::NothingEligible;
                }
                let joined = match result_receiver.recv() {
                    Ok(joined) => joined,
                    Err(error) => {
                        eprintln!("drive: worker result channel failed: {error}");
                        break DriveTermination::StorageFailure;
                    }
                };
                active_workers = active_workers.saturating_sub(1);
                for joined in std::iter::once(joined) {
                    active_holds.retain(|(id, _, _)| *id != joined.0.id);
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
                        migration_version,
                        reservation_id,
                    ) = joined;
                    let heartbeat_failed = worktree_heartbeat
                        .as_ref()
                        .is_some_and(crate::worktree::WorktreeHeartbeatGuard::failed);
                    drop(worktree_heartbeat);
                    let (mut result, mut trace) = match execution {
                        Ok(value) => value,
                        Err(_) => {
                            eprintln!("drive: attempt worker panicked for {}", target.id);
                            if let Some(lease) = &mut worktree {
                                if let Err(error) = lease.mark_state("retained_unclassified") {
                                    eprintln!(
                                        "drive: cannot persist panicked worktree state: {error}"
                                    );
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
                    if trace.retained_reason.is_none() {
                        if let Err(crate::run::RunError::Context(error)) = &result {
                            trace.retained_reason = Some(error.retention_class());
                        }
                    }
                    if let Err(crate::run::RunError::HumanReviewRequired {
                        result: implementation,
                        cycle,
                        prd_id,
                    }) = &result
                    {
                        if let Ok(Some(checkpoint)) =
                            familiar_ai_storage::CheckpointRepository::new(db.conn())
                                .get(&repository.key, prd_id)
                        {
                            let mut pending_hashes = Vec::new();
                            for finding in cycle
                                .scope_evaluations
                                .iter()
                                .flat_map(|evaluation| &evaluation.findings)
                            {
                                if let Ok(json) = serde_json::to_string(finding) {
                                    let hash = familiar_ai_review::content_hash(json.as_bytes());
                                    if OrchestrationRepository::new(db.conn())
                                        .record_scope_finding(
                                            &repository.key,
                                            &checkpoint.checkpoint_id,
                                            prd_id,
                                            &checkpoint.diff_hash,
                                            &hash,
                                            &json,
                                        )
                                        .is_ok()
                                    {
                                        pending_hashes.push(hash);
                                    }
                                }
                            }
                            // PRD-080: a scope pause is decidable, not a dead
                            // end — surface the exact command per finding.
                            for hash in &pending_hashes {
                                eprintln!(
                                    "drive: scope decision pending for {prd_id}: familiar-ai scope-decisions {hash} --candidate-hash {} --approve|--reject --actor human:<identity> --reason \"...\"",
                                    checkpoint.diff_hash
                                );
                            }
                        }
                        if let Some(policy) = delivery_policy.filter(|policy| {
                            policy.mode == familiar_ai_core::DeliveryMode::PocSelfApproval
                        }) {
                            let warrant =
                                policy.poc_warrant.as_ref().expect("validated PoC warrant");
                            let unexpired =
                                chrono::DateTime::parse_from_rfc3339(&warrant.expires_at)
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
                    if let Some(reservation_id) = reservation_id {
                        let observation = if (warrant.max_cost_microusd == 0 || cost.is_some())
                            && (warrant.max_tokens == 0 || tokens.is_some())
                        {
                            let mut observed = Vec::new();
                            if let Some(value) = cost {
                                observed.push(ResourceRequest {
                                    pool_id: session_id.clone(),
                                    resource_type: ResourceType::NanousdBudget,
                                    amount: value.saturating_mul(1_000),
                                });
                            }
                            if let Some(value) = tokens {
                                observed.push(ResourceRequest {
                                    pool_id: session_id.clone(),
                                    resource_type: ResourceType::UncachedTokens,
                                    amount: value,
                                });
                            }
                            SettlementObservation::Known(observed)
                        } else {
                            SettlementObservation::Unknown {
                                policy: UnknownConsumptionPolicy::SettleReservedAmount,
                            }
                        };
                        if let Err(error) = ReservationRepository::new(db.conn_mut()).settle(
                            &reservation_id,
                            observation,
                            &format!("{session_id}:{sequence}"),
                        ) {
                            eprintln!("drive: cannot settle component warrant: {error}");
                            batch_stop = Some(DriveTermination::StorageFailure);
                        }
                    }
                    let unclassified = result.is_err() && trace.retained_reason.is_none();
                    if let Err(error) = &result {
                        eprintln!("drive: attempt {sequence} {} failed: {error}", target.id);
                    }
                    // Review success is not completion. Commit the candidate,
                    // land it against the latest persisted integration revision,
                    // then atomically expose completion to dependency selection.
                    let mut integration_ok = result.is_ok();
                    if integration_ok {
                        let integration = (|| -> Result<(), String> {
                            let root = worktree.as_ref().map(|w| w.path()).ok_or_else(|| {
                                "reviewed candidate has no isolated worktree".to_string()
                            })?;
                            git_output(root, &["add", "-A"])?;
                            let status = git_output(root, &["status", "--porcelain"])?;
                            if !status.is_empty() {
                                git_output(
                                    root,
                                    &[
                                        "commit",
                                        "-m",
                                        &format!("familiar: implement {}", target.id),
                                    ],
                                )?;
                            }
                            let candidate = git_output(root, &["rev-parse", "HEAD"])?;
                            if let Some(version) = migration_version {
                                validate_reserved_migration(root, &candidate, version)?;
                            }
                            DriverRepository::new(db.conn())
                                .record_attempt_diagnostics(
                                    &session_id,
                                    sequence,
                                    trace.execution_id.as_deref(),
                                    Some(adapter_id),
                                    routed_model.as_deref().or(configured_model),
                                    None,
                                    None,
                                    "review_complete",
                                )
                                .map_err(|e| e.to_string())?;
                            let prior = OrchestrationRepository::new(db.conn())
                                .integration_revision(&session_id)
                                .map_err(|e| e.to_string())?;
                            let merged = merge_candidate(&repository.worktree, &prior, &candidate)?;
                            let execution_id = trace.execution_id.as_deref().ok_or_else(|| {
                                "reviewed candidate has no execution id".to_string()
                            })?;
                            let required = config
                                .review
                                .verification
                                .iter()
                                .filter(|c| c.required)
                                .map(|c| c.check_id.clone())
                                .collect::<Vec<_>>();
                            let actor = format!("system:familiar-ai-run:{execution_id}");
                            let checkpoint =
                                familiar_ai_storage::CheckpointRepository::new(db.conn())
                                    .get(&repository.key, &target.id.to_string())
                                    .map_err(|e| e.to_string())?;
                            let now = chrono::Utc::now().to_rfc3339();
                            familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
                            .complete_run_with(
                                &repository,
                                &target,
                                execution_id,
                                &actor,
                                &required,
                                |tx| {
                                    let changed = tx.execute(
                                        "UPDATE driver_sessions SET integration_revision=?1 WHERE session_id=?2 AND integration_revision=?3 AND ended_at IS NULL",
                                        rusqlite::params![merged, session_id, prior],
                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                    if changed != 1 {
                                        return Err(BacklogStoreError::Storage(
                                            "integration revision changed during landing".into(),
                                        ));
                                    }
                                    let changed = tx.execute(
                                        "UPDATE driver_attempts SET candidate_revision=?1,integrated_at=?2,last_durable_phase='integrated' WHERE session_id=?3 AND sequence=?4 AND integrated_at IS NULL",
                                        rusqlite::params![merged, now, session_id, sequence],
                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                    if changed != 1 {
                                        return Err(BacklogStoreError::Storage(
                                            "attempt was already integrated or is missing".into(),
                                        ));
                                    }
                                    if let Some(checkpoint) = &checkpoint {
                                        tx.execute(
                                            "UPDATE execution_checkpoints SET phase='completed',invalid_reason=NULL,updated_at=?1 WHERE checkpoint_id=?2",
                                            rusqlite::params![now, checkpoint.checkpoint_id],
                                        ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                        tx.execute(
                                            "INSERT OR IGNORE INTO execution_checkpoint_events(event_id,checkpoint_id,event_type,prior_phase,resulting_phase,detail,recorded_at) VALUES(?1,?2,'phase_transition',?3,'completed',?4,?5)",
                                            rusqlite::params![format!("{}:completed", checkpoint.checkpoint_id), checkpoint.checkpoint_id, checkpoint.phase, format!("candidate={candidate} integration={merged}"), now],
                                        ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                    }
                                    let changed = tx.execute(
                                        "UPDATE migration_version_reservations SET state='consumed',resolved_at=?1 WHERE session_id=?2 AND prd_id=?3 AND state='reserved'",
                                        rusqlite::params![now, session_id, target.id.to_string()],
                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                    if migration_version.is_some() && changed != 1 {
                                        return Err(BacklogStoreError::Storage(
                                            "active migration reservation is missing".into(),
                                        ));
                                    }
                                    Ok(())
                                },
                            )
                            .map_err(|e| e.to_string())?;
                            Ok(())
                        })();
                        if let Err(error) = integration {
                            if OrchestrationRepository::new(db.conn())
                                .reservation(&session_id, &target.id.to_string())
                                .ok()
                                .flatten()
                                .is_some_and(|r| r.state == "reserved")
                            {
                                let _ = OrchestrationRepository::new(db.conn()).resolve_migration(
                                    &session_id,
                                    &target.id.to_string(),
                                    false,
                                );
                            }
                            eprintln!(
                                "drive: integration remediation required for {}: {error}",
                                target.id
                            );
                            integration_ok = false;
                        }
                    }
                    // PRD-077 circuit breaker: identical deterministic
                    // retained reasons indicate a shared configuration cause;
                    // the third occurrence stops the session with one
                    // executable recovery plan instead of burning the warrant.
                    const DETERMINISTIC_REASONS: &[&str] = &[
                        "preflight_failed",
                        "malformed_output",
                        "review_disabled",
                        "worktree_failed",
                        "warrant_reservation_refused",
                        "implementation_token_usage_unknown",
                        "missing_authoritative_input_reference",
                        "unreadable_reference",
                        "context_compilation_failed",
                        "implementation_incomplete",
                    ];
                    let (outcome, retained_reason) = match (&result, integration_ok) {
                        (Ok(_), true) => {
                            completed += 1;
                            ("completed", None)
                        }
                        _ => (
                            "retained",
                            trace.retained_reason.or(Some(if result.is_ok() {
                                "integration_failed"
                            } else {
                                "unclassified_result"
                            })),
                        ),
                    };
                    if let Some(reason) = retained_reason {
                        if DETERMINISTIC_REASONS.contains(&reason) {
                            let victims = failure_streaks.entry(reason.to_string()).or_default();
                            victims.push(target.id.to_string());
                            if victims.len() >= 3 && batch_stop.is_none() {
                                let plan = format!(
                                    "deterministic_failure_cascade reason={reason} prds={} — fix the shared cause once, then rerun the remaining allowlist: familiar-ai drive --max-prds {} {}",
                                    victims.join(","),
                                    warrant.max_prds.max(1),
                                    warrant
                                        .prd_allowlist
                                        .as_ref()
                                        .map(|ids| ids
                                            .iter()
                                            .filter(|id| !attempted_ids.contains(id))
                                            .map(|id| format!("--prd {id}"))
                                            .collect::<Vec<_>>()
                                            .join(" "))
                                        .unwrap_or_default()
                                );
                                eprintln!("drive: {plan}");
                                if let Err(error) = DriverRepository::new(db.conn())
                                    .record_session_detail(&session_id, &plan)
                                {
                                    eprintln!("drive: cannot persist recovery plan: {error}");
                                }
                                batch_stop = Some(DriveTermination::DeterministicFailureCascade);
                            }
                        }
                    }
                    if let Some(lease) = &mut worktree {
                        let state = if integration_ok {
                            "ready_for_delivery"
                        } else {
                            "retained"
                        };
                        if let Err(error) = lease.mark_state(state) {
                            eprintln!("drive: cannot persist worktree state: {error}");
                            batch_stop = Some(DriveTermination::StorageFailure);
                            continue;
                        }
                        if integration_ok
                            && delivery_policy.is_some_and(|policy| {
                                policy.mode != familiar_ai_core::DeliveryMode::Disabled
                            })
                        {
                            let policy = delivery_policy.expect("checked delivery policy");
                            if delivered >= policy.max_deliveries_per_session {
                                batch_stop
                                    .get_or_insert(DriveTermination::BudgetDeliveriesExhausted);
                            } else {
                                let delivery_heartbeat =
                                    lease.start_heartbeat(Duration::from_secs(
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
                                            eprintln!(
                                                "drive: cannot persist delivery phase: {error}"
                                            );
                                            batch_stop = Some(DriveTermination::StorageFailure);
                                        }
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "drive: delivery blocked for {}: {error}",
                                            target.id
                                        );
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
                                let escalation_base = OrchestrationRepository::new(db.conn())
                                    .integration_revision(&session_id)
                                    .map_err(|error| DriveError::Storage(error.to_string()))?;
                                match crate::worktree::WorktreeLease::create_component_at(
                                    &repository.worktree,
                                    &paths.state_dir,
                                    &session_id,
                                    &escalation_component,
                                    (!config.driver.worktree_root.is_empty())
                                        .then(|| Path::new(&config.driver.worktree_root)),
                                    &escalation_base,
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
                                                    timer
                                                        .elapsed()
                                                        .as_millis()
                                                        .min(u64::MAX as u128)
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
                                                    known_tokens =
                                                        known_tokens.saturating_add(value);
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
                                                // Escalated review success has the same completion
                                                // boundary as a primary attempt: commit and merge the
                                                // candidate, then expose integration and backlog
                                                // completion in one transaction.
                                                let mut escalation_integration_ok =
                                                    escalated.is_ok();
                                                if escalation_integration_ok {
                                                    let integration = (|| -> Result<(), String> {
                                                        git_output(
                                                            &escalation_root,
                                                            &["add", "-A"],
                                                        )?;
                                                        let status = git_output(
                                                            &escalation_root,
                                                            &["status", "--porcelain"],
                                                        )?;
                                                        if !status.is_empty() {
                                                            git_output(
                                                                &escalation_root,
                                                                &[
                                                                    "commit",
                                                                    "-m",
                                                                    &format!(
                                                                        "familiar: implement {}",
                                                                        target.id
                                                                    ),
                                                                ],
                                                            )?;
                                                        }
                                                        let candidate = git_output(
                                                            &escalation_root,
                                                            &["rev-parse", "HEAD"],
                                                        )?;
                                                        if let Some(version) = migration_version {
                                                            validate_reserved_migration(
                                                                &escalation_root,
                                                                &candidate,
                                                                version,
                                                            )?;
                                                        }
                                                        let prior =
                                                            OrchestrationRepository::new(db.conn())
                                                                .integration_revision(&session_id)
                                                                .map_err(|e| e.to_string())?;
                                                        let merged = merge_candidate(
                                                            &repository.worktree,
                                                            &prior,
                                                            &candidate,
                                                        )?;
                                                        let execution_id = escalation_trace
                                                            .execution_id
                                                            .as_deref()
                                                            .ok_or_else(|| {
                                                                "reviewed escalated candidate has no execution id"
                                                                    .to_string()
                                                            })?;
                                                        let required = config
                                                            .review
                                                            .verification
                                                            .iter()
                                                            .filter(|check| check.required)
                                                            .map(|check| check.check_id.clone())
                                                            .collect::<Vec<_>>();
                                                        let actor = format!(
                                                            "system:familiar-ai-run:{execution_id}"
                                                        );
                                                        let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
                                                            .get(&repository.key, &target.id.to_string())
                                                            .map_err(|e| e.to_string())?;
                                                        let now = chrono::Utc::now().to_rfc3339();
                                                        familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
                                                            .complete_run_with(
                                                                &repository,
                                                                &target,
                                                                execution_id,
                                                                &actor,
                                                                &required,
                                                                |tx| {
                                                                    let changed = tx.execute(
                                                                        "UPDATE driver_sessions SET integration_revision=?1 WHERE session_id=?2 AND integration_revision=?3 AND ended_at IS NULL",
                                                                        rusqlite::params![merged, session_id, prior],
                                                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                                                    if changed != 1 {
                                                                        return Err(BacklogStoreError::Storage("integration revision changed during escalated landing".into()));
                                                                    }
                                                                    let changed = tx.execute(
                                                                        "UPDATE driver_attempts SET candidate_revision=?1,integrated_at=?2,last_durable_phase='integrated' WHERE session_id=?3 AND sequence=?4 AND integrated_at IS NULL",
                                                                        rusqlite::params![merged, now, session_id, escalation_sequence],
                                                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                                                    if changed != 1 {
                                                                        return Err(BacklogStoreError::Storage("escalated attempt was already integrated or is missing".into()));
                                                                    }
                                                                    if let Some(checkpoint) = &checkpoint {
                                                                        tx.execute(
                                                                            "UPDATE execution_checkpoints SET phase='completed',invalid_reason=NULL,updated_at=?1 WHERE checkpoint_id=?2",
                                                                            rusqlite::params![now, checkpoint.checkpoint_id],
                                                                        ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                                                        tx.execute(
                                                                            "INSERT OR IGNORE INTO execution_checkpoint_events(event_id,checkpoint_id,event_type,prior_phase,resulting_phase,detail,recorded_at) VALUES(?1,?2,'phase_transition',?3,'completed',?4,?5)",
                                                                            rusqlite::params![format!("{}:completed", checkpoint.checkpoint_id), checkpoint.checkpoint_id, checkpoint.phase, format!("candidate={candidate} integration={merged}"), now],
                                                                        ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                                                    }
                                                                    let changed = tx.execute(
                                                                        "UPDATE migration_version_reservations SET state='consumed',resolved_at=?1 WHERE session_id=?2 AND prd_id=?3 AND state='reserved'",
                                                                        rusqlite::params![now, session_id, target.id.to_string()],
                                                                    ).map_err(|e| BacklogStoreError::Storage(e.to_string()))?;
                                                                    if migration_version.is_some() && changed != 1 {
                                                                        return Err(BacklogStoreError::Storage("active migration reservation is missing".into()));
                                                                    }
                                                                    Ok(())
                                                                },
                                                            )
                                                            .map_err(|e| e.to_string())?;
                                                        Ok(())
                                                    })(
                                                    );
                                                    if let Err(error) = integration {
                                                        if OrchestrationRepository::new(db.conn())
                                                            .reservation(
                                                                &session_id,
                                                                &target.id.to_string(),
                                                            )
                                                            .ok()
                                                            .flatten()
                                                            .is_some_and(|reservation| {
                                                                reservation.state == "reserved"
                                                            })
                                                        {
                                                            let _ = OrchestrationRepository::new(
                                                                db.conn(),
                                                            )
                                                            .resolve_migration(
                                                                &session_id,
                                                                &target.id.to_string(),
                                                                false,
                                                            );
                                                        }
                                                        eprintln!(
                                                            "drive: escalated integration remediation required for {}: {error}",
                                                            target.id
                                                        );
                                                        escalation_integration_ok = false;
                                                    }
                                                }
                                                let escalation_outcome =
                                                    if escalation_integration_ok {
                                                        "completed"
                                                    } else {
                                                        "retained"
                                                    };
                                                let escalation_reason = if escalation_integration_ok
                                                {
                                                    None
                                                } else if escalated.is_ok() {
                                                    Some("integration_failed")
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
                                                            .and_then(|value| {
                                                                value.model.as_deref()
                                                            })
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
                                                    batch_stop =
                                                        Some(DriveTermination::StorageFailure);
                                                } else if escalation_integration_ok {
                                                    completed = completed.saturating_add(1);
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
                                                                    let _ = escalation_tree
                                                                        .mark_state(
                                                                            &delivery.phase,
                                                                        );
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
                                                eprintln!(
                                                    "drive: cannot record escalation: {error}"
                                                );
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
                    stop_after_drain.get_or_insert(reason);
                }
                if let Err(error) =
                    DriverRepository::new(db.conn()).heartbeat(&session_id, &session_id)
                {
                    eprintln!("drive: heartbeat persistence failed: {error}");
                    break DriveTermination::StorageFailure;
                }
            };
            Ok(termination)
        })?
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

fn git_output(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn next_migration_version(repository: &Path) -> u64 {
    std::fs::read_dir(repository.join("crates/familiar-ai-storage/migrations"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .split('_')
                .next()?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn validate_reserved_migration(
    repository: &Path,
    candidate: &str,
    reserved: u64,
) -> Result<(), String> {
    let parent = git_output(repository, &["rev-parse", &format!("{candidate}^")])?;
    let changes = git_output(repository, &["diff", "--name-status", &parent, candidate])?;
    let authored = changes
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let status = fields.next()?;
            let path = fields.next_back()?;
            (status.starts_with('A') && path.starts_with("crates/familiar-ai-storage/migrations/"))
                .then_some(path)
        })
        .collect::<Vec<_>>();
    let expected = format!("{reserved:03}_");
    if authored.len() != 1
        || !authored[0]
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(&expected))
    {
        return Err(format!(
            "candidate must consume reserved migration {reserved:03} exactly once; authored={authored:?}"
        ));
    }
    Ok(())
}

fn component_parallelism(config: &Config, warrant: &DriveWarrant) -> usize {
    let _ = warrant;
    config.driver.max_parallel_components
}

fn component_warrant_requests(
    pool_id: &str,
    warrant: &DriveWarrant,
    implementation_tokens: u64,
    parallelism: usize,
) -> Vec<ResourceRequest> {
    let divisor = u64::try_from(parallelism.max(1)).unwrap_or(u64::MAX);
    let mut requests = Vec::new();
    if warrant.max_cost_microusd > 0 {
        requests.push(ResourceRequest {
            pool_id: pool_id.to_owned(),
            resource_type: ResourceType::NanousdBudget,
            amount: warrant
                .max_cost_microusd
                .saturating_mul(1_000)
                .checked_div(divisor)
                .unwrap_or(0)
                .max(1),
        });
    }
    if warrant.max_tokens > 0 {
        let fair_share = warrant.max_tokens.checked_div(divisor).unwrap_or(0).max(1);
        requests.push(ResourceRequest {
            pool_id: pool_id.to_owned(),
            resource_type: ResourceType::UncachedTokens,
            amount: if implementation_tokens > 0 {
                implementation_tokens.min(fair_share)
            } else {
                fair_share
            },
        });
    }
    requests
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
        .input_tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_heartbeat_observes_latest_durable_phase_on_later_tick() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("heartbeat.sqlite3");
        let database = Database::open(&database_path).unwrap();
        database.run_migrations().unwrap();
        let driver = DriverRepository::new(database.conn());
        driver.open_session("timed", "repo", "{}").unwrap();
        let sequence = driver
            .record_attempt_started("timed", "PRD-66", "docs/prds/PRD-066.md", None)
            .unwrap();
        driver
            .record_attempt_diagnostics(
                "timed",
                sequence,
                None,
                None,
                None,
                None,
                None,
                "preflight",
            )
            .unwrap();
        let update_path = database_path.clone();
        let updater = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            let database = Database::open(&update_path).unwrap();
            DriverRepository::new(database.conn())
                .record_attempt_diagnostics(
                    "timed",
                    sequence,
                    None,
                    None,
                    None,
                    None,
                    None,
                    "review_complete",
                )
                .unwrap();
        });
        std::thread::sleep(Duration::from_millis(75));
        assert_eq!(
            durable_attempt_phase(&database_path, "timed", sequence),
            "review_complete"
        );
        updater.join().unwrap();
    }

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
            (
                DriveTermination::DeterministicFailureCascade,
                "deterministic_failure_cascade",
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
            DriveTermination::DeterministicFailureCascade,
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
            r#"{"max_prds":4,"max_cost_nanousd":0,"max_uncached_tokens":0,"max_duration_ms":60000}"#
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
    fn component_parallelism_composes_with_finite_warrants() {
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
        assert_eq!(component_parallelism(&config, &warrant), 7);
        warrant.max_tokens = 0;
        warrant.max_cost_microusd = 1;
        assert_eq!(component_parallelism(&config, &warrant), 7);
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
        limit: usize,
    ) -> (Vec<PrdId>, Vec<SelectionDecision>) {
        let mut decisions = Vec::new();
        let selected = match select_batch(
            repository,
            discovered,
            db,
            &BTreeSet::new(),
            allowlist,
            &[],
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 6);
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 6);
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 6);
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 6);
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, Some(&allowlist), 6);
        assert_eq!(selected, vec![PrdId::new(2)]);
        let excluded = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(1))
            .unwrap();
        assert_eq!(excluded.decision, "excluded_allowlist");
    }

    /// PRD-077 (FAM-BUG-012): a predecessor attempted this session without
    /// integrating defers its dependents with a durable named decision.
    #[test]
    fn attempted_unintegrated_dependency_defers_dependent_with_named_decision() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Active, &["a.rs"]),
            contract_prd(2, &[1], Active, &["b.rs"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        // PRD-1 was attempted (retained) this session; backlog stays pending.
        let attempted: BTreeSet<PrdId> = [PrdId::new(1)].into_iter().collect();
        let mut decisions = Vec::new();
        let selection = select_batch(
            &repository,
            &discovered,
            &mut db,
            &attempted,
            None,
            &[],
            6,
            &mut decisions,
        )
        .unwrap();
        assert!(matches!(selection, Selection::NothingEligible));
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .expect("dependent must receive a decision");
        assert_eq!(deferred.decision, "dependency_not_integrated");
        assert!(deferred.detail.contains("PRD-1"), "{}", deferred.detail);
    }

    /// PRD-077: an in-flight worker's scope is held across scheduling passes —
    /// a later pass must not admit overlapping work while it runs.
    #[test]
    fn in_flight_scope_hold_defers_overlapping_admission() {
        use familiar_ai_core::PrdLocation::Active;
        let (_temp, repository) = test_repository();
        let discovered = vec![contract_prd(2, &[], Active, &["src/lib.rs"])];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let holds: Vec<ActiveHold> = vec![(
            PrdId::new(1),
            vec![(
                "src/".into(),
                familiar_ai_review::ExpectedMatchKind::Directory,
            )],
            Vec::new(),
        )];
        let mut decisions = Vec::new();
        let selection = select_batch(
            &repository,
            &discovered,
            &mut db,
            &BTreeSet::new(),
            None,
            &holds,
            6,
            &mut decisions,
        )
        .unwrap();
        assert!(matches!(selection, Selection::NothingEligible));
        let deferred = decisions
            .iter()
            .find(|decision| decision.prd_id == PrdId::new(2))
            .unwrap();
        assert_eq!(deferred.decision, "deferred_scope_held");
        assert!(deferred.detail.contains("PRD-1"), "{}", deferred.detail);
    }

    #[test]
    fn integrated_dependency_admits_dependent_when_delivery_is_disabled() {
        use familiar_ai_core::PrdLocation::{Active, Archived};
        let (_temp, repository) = test_repository();
        let discovered = vec![
            contract_prd(1, &[], Archived, &["a.rs"]),
            contract_prd(2, &[1], Active, &["b.rs"]),
        ];
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 6);
        assert_eq!(selected, vec![PrdId::new(2)]);
        assert!(decisions.iter().all(|decision| {
            decision.prd_id != PrdId::new(2) || decision.decision == "ready_selected"
        }));
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
        let (selected, decisions) = select(&repository, &discovered, &mut db, None, 1);
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
            r#"{"max_prds":4,"max_cost_nanousd":0,"max_uncached_tokens":0,"max_duration_ms":0,"prd_allowlist":["PRD-65"]}"#
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
