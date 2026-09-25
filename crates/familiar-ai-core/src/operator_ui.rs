//! Versioned contract shared by every human-facing Familiar desktop.
//!
//! This module deliberately contains no GUI or daemon implementation.  GTK,
//! Tauri, tests, and the daemon transport all consume the same inventory so a
//! platform cannot silently lose an operator capability.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPERATOR_PROTOCOL_VERSION: u32 = 1;
pub const MAX_OPERATOR_PAGE: usize = 2_000;
pub const MAX_OPERATOR_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum OperatorQuery {
    InferenceStatus,
    TestConnection { target: String },
    Repositories,
    Gates { repo: String },
    Backlog { repo: String, limit: usize },
    Sessions { repo: String, limit: usize },
    Attempts { repo: String, session_id: String },
    Budget { repo: String, session_id: String },
    Review { repo: String, session_id: String },
    PrdText { repo: String, prd_path: String },
    Executions { repo: String, limit: usize },
    ProjectState { repo: String },
    DiscoverModels,
    ConfigChoices,
    Rounds { repo: String, limit: usize },
    Checkpoints { repo: String },
    BlockedReasons { repo: String },
    Dependencies { repo: String },
    InferenceSettings,
    ConfigDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorQueryKind {
    InferenceStatus,
    TestConnection,
    Repositories,
    Gates,
    Backlog,
    Sessions,
    Attempts,
    Budget,
    Review,
    PrdText,
    Executions,
    ProjectState,
    DiscoverModels,
    ConfigChoices,
    Rounds,
    Checkpoints,
    BlockedReasons,
    Dependencies,
    InferenceSettings,
    ConfigDocument,
}

impl OperatorQuery {
    pub const NAMES: [&'static str; 20] = [
        "inference_status",
        "test_connection",
        "repositories",
        "gates",
        "backlog",
        "sessions",
        "attempts",
        "budget",
        "review",
        "prd_text",
        "executions",
        "project_state",
        "discover_models",
        "config_choices",
        "rounds",
        "checkpoints",
        "blocked_reasons",
        "dependencies",
        "inference_settings",
        "config_document",
    ];

    pub fn is_slow(&self) -> bool {
        matches!(self, Self::TestConnection { .. } | Self::DiscoverModels)
    }

    pub fn kind(&self) -> OperatorQueryKind {
        match self {
            Self::InferenceStatus => OperatorQueryKind::InferenceStatus,
            Self::TestConnection { .. } => OperatorQueryKind::TestConnection,
            Self::Repositories => OperatorQueryKind::Repositories,
            Self::Gates { .. } => OperatorQueryKind::Gates,
            Self::Backlog { .. } => OperatorQueryKind::Backlog,
            Self::Sessions { .. } => OperatorQueryKind::Sessions,
            Self::Attempts { .. } => OperatorQueryKind::Attempts,
            Self::Budget { .. } => OperatorQueryKind::Budget,
            Self::Review { .. } => OperatorQueryKind::Review,
            Self::PrdText { .. } => OperatorQueryKind::PrdText,
            Self::Executions { .. } => OperatorQueryKind::Executions,
            Self::ProjectState { .. } => OperatorQueryKind::ProjectState,
            Self::DiscoverModels => OperatorQueryKind::DiscoverModels,
            Self::ConfigChoices => OperatorQueryKind::ConfigChoices,
            Self::Rounds { .. } => OperatorQueryKind::Rounds,
            Self::Checkpoints { .. } => OperatorQueryKind::Checkpoints,
            Self::BlockedReasons { .. } => OperatorQueryKind::BlockedReasons,
            Self::Dependencies { .. } => OperatorQueryKind::Dependencies,
            Self::InferenceSettings => OperatorQueryKind::InferenceSettings,
            Self::ConfigDocument => OperatorQueryKind::ConfigDocument,
        }
    }

    pub fn validate(&self) -> Result<(), OperatorError> {
        let limit = match self {
            Self::Backlog { limit, .. }
            | Self::Sessions { limit, .. }
            | Self::Executions { limit, .. }
            | Self::Rounds { limit, .. } => Some(*limit),
            _ => None,
        };
        if limit.is_some_and(|limit| limit == 0 || limit > MAX_OPERATOR_PAGE) {
            return Err(OperatorError::invalid(format!(
                "limit must be between 1 and {MAX_OPERATOR_PAGE}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEdit {
    pub path: Vec<String>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OperatorAction {
    StartPrd {
        repo: String,
        prd_path: String,
    },
    /// One drive session over exactly these PRDs: isolated worktrees, a
    /// recorded session and attempts, usage, and the scope-disjoint merge
    /// queue. Start on one card is a one-PRD wave.
    StartWave {
        repo: String,
        prd_paths: Vec<String>,
    },
    ResumePrd {
        repo: String,
        prd_id: String,
    },
    CancelExecution {
        repo: String,
        execution_id: String,
    },
    SetProjectPaused {
        repo: String,
        paused: bool,
    },
    ReleasePrd {
        repo: String,
        prd_path: String,
        actor: String,
        reason: String,
    },
    CompletePrd {
        repo: String,
        prd_path: String,
        actor: String,
        reason: String,
    },
    SaveConfig {
        edits: Vec<ConfigEdit>,
    },
    SaveInferenceConfig {
        mode: String,
        builtin_url: String,
        builtin_model: String,
    },
    StopDaemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorActionKind {
    StartPrd,
    StartWave,
    ResumePrd,
    CancelExecution,
    SetProjectPaused,
    ReleasePrd,
    CompletePrd,
    SaveConfig,
    SaveInferenceConfig,
    StopDaemon,
}

impl OperatorAction {
    pub const NAMES: [&'static str; 10] = [
        "start_prd",
        "start_wave",
        "resume_prd",
        "cancel_execution",
        "set_project_paused",
        "release_prd",
        "complete_prd",
        "save_config",
        "save_inference_config",
        "stop_daemon",
    ];

    pub fn summary(&self) -> String {
        match self {
            Self::StartPrd { prd_path, .. } => format!("Start a run of {prd_path}"),
            Self::StartWave { prd_paths, .. } => {
                format!("Start one drive session over {} PRD(s)", prd_paths.len())
            }
            Self::ResumePrd { prd_id, .. } => format!("Re-drive retained work for {prd_id}"),
            Self::CancelExecution { execution_id, .. } => format!("Stop execution {execution_id}"),
            Self::SetProjectPaused { paused: true, .. } => {
                "Pause this project — queued work stops being claimed".into()
            }
            Self::SetProjectPaused { paused: false, .. } => "Resume this project".into(),
            Self::ReleasePrd { prd_path, .. } => format!("Release {prd_path} back to pending"),
            Self::CompletePrd { prd_path, .. } => format!("Force-complete {prd_path}"),
            Self::SaveConfig { edits } => format!("Save {} configuration change(s)", edits.len()),
            Self::SaveInferenceConfig { mode, .. } => {
                format!("Set inference mode to {mode} and reload the router")
            }
            Self::StopDaemon => "Stop Familiar and all daemon-owned work".into(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::StartPrd { .. } => "start_prd",
            Self::StartWave { .. } => "start_wave",
            Self::ResumePrd { .. } => "resume_prd",
            Self::CancelExecution { .. } => "cancel_execution",
            Self::SetProjectPaused { .. } => "set_project_paused",
            Self::ReleasePrd { .. } => "release_prd",
            Self::CompletePrd { .. } => "complete_prd",
            Self::SaveConfig { .. } => "save_config",
            Self::SaveInferenceConfig { .. } => "save_inference_config",
            Self::StopDaemon => "stop_daemon",
        }
    }

    pub fn kind(&self) -> OperatorActionKind {
        match self {
            Self::StartPrd { .. } => OperatorActionKind::StartPrd,
            Self::StartWave { .. } => OperatorActionKind::StartWave,
            Self::ResumePrd { .. } => OperatorActionKind::ResumePrd,
            Self::CancelExecution { .. } => OperatorActionKind::CancelExecution,
            Self::SetProjectPaused { .. } => OperatorActionKind::SetProjectPaused,
            Self::ReleasePrd { .. } => OperatorActionKind::ReleasePrd,
            Self::CompletePrd { .. } => OperatorActionKind::CompletePrd,
            Self::SaveConfig { .. } => OperatorActionKind::SaveConfig,
            Self::SaveInferenceConfig { .. } => OperatorActionKind::SaveInferenceConfig,
            Self::StopDaemon => OperatorActionKind::StopDaemon,
        }
    }

    pub fn warning(&self) -> Option<&'static str> {
        match self {
            Self::ReleasePrd { .. } => Some("Returns the PRD to pending and DISCARDS the retained work. It cannot be undone."),
            Self::CompletePrd { .. } => Some("Marks the PRD completed without satisfying its gates. It does not merge, deliver, or verify the retained work. It cannot be undone."),
            Self::CancelExecution { .. } => Some("This terminates the running worker. Work in progress may be lost."),
            Self::StopDaemon => Some("This stops Familiar's daemon and all daemon-owned work. The desktop remains open but disconnected."),
            _ => None,
        }
    }

    pub fn needs_actor_and_reason(&self) -> bool {
        matches!(self, Self::ReleasePrd { .. } | Self::CompletePrd { .. })
    }

    pub fn validate(&self) -> Result<(), OperatorError> {
        match self {
            Self::ReleasePrd { actor, reason, .. } | Self::CompletePrd { actor, reason, .. }
                if actor.trim().is_empty() || reason.trim().is_empty() =>
            {
                Err(OperatorError::invalid("actor and reason are required"))
            }
            Self::SaveConfig { edits } if edits.is_empty() || edits.len() > 500 => Err(
                OperatorError::invalid("configuration save requires 1 to 500 edits"),
            ),
            Self::StartWave { prd_paths, .. } if prd_paths.is_empty() || prd_paths.len() > 50 => {
                Err(OperatorError::invalid("a wave names 1 to 50 PRDs"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorMutation {
    pub request_id: String,
    pub idempotency_key: String,
    pub action: OperatorAction,
}

impl OperatorMutation {
    pub fn validate(&self) -> Result<(), OperatorError> {
        if self.request_id.trim().is_empty() || self.idempotency_key.trim().is_empty() {
            return Err(OperatorError::invalid(
                "request_id and idempotency_key are required",
            ));
        }
        if self.request_id.len() > 200 || self.idempotency_key.len() > 500 {
            return Err(OperatorError::invalid(
                "operator request identifiers are too long",
            ));
        }
        self.action.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorReply {
    pub protocol_version: u32,
    pub daemon_generation: u64,
    pub revision: u64,
    pub payload: OperatorPayload,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum OperatorPayload {
    Query {
        query: OperatorQueryKind,
        data: Value,
    },
    Mutation {
        action: OperatorActionKind,
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorEvent {
    pub protocol_version: u32,
    pub daemon_generation: u64,
    pub sequence: u64,
    pub topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl OperatorError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_request".into(),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "daemon_unavailable".into(),
            message: message.into(),
            retryable: true,
        }
    }
}

pub trait OperatorDataSource: Send + Sync + 'static {
    fn query(&self, query: OperatorQuery) -> Result<Value, String>;
    fn act(&self, action: OperatorAction) -> Result<Value, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_is_stable_and_serialized_names_match() {
        assert_eq!(OperatorQuery::NAMES.len(), 20);
        assert_eq!(OperatorAction::NAMES.len(), 10);
        let value = serde_json::to_value(OperatorQuery::Backlog {
            repo: "/r".into(),
            limit: 5,
        })
        .unwrap();
        assert_eq!(value["query"], "backlog");
        let value = serde_json::to_value(OperatorAction::StopDaemon).unwrap();
        assert_eq!(value["action"], "stop_daemon");
    }

    #[test]
    fn destructive_contract_is_not_optional() {
        let release = OperatorAction::ReleasePrd {
            repo: "/r".into(),
            prd_path: "p".into(),
            actor: "".into(),
            reason: "".into(),
        };
        assert!(release.warning().unwrap().contains("DISCARDS"));
        assert!(release.needs_actor_and_reason());
        assert!(release.validate().is_err());
        assert!(OperatorAction::StopDaemon.warning().is_some());
    }

    #[test]
    fn result_bounds_fail_closed() {
        assert!(OperatorQuery::Backlog {
            repo: "/r".into(),
            limit: 0
        }
        .validate()
        .is_err());
        assert!(OperatorQuery::Backlog {
            repo: "/r".into(),
            limit: MAX_OPERATOR_PAGE + 1
        }
        .validate()
        .is_err());
    }
}
