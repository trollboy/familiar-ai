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
use std::time::Instant;

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, BacklogStatus, BacklogStatusStore, Config,
    DiscoveredPrd, FilesystemBacklogDiscovery, PrdId, RepositoryIdentity,
};
use familiar_ai_storage::{Database, DriverRepository, ExecutionHistoryRepository};

use crate::run::{execute_with_config_tracked, AgentSet};

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
        }
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
    Eligible(Box<DiscoveredPrd>),
    BacklogEmpty,
    NothingEligible,
}

fn select_next(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    db: &mut Database,
    attempted: &BTreeSet<PrdId>,
) -> Result<Selection, DriveError> {
    if discovered.is_empty() {
        return Ok(Selection::BacklogEmpty);
    }
    let mut entries = familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    entries.sort_by(|a, b| {
        (a.prd.number, a.prd.path.as_str().as_bytes())
            .cmp(&(b.prd.number, b.prd.path.as_str().as_bytes()))
    });
    let statuses: std::collections::BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.prd.id.clone(), entry.status))
        .collect();
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
            return Ok(Selection::Eligible(Box::new(entry.prd)));
        }
    }
    Ok(Selection::NothingEligible)
}

/// Execute eligible backlog PRDs until a closed termination condition is met.
pub fn drive(
    agents: &AgentSet<'_>,
    config: &Config,
    paths: &AppPaths,
    warrant: DriveWarrant,
) -> Result<DriveSummary, DriveError> {
    warrant.validate().map_err(DriveError::Config)?;
    let current = std::env::current_dir().map_err(|error| {
        DriveError::Config(format!("cannot resolve current directory: {error}"))
    })?;
    // The operator's configuration selects the grammar; a repository nobody
    // described resolves to canonical at the canonical locations.
    let discovery = FilesystemBacklogDiscovery::with_profile(config.repository_profile(&current));
    let repository = discovery
        .resolve(&current)
        .map_err(|error| DriveError::Config(error.to_string()))?;

    let database_path = config.database.resolve_path(&paths.data_dir);
    let mut db =
        Database::open(&database_path).map_err(|error| DriveError::Storage(error.to_string()))?;
    db.run_migrations()
        .map_err(|error| DriveError::Storage(error.to_string()))?;

    let session_id = format!("drive-{}", crate::run::new_id());
    DriverRepository::new(db.conn())
        .open_session(&session_id, &repository.key, &warrant.as_json())
        .map_err(|error| DriveError::Storage(error.to_string()))?;
    eprintln!(
        "drive: session {session_id} started warrant={}",
        warrant.as_json()
    );

    let timer = Instant::now();
    let mut attempted_ids: BTreeSet<PrdId> = BTreeSet::new();
    let mut attempted = 0_u64;
    let mut completed = 0_u64;
    let mut known_cost = 0_u64;

    let termination = loop {
        let elapsed = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if let Some(reason) = warrant.exhausted(attempted, known_cost, elapsed) {
            break reason;
        }
        let discovered = match discovery.discover(&repository) {
            Ok(discovered) => discovered,
            Err(error) => {
                eprintln!("drive: discovery failed: {error}");
                break DriveTermination::StorageFailure;
            }
        };
        if let Some(report) = discovered.conflict_report() {
            eprintln!("drive: refusing conflicting identities: {report}");
        }
        let discovered = discovered.prds;
        if let Err(error) = validate_graph(&discovered) {
            eprintln!("drive: backlog graph invalid: {error}");
            break DriveTermination::StorageFailure;
        }
        let target = match select_next(&repository, &discovered, &mut db, &attempted_ids)? {
            Selection::Eligible(prd) => *prd,
            Selection::BacklogEmpty => break DriveTermination::BacklogEmpty,
            Selection::NothingEligible => break DriveTermination::NothingEligible,
        };

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
                break DriveTermination::StorageFailure;
            }
        };
        eprintln!("drive: attempt {sequence} {} {}", target.id, target.path);

        let prd_path = Path::new(target.path.as_str());
        let attempt_timer = Instant::now();
        let (result, trace) = execute_with_config_tracked(prd_path, agents, config, paths);
        let duration_ms = attempt_timer.elapsed().as_millis().min(u64::MAX as u128) as u64;

        let cost = trace
            .execution_id
            .as_deref()
            .and_then(|id| attempt_cost(&db, id));
        if let Some(value) = cost {
            known_cost = known_cost.saturating_add(value);
        }
        let (outcome, retained_reason) = match &result {
            Ok(_) => {
                completed += 1;
                ("completed", None)
            }
            Err(_) => ("retained", trace.retained_reason),
        };
        if let Err(error) = DriverRepository::new(db.conn()).record_attempt_finished(
            &session_id,
            sequence,
            outcome,
            retained_reason,
            cost,
            Some(duration_ms),
            trace.review_scope.as_str(),
            trace.execution_context_scope.as_str(),
        ) {
            eprintln!("drive: cannot record attempt outcome: {error}");
            break DriveTermination::StorageFailure;
        }
        eprintln!(
            "drive: attempt {sequence} {} outcome={outcome}{}",
            target.id,
            retained_reason
                .map(|reason| format!(" reason={reason}"))
                .unwrap_or_default()
        );

        // Unknown cost is never treated as zero: with a cost ceiling in force,
        // an unmeasurable attempt ends the session rather than silently
        // consuming an unaccounted budget.
        if warrant.max_cost_microusd > 0 && cost.is_none() {
            break DriveTermination::CostUnknown;
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
        ] {
            assert_eq!(termination.as_str(), text);
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
}
