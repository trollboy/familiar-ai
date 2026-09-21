//! Helpers shared by more than one `familiar-ai` CLI subcommand
//! implementation.

use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::Database;

pub fn escape_output(value: &str) -> String {
    format!("{value:?}")
}

pub fn database() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}

pub fn effective_repository_config(
    paths: &AppPaths,
    repository: &std::path::Path,
) -> Result<Config, String> {
    crate::config_cli::effective_config_for_repository(
        &crate::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        repository,
    )
}

/// PRD-090: every top-level command name that moved under an administrative
/// namespace, paired with the full invocation that replaces it. The binary
/// keeps each old name working as a hidden top-level alias — this table is
/// what it consults to print the replacement, and what
/// `tests/cli_surface.rs` walks to pin that every relocation still resolves.
pub const RELOCATED_COMMAND_ALIASES: &[(&str, &str)] = &[
    ("scope-decisions", "approve"),
    ("billing", "accounting billing"),
    ("compress", "config compress"),
    ("model-residency", "config model-residency"),
    ("status", "stewardship status"),
    ("preflight", "stewardship preflight"),
    ("history", "stewardship history"),
    ("usage", "accounting usage"),
    ("onboard", "plan onboard"),
    ("backlog", "plan backlog"),
    ("batch-review", "plan batch-review"),
    ("control", "ops control"),
    ("worker", "ops worker"),
    ("operator", "ops operator"),
    ("gate", "ops gate"),
    ("waive", "ops waive"),
];

/// PRD-090: the declared top-level surface -- the daily verbs a session
/// actually runs, plus the small set of administrative namespaces that hold
/// everything else. `tests/cli_surface.rs` asserts `familiar-ai --help`
/// lists exactly this set (and nothing hidden), so growing the top level is
/// a deliberate decision with a diff, not an accretion.
pub const TOP_LEVEL_COMMAND_CEILING: usize = 12;
pub const TOP_LEVEL_COMMANDS: &[&str] = &[
    "next",
    "run",
    "drive",
    "resume",
    "report",
    "approve",
    "deliver",
    "config",
    "accounting",
    "stewardship",
    "plan",
    "ops",
];

/// The exact note printed when a relocated command's previous top-level
/// invocation is used, naming the form that replaces it. `None` when
/// `invoked` never moved.
pub fn relocation_notice(invoked: &str) -> Option<String> {
    RELOCATED_COMMAND_ALIASES
        .iter()
        .find(|(old, _)| *old == invoked)
        .map(|(old, new)| {
            format!(
                "note: 'familiar-ai {old}' is now 'familiar-ai {new}' (this alias will keep working)"
            )
        })
}

/// PRD-090: the four states the front door (bare `familiar-ai`, no
/// arguments) can report, and the single runnable command that answers
/// each one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontDoorState {
    NothingToDo,
    WorkEligible { command: String },
    DecisionPending { command: String },
    SessionStopped { command: String },
}

impl FrontDoorState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NothingToDo => "nothing to do",
            Self::WorkEligible { .. } => "work eligible",
            Self::DecisionPending { .. } => "decision pending",
            Self::SessionStopped { .. } => "session stopped",
        }
    }

    pub fn next_command(&self) -> &str {
        match self {
            Self::NothingToDo => "familiar-ai next",
            Self::WorkEligible { command }
            | Self::DecisionPending { command }
            | Self::SessionStopped { command } => command,
        }
    }
}

/// Pure priority resolution over the three independently-computed signals a
/// repository can carry at once. A pending scope decision always wins --
/// PRD-083's approve command stays reachable by construction, never
/// shadowed by ordinary eligible work. Next, a driver session that stopped
/// for a reason needing a human look. Only then ordinary eligible work, and
/// finally nothing.
pub fn resolve_front_door_state(
    pending_decision_command: Option<String>,
    stopped_session_command: Option<String>,
    eligible_work_command: Option<String>,
) -> FrontDoorState {
    if let Some(command) = pending_decision_command {
        return FrontDoorState::DecisionPending { command };
    }
    if let Some(command) = stopped_session_command {
        return FrontDoorState::SessionStopped { command };
    }
    if let Some(command) = eligible_work_command {
        return FrontDoorState::WorkEligible { command };
    }
    FrontDoorState::NothingToDo
}

/// Driver session termination reasons meaning the unattended run did not
/// reach a deliberate, finite stop and needs a human to look at
/// `familiar-ai report` -- the same crash-like set
/// `DriveTermination::worker_should_restart` restarts a supervised worker
/// for.
pub fn termination_needs_attention(reason: &str) -> bool {
    matches!(
        reason,
        "storage_failure"
            | "interrupted"
            | "unclassified_result"
            | "worker_heartbeat_lost"
            | "preflight_failed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_pending_outranks_every_other_state() {
        let state = resolve_front_door_state(
            Some("familiar-ai approve --finding-hash h --candidate-hash c --approve --actor human:<identity> --reason \"<why>\"".into()),
            Some("familiar-ai report s-1".into()),
            Some("familiar-ai run docs/prds/PRD-001.md".into()),
        );
        assert_eq!(state.label(), "decision pending");
        assert!(state.next_command().starts_with("familiar-ai approve"));
    }

    #[test]
    fn session_stopped_outranks_eligible_work() {
        let state = resolve_front_door_state(
            None,
            Some("familiar-ai report s-1".into()),
            Some("familiar-ai run docs/prds/PRD-001.md".into()),
        );
        assert_eq!(state.label(), "session stopped");
        assert_eq!(state.next_command(), "familiar-ai report s-1");
    }

    #[test]
    fn work_eligible_when_nothing_else_is_pending() {
        let state = resolve_front_door_state(
            None,
            None,
            Some("familiar-ai run docs/prds/PRD-001.md".into()),
        );
        assert_eq!(state.label(), "work eligible");
        assert_eq!(state.next_command(), "familiar-ai run docs/prds/PRD-001.md");
    }

    #[test]
    fn nothing_to_do_falls_back_to_next() {
        let state = resolve_front_door_state(None, None, None);
        assert_eq!(state.label(), "nothing to do");
        assert_eq!(state.next_command(), "familiar-ai next");
    }

    #[test]
    fn relocation_notice_names_the_replacement() {
        assert_eq!(
            relocation_notice("billing").as_deref(),
            Some("note: 'familiar-ai billing' is now 'familiar-ai accounting billing' (this alias will keep working)")
        );
        assert_eq!(relocation_notice("next"), None);
    }

    #[test]
    fn declared_top_level_surface_stays_under_its_ceiling() {
        assert!(TOP_LEVEL_COMMANDS.len() <= TOP_LEVEL_COMMAND_CEILING);
    }
}
