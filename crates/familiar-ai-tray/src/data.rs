//! The seam between the tray's windows and the daemon's data.
//!
//! The tray cannot depend on `familiar-ai-daemon` — the daemon depends on the
//! tray — so the windows describe what they need and the daemon answers. The
//! answers are the same `serde_json::Value` shapes the dashboard's HTTP
//! endpoints already return, deliberately: those shapes are pinned by the
//! daemon's own tests, and a second set of typed DTOs here would be a copy of
//! a schema that is already covered.

use serde_json::Value;

/// One question a window can ask. Everything except `TestConnection` is a
/// local SQLite read or a cached-state read and is cheap enough to answer on
/// the GTK main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Router health: modes, backends, per-backend load/health state.
    InferenceStatus,
    /// Probes a backend over the network. The only slow query — callers must
    /// run it off the main thread or the window freezes for its duration.
    TestConnection {
        target: String,
    },
    /// Repositories this database holds stewardship state for.
    Repositories,
    /// PRDs stopped awaiting a human decision.
    Gates {
        repo: String,
    },
    Backlog {
        repo: String,
        limit: usize,
    },
    Sessions {
        repo: String,
        limit: usize,
    },
    Attempts {
        repo: String,
        session_id: String,
    },
    Budget {
        repo: String,
        session_id: String,
    },
    Review {
        repo: String,
        session_id: String,
    },
    /// The text of one PRD, so it can be read without leaving the window.
    PrdText {
        repo: String,
        prd_path: String,
    },
    /// Control-plane executions for this repository, newest first.
    Executions {
        repo: String,
        limit: usize,
    },
    /// `active`, `paused`, `archived`, or absent when the repository has never
    /// been registered with the control plane.
    ProjectState {
        repo: String,
    },
    /// Model identifiers fetched live from each configured provider's
    /// `/v1/models`. Slow — it goes over the network to every provider — so
    /// callers run it off the GTK thread and fill the dropdowns when it lands.
    DiscoverModels,
    /// The values a setting may take, and which of them this machine can
    /// actually use. Asked once per form so every dropdown is built from what
    /// is installed and authenticated rather than from a hard-coded list.
    ConfigChoices,
    /// The driver's sessions and the attempts inside them, for the backlog's
    /// waterfall. A "round" is a session — the unit the driver works in.
    Rounds {
        repo: String,
        limit: usize,
    },
    /// Latest recorded pipeline phase per PRD, for the backlog's progress meter.
    Checkpoints {
        repo: String,
    },
    /// Why each blocked PRD is blocked: the scope findings that stopped it.
    BlockedReasons {
        repo: String,
    },
    /// Declared dependencies per PRD, and which of them are not yet completed.
    /// A PRD with unmet dependencies cannot be run, and the form must not
    /// offer to start it.
    Dependencies {
        repo: String,
    },
    /// The effective inference settings: the file's values where it has them,
    /// the schema's defaults where it does not.
    ///
    /// Distinct from `ConfigDocument`, which reports the file verbatim. A
    /// daemon with no `[inference]` table has no inference rows in the
    /// document at all, so a form built from that document offers nothing to
    /// edit — which is exactly the state someone reaches for when they want
    /// to configure inference for the first time.
    InferenceSettings,
    /// The whole of config.toml, parsed to JSON, plus the file's own path.
    /// The form is built from the document as it actually is, so a setting
    /// nobody has hard-coded a widget for still appears.
    ConfigDocument,
}

impl Query {
    /// Whether answering this query may block on the network. The windows use
    /// it to decide what must be pushed onto a worker thread.
    pub fn is_slow(&self) -> bool {
        matches!(self, Query::TestConnection { .. } | Query::DiscoverModels)
    }
}

/// Something the operator does, as opposed to something they read.
///
/// Split from [`Query`] because these change state and several of them cannot
/// be undone: the window must confirm before running one, and [`Action::warning`]
/// is what it shows.
/// One field the operator changed, addressed by its path through the
/// document. Numeric segments index arrays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEdit {
    pub path: Vec<String>,
    /// The new value as typed. The daemon parses it against the type already
    /// at that path and refuses a change that would alter the type.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Queues one PRD for execution through the control plane.
    StartPrd { repo: String, prd_path: String },
    /// Re-drives a PRD's retained candidate from where it stopped.
    ResumePrd { repo: String, prd_id: String },
    /// Stops one execution, terminating its worker process group.
    CancelExecution { repo: String, execution_id: String },
    /// Pauses or resumes the whole project. The control plane has no
    /// per-execution pause, so this is deliberately project-wide.
    SetProjectPaused { repo: String, paused: bool },
    /// Returns a retained PRD to pending. Discards the retained work.
    ReleasePrd {
        repo: String,
        prd_path: String,
        actor: String,
        reason: String,
    },
    /// Force-completes a retained PRD without its gates being satisfied.
    CompletePrd {
        repo: String,
        prd_path: String,
        actor: String,
        reason: String,
    },
    /// Writes changed fields back to config.toml, preserving its comments and
    /// layout. Applied together: either every edit lands or none does.
    SaveConfig { edits: Vec<ConfigEdit> },
    /// Writes the inference settings, creating `[inference.text]` when the
    /// file has no such table, and then rebuilds the running router from them
    /// so the change takes effect without a restart.
    SaveInferenceConfig {
        mode: String,
        builtin_url: String,
        builtin_model: String,
    },
}

