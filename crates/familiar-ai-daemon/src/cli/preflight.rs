//! `familiar-ai preflight` — validate prerequisites without claiming a PRD
//! or invoking a model.

use familiar_ai_core::AppPaths;
use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};

use crate::run::{build_agent, resolved_agent_entries, resolved_remediation_entry, AgentSet};

use super::shared::effective_repository_config;

pub fn preflight_command() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|error| error.to_string())?;
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config)?);
    let report = crate::preflight::run(
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        &repository.worktree,
    );
    for check in &report.checks {
        let status = match check.status {
            crate::preflight::PreflightStatus::Passed => "passed",
            crate::preflight::PreflightStatus::Failed => "failed",
            crate::preflight::PreflightStatus::EnvironmentDenied => "environment_denied",
        };
        println!("{status}\t{}\t{}", check.check_id, check.detail);
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(format!("preflight failed: {}", report.failure_summary()))
    }
}
