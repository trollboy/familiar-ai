//! Compatibility names for the UI-neutral operator contract.
//!
//! New frontends import this contract from `familiar-ai-core`; GTK keeps these
//! names until it is retired after the cross-platform parity gates pass.

pub use familiar_ai_core::operator_ui::{
    ConfigEdit, OperatorAction as Action, OperatorDataSource as DataSource, OperatorQuery as Query,
};

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
    fn only_connection_tests_and_discovery_are_slow() {
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