impl Action {
    /// What the operator is about to do, in one line.
    pub fn summary(&self) -> String {
        match self {
            Self::StartPrd { prd_path, .. } => format!("Start a run of {prd_path}"),
            Self::ResumePrd { prd_id, .. } => format!("Re-drive the retained work for {prd_id}"),
            Self::CancelExecution { execution_id, .. } => format!("Stop execution {execution_id}"),
            Self::SetProjectPaused { paused: true, .. } => {
                "Pause this project — queued work stops being claimed".into()
            }
            Self::SetProjectPaused { paused: false, .. } => "Resume this project".into(),
            Self::ReleasePrd { prd_path, .. } => format!("Release {prd_path} back to pending"),
            Self::CompletePrd { prd_path, .. } => format!("Force-complete {prd_path}"),
            Self::SaveInferenceConfig { mode, .. } => {
                format!("Set inference mode to {mode} and reload the router")
            }
            Self::SaveConfig { edits } => format!(
                "Save {} change{} to config.toml",
                edits.len(),
                if edits.len() == 1 { "" } else { "s" }
            ),
        }
    }

    /// The consequence that cannot be taken back, when there is one. The
    /// window refuses to run an action carrying a warning without a
    /// confirmation.
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            Self::ReleasePrd { .. } => Some(
                "Returns the PRD to pending and DISCARDS the work retained for it.\n\n\
                 It cannot be undone.",
            ),
            Self::CompletePrd { .. } => Some(
                "Marks the PRD completed in the backlog without its gates being satisfied.\n\n\
                 It does NOT merge, deliver or verify anything — the retained work stays \
                 exactly where it is, and the review findings that stopped it are never \
                 adjudicated. The backlog simply stops treating the PRD as outstanding.\n\n\
                 Recorded against your name and reason. It cannot be undone.",
            ),
            Self::CancelExecution { .. } => {
                Some("This terminates the running worker. Work in progress may be lost.")
            }
            // Saving config is reversible by saving again, and the daemon
            // keeps a backup of the previous file.
            Self::StartPrd { .. }
            | Self::ResumePrd { .. }
            | Self::SetProjectPaused { .. }
            | Self::SaveInferenceConfig { .. }
            | Self::SaveConfig { .. } => None,
        }
    }

    /// Whether an actor and reason are required. The backlog recovery paths
    /// refuse anonymous mutations, so the window must collect them first.
    pub fn needs_actor_and_reason(&self) -> bool {
        matches!(self, Self::ReleasePrd { .. } | Self::CompletePrd { .. })
    }
}

/// Answers [`Query`]s and performs [`Action`]s. Implemented by the daemon over
/// the same stewardship functions, inference router and control plane the
/// dashboard endpoints and CLI use, so the window and the other surfaces can
/// never disagree about what is true or what an action means.
pub trait DataSource: Send + Sync + 'static {
    fn query(&self, query: Query) -> Result<Value, String>;

    /// Performs an action. Slow: every variant either touches the control
    /// plane or writes the backlog, so callers run it off the GTK thread.
    fn act(&self, action: Action) -> Result<Value, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_actions_carry_a_warning_and_safe_ones_do_not() {
        let release = Action::ReleasePrd {
            repo: "/r".into(),
            prd_path: "docs/prds/PRD-1.md".into(),
            actor: "human:x".into(),
            reason: "why".into(),
        };
        assert!(release.warning().unwrap().contains("DISCARDS"));
        assert!(release.needs_actor_and_reason());
        assert!(release.summary().contains("PRD-1.md"));

        let start = Action::StartPrd {
            repo: "/r".into(),
            prd_path: "docs/prds/PRD-1.md".into(),
        };
        assert!(start.warning().is_none());
        assert!(!start.needs_actor_and_reason());

        // Pausing is reversible, so it must not demand a confirmation, but it
        // must say that it is project-wide rather than per-PRD.
        let pause = Action::SetProjectPaused {
            repo: "/r".into(),
            paused: true,
        };
        assert!(pause.warning().is_none());
        assert!(pause.summary().contains("project"));

        assert!(Action::CancelExecution {
            repo: "/r".into(),
            execution_id: "exec-1".into(),
        }
        .warning()
        .is_some());
    }

    #[test]
    fn only_connection_tests_are_slow() {
        assert!(Query::TestConnection {
            target: "text_primary".into()
        }
        .is_slow());
        assert!(Query::DiscoverModels.is_slow());
        assert!(!Query::InferenceStatus.is_slow());
        assert!(!Query::Repositories.is_slow());
        assert!(!Query::Gates { repo: "/r".into() }.is_slow());
    }
}
